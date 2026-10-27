use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

/// Run a foreground command and own its lifetime until it exits.
///
/// On Unix, termination signals sent to OCM while the child runs are relayed
/// to the child, and OCM keeps waiting for it instead of exiting first.
pub fn run_direct(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<i32, String> {
    run_reporting_as(command, args, env, cwd, &format!("\"{command}\""))
}

/// Run `command` through the platform shell. `subject` names what ran in a
/// signal diagnostic, because the shell may replace itself with the command.
pub fn run_shell(
    command: &str,
    subject: &str,
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<i32, String> {
    let (shell, flag, script) = if cfg!(windows) {
        ("cmd", "/C", command.to_string())
    } else {
        (
            "sh",
            "-lc",
            crate::infra::shell::render_env_scoped_login_command(command, env),
        )
    };
    run_reporting_as(shell, &[flag.to_string(), script], env, cwd, subject)
}

fn run_reporting_as(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
    subject: &str,
) -> Result<i32, String> {
    let mut process = Command::new(command);
    process
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_clear()
        .envs(env)
        .current_dir(cwd);
    let status = run_foreground(&mut process)
        .map_err(|error| format!("failed to run \"{command}\": {error}"))?;
    if let Some(diagnostic) = signal_diagnostic(subject, &status) {
        let _ = writeln!(io::stderr().lock(), "{diagnostic}");
    }
    Ok(exit_status_code(&status))
}

/// Make `process_group` the foreground group of the terminal on stdin.
#[cfg(unix)]
pub(crate) fn set_terminal_foreground_process_group(
    process_group: libc::pid_t,
) -> Result<(), String> {
    assign_terminal_foreground(libc::STDIN_FILENO, process_group)
        .map_err(|error| format!("failed assigning terminal ownership: {error}"))
}

/// Async-signal-safe: it does not allocate, so a child can call it between
/// fork and exec.
#[cfg(unix)]
fn assign_terminal_foreground(
    terminal: libc::c_int,
    process_group: libc::pid_t,
) -> std::io::Result<()> {
    // A background process group may change the foreground group only while
    // SIGTTOU is blocked.
    let mut previous = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    let mut blocked = unsafe { std::mem::zeroed::<libc::sigset_t>() };
    unsafe {
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
    }
    let masked = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, &mut previous) };
    if masked != 0 {
        return Err(std::io::Error::from_raw_os_error(masked));
    }
    let assigned = unsafe { libc::tcsetpgrp(terminal, process_group) };
    let error = (assigned == -1).then(std::io::Error::last_os_error);
    let restored =
        unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut()) };
    if restored != 0 {
        return Err(std::io::Error::from_raw_os_error(restored));
    }
    error.map_or(Ok(()), Err)
}

#[cfg(not(unix))]
fn run_foreground(process: &mut Command) -> std::io::Result<ExitStatus> {
    process.status()
}

#[cfg(unix)]
fn run_foreground(process: &mut Command) -> std::io::Result<ExitStatus> {
    foreground::run(process)
}

#[cfg(unix)]
mod foreground {
    use std::io;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, ExitStatus};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::time::Duration;

    use crate::infra::process_identity::{
        observe_process, process_group_has_live_members, process_group_has_live_members_except,
    };

    /// Signals that ask a foreground command to stop. SIGKILL cannot be relayed.
    const RELAYED: [libc::c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];
    /// The longest a received signal waits before OCM passes it on. The
    /// command's exit wakes OCM immediately.
    const RELAY_INTERVAL: Duration = Duration::from_millis(100);

    // Signal dispositions are process-wide, so one foreground child is owned at a time.
    static OWNERSHIP: Mutex<()> = Mutex::new(());
    static OWNER: AtomicI32 = AtomicI32::new(0);
    static RECEIVED: AtomicU64 = AtomicU64::new(0);

    /// The child normally leads its own process group, as a shell job does,
    /// so a stop request reaches launcher shells and their descendants
    /// together. When OCM owns the terminal, that group takes the terminal
    /// while it runs and terminal keys reach it once, directly. When OCM shares
    /// its terminal job with other processes, such as a pager reading its
    /// output, the child stays in that job so they all keep the terminal.
    pub(super) fn run(process: &mut Command) -> io::Result<ExitStatus> {
        let _ownership = OWNERSHIP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        OWNER.store(unsafe { libc::getpid() }, Ordering::SeqCst);
        RECEIVED.store(0, Ordering::SeqCst);
        let relay = Relay::install()?;
        let result = spawn_and_wait(process);
        drop(relay);
        // When a stop request ended the command, OCM ends the same way, as a
        // shell job would; its caller then sees the interruption, not an exit
        // code. Signals that arrived with no command to receive them were
        // meant for OCM itself.
        let stop_request = result.as_ref().ok().and_then(|(_, signal)| *signal);
        for signal in stop_request.into_iter().chain(take_received()) {
            unsafe {
                libc::raise(signal);
            }
        }
        result.map(|(status, _)| status)
    }

    fn spawn_and_wait(process: &mut Command) -> io::Result<(ExitStatus, Option<libc::c_int>)> {
        let mut terminal = Terminal::observe();
        let isolate = !terminal.owned || alone_in_process_group();
        if isolate {
            let take_terminal = terminal.owned.then(|| terminal.fd()).flatten();
            unsafe {
                process.pre_exec(move || {
                    if libc::setpgid(0, 0) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    if let Some(tty) = take_terminal {
                        super::assign_terminal_foreground(tty, libc::getpid())?;
                    }
                    Ok(())
                });
            }
            // The child can claim the terminal and still fail to exec, so
            // reclaim it on every path.
            terminal.assigned = take_terminal.is_some();
        }
        let mut child = match process.spawn() {
            Ok(child) => child,
            Err(error) => {
                terminal.restore();
                return Err(error);
            }
        };
        let pid = child.id() as libc::pid_t;
        let target = if isolate {
            Target::Group(pid)
        } else {
            Target::SharedJob(pid)
        };

        let mut relayed = 0_u64;
        let exited = watch_exit(pid).ok();
        loop {
            relayed |= relay_received(target);
            let done = match &exited {
                Some(exited) => match exited.recv_timeout(RELAY_INTERVAL) {
                    // A failed observation also ends here; reaping below
                    // reports the same failure instead of hanging.
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
                    Err(RecvTimeoutError::Timeout) => false,
                },
                // No observer thread: poll instead.
                None => {
                    std::thread::sleep(RELAY_INTERVAL);
                    has_exited(pid)
                }
            };
            if done {
                break;
            }
            if isolate {
                terminal.follow_stopped_child(pid);
            }
        }
        let status = child.wait();
        let death = status.as_ref().ok().and_then(ExitStatusExt::signal);
        // Only a command group holding the terminal receives its keys without
        // OCM; in a shared job OCM saw them itself and relayed them. Use the
        // current assignment: a background job can take the terminal after fg.
        let terminal_stop = isolate
            && terminal.assigned
            && matches!(death, Some(libc::SIGINT | libc::SIGQUIT | libc::SIGHUP));
        terminal.restore();
        // A shell leader can exit before the processes it started finish
        // handling the same stop request. The group ID stays reserved while
        // any member remains. Waiting is best effort: an unreadable process
        // list ends it.
        if isolate && (relayed != 0 || terminal_stop) {
            while process_group_has_live_members(pid as u32).unwrap_or(false) {
                relayed |= relay_received(target);
                std::thread::sleep(RELAY_INTERVAL);
            }
        }
        let stop_request = death.filter(|signal| {
            matches!(*signal, libc::SIGHUP | libc::SIGINT | libc::SIGTERM)
                && (relayed & signal_bit(*signal) != 0 || terminal_stop)
        });
        Ok((status?, stop_request))
    }

    #[derive(Clone, Copy)]
    enum Target {
        /// The command leads its own process group.
        Group(libc::pid_t),
        /// The command shares OCM's foreground terminal job.
        SharedJob(libc::pid_t),
    }

    /// Whether OCM is the only process in its process group. Any doubt keeps
    /// the command in OCM's job, the behavior OCM had before owning commands.
    pub(super) fn alone_in_process_group() -> bool {
        let group = unsafe { libc::getpgrp() } as u32;
        process_group_has_live_members_except(group, std::process::id()) == Ok(false)
    }

    /// Report when the leader exits, leaving it unreaped so its process group
    /// ID cannot be reused while OCM still signals that group.
    fn watch_exit(pid: libc::pid_t) -> io::Result<mpsc::Receiver<()>> {
        let (sender, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("ocm-foreground-wait".to_string())
            .spawn(move || {
                while !wait_for_exit(pid, 0) {
                    // Darwin can report a stopped child despite WEXITED.
                    std::thread::sleep(Duration::from_millis(10));
                }
                let _ = sender.send(());
            })?;
        Ok(receiver)
    }

    fn has_exited(pid: libc::pid_t) -> bool {
        wait_for_exit(pid, libc::WNOHANG)
    }

    /// Whether the leader has exited, without reaping it. An observation
    /// failure also reports true, so the caller reaps and reports it.
    fn wait_for_exit(pid: libc::pid_t, options: libc::c_int) -> bool {
        loop {
            let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
            let waited = unsafe {
                libc::waitid(
                    libc::P_PID,
                    pid as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | options,
                )
            };
            if waited == -1 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return true;
            }
            return unsafe { info.si_pid() } == pid
                && matches!(
                    info.si_code,
                    libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
                );
        }
    }

    fn relay_received(target: Target) -> u64 {
        let mut relayed = 0;
        for signal in take_received() {
            match target {
                Target::Group(group) => unsafe {
                    libc::kill(-group, signal);
                },
                // OCM cannot tell a terminal key from a signal sent to it
                // alone, so the command may also see a terminal key twice.
                Target::SharedJob(pid) => unsafe {
                    libc::kill(pid, signal);
                },
            }
            relayed |= signal_bit(signal);
        }
        if relayed != 0 {
            // A stopped command could not act on the request.
            let target = match target {
                Target::Group(group) => -group,
                Target::SharedJob(pid) => pid,
            };
            unsafe {
                libc::kill(target, libc::SIGCONT);
            }
        }
        relayed
    }

    struct Terminal {
        /// OCM's controlling terminal, found independently of redirected
        /// standard streams.
        tty: Option<std::fs::File>,
        /// OCM's process group was the terminal's foreground group.
        owned: bool,
        /// The child's group holds the terminal on OCM's behalf.
        assigned: bool,
    }

    impl Terminal {
        fn observe() -> Self {
            use std::os::unix::fs::OpenOptionsExt;

            let tty = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOCTTY)
                .open("/dev/tty")
                .ok();
            let mut terminal = Self {
                tty,
                owned: false,
                assigned: false,
            };
            terminal.owned = terminal.in_foreground();
            terminal
        }

        fn fd(&self) -> Option<libc::c_int> {
            use std::os::fd::AsRawFd;

            self.tty.as_ref().map(AsRawFd::as_raw_fd)
        }

        fn in_foreground(&self) -> bool {
            self.fd()
                .is_some_and(|tty| unsafe { libc::tcgetpgrp(tty) == libc::getpgrp() })
        }

        /// Job control stops the child's group without OCM. Stop OCM too so
        /// the shell regains the terminal, then resume the group with it.
        fn follow_stopped_child(&mut self, group: libc::pid_t) {
            let Some(tty) = self.fd() else {
                return;
            };
            if !observe_process(group as u32)
                .is_ok_and(|process| process.is_some_and(|process| process.stopped))
            {
                return;
            }
            self.restore();
            unsafe {
                libc::kill(libc::getpid(), libc::SIGSTOP);
            }
            if self.in_foreground() {
                self.assigned = super::assign_terminal_foreground(tty, group).is_ok();
            }
            unsafe {
                libc::kill(-group, libc::SIGCONT);
            }
        }

        /// Best effort: the command's result stands even if the terminal
        /// cannot be handed back.
        fn restore(&mut self) {
            if std::mem::take(&mut self.assigned)
                && let Some(tty) = self.fd()
            {
                let _ = super::assign_terminal_foreground(tty, unsafe { libc::getpgrp() });
            }
        }
    }

    fn signal_bit(signal: libc::c_int) -> u64 {
        1 << signal
    }

    fn take_received() -> impl Iterator<Item = libc::c_int> {
        let received = RECEIVED.swap(0, Ordering::SeqCst);
        RELAYED
            .into_iter()
            .filter(move |signal| received & signal_bit(*signal) != 0)
    }

    extern "C" fn record(signal: libc::c_int) {
        if unsafe { libc::getpid() } != OWNER.load(Ordering::SeqCst) {
            // A child between fork and exec still runs OCM's handlers; give it
            // the default action the signal would have after exec.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
            return;
        }
        RECEIVED.fetch_or(signal_bit(signal), Ordering::SeqCst);
    }

    struct Relay {
        previous: Vec<(libc::c_int, libc::sigaction)>,
    }

    impl Relay {
        fn install() -> io::Result<Self> {
            // Dropping a partial installation restores what it changed.
            let mut installed = Self {
                previous: Vec::with_capacity(RELAYED.len()),
            };
            for signal in RELAYED {
                let previous = query(signal)?;
                // An ignored signal stays ignored, as the child inherits it.
                if previous.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
                action.sa_sigaction = record as *const () as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                unsafe {
                    libc::sigemptyset(&mut action.sa_mask);
                }
                set(signal, &action)?;
                installed.previous.push((signal, previous));
            }
            Ok(installed)
        }
    }

    impl Drop for Relay {
        // Restoring a disposition this process read back cannot fail.
        fn drop(&mut self) {
            for (signal, previous) in self.previous.drain(..) {
                let _ = set(signal, &previous);
            }
        }
    }

    fn query(signal: libc::c_int) -> io::Result<libc::sigaction> {
        let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
        if unsafe { libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { action.assume_init() })
    }

    fn set(signal: libc::c_int, action: &libc::sigaction) -> io::Result<()> {
        if unsafe { libc::sigaction(signal, action, std::ptr::null_mut()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Report a child's exit the way a shell does: its exit code, or 128 plus the
/// signal number when a signal terminated it.
fn exit_status_code(status: &ExitStatus) -> i32 {
    #[cfg(unix)]
    if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(status) {
        return 128 + signal;
    }
    status.code().unwrap_or(1)
}

/// Describe a signal death on stderr, as a shell does. Like shells, stay quiet
/// for SIGINT and SIGPIPE, which usually come from the terminal or the pipeline.
#[cfg(unix)]
fn signal_diagnostic(subject: &str, status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;

    let signal = status.signal()?;
    if signal == libc::SIGINT || signal == libc::SIGPIPE {
        return None;
    }
    let name = signal_name(signal)
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    let core = if status.core_dumped() {
        " (core dumped)"
    } else {
        ""
    };
    Some(format!(
        "ocm: {subject} was terminated by signal {signal}{name}{core}"
    ))
}

#[cfg(not(unix))]
fn signal_diagnostic(_subject: &str, _status: &ExitStatus) -> Option<String> {
    None
}

#[cfg(unix)]
fn signal_name(signal: i32) -> Option<&'static str> {
    Some(match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        _ => return None,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::path::Path;
    use std::process::{Command, ExitStatus, Stdio};

    use super::{exit_status_code, signal_diagnostic};

    #[test]
    fn numeric_exit_codes_are_reported_unchanged() {
        for code in [0, 3, 42, 255] {
            let status = ExitStatus::from_raw(code << 8);
            assert_eq!(exit_status_code(&status), code);
            assert_eq!(signal_diagnostic("\"openclaw\"", &status), None);
        }
    }

    #[test]
    fn signal_deaths_are_reported_as_128_plus_the_signal() {
        for signal in [
            libc::SIGTERM,
            libc::SIGKILL,
            libc::SIGINT,
            libc::SIGSEGV,
            libc::SIGABRT,
        ] {
            let status = ExitStatus::from_raw(signal);
            assert_eq!(exit_status_code(&status), 128 + signal);
        }
    }

    #[test]
    fn signal_deaths_are_described_except_terminal_and_pipeline_signals() {
        assert_eq!(
            signal_diagnostic("\"openclaw\"", &ExitStatus::from_raw(libc::SIGKILL)).as_deref(),
            Some(&*format!(
                "ocm: \"openclaw\" was terminated by signal {} (SIGKILL)",
                libc::SIGKILL
            ))
        );
        assert_eq!(
            signal_diagnostic(
                "launcher \"dev\"",
                &ExitStatus::from_raw(libc::SIGSEGV | 0x80)
            )
            .as_deref(),
            Some(&*format!(
                "ocm: launcher \"dev\" was terminated by signal {} (SIGSEGV) (core dumped)",
                libc::SIGSEGV
            ))
        );
        assert_eq!(
            signal_diagnostic("\"openclaw\"", &ExitStatus::from_raw(libc::SIGINT)),
            None
        );
        assert_eq!(
            signal_diagnostic("\"openclaw\"", &ExitStatus::from_raw(libc::SIGPIPE)),
            None
        );
    }

    #[test]
    fn failed_launch_returns_the_terminal_to_ocm() {
        const CHILD: &str = "OCM_TEST_FOREGROUND_TERMINAL_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // The check needs its own session and controlling terminal.
            let (mut master, mut slave) = (-1, -1);
            let opened = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut::<libc::c_char>(),
                    std::ptr::null_mut::<libc::termios>(),
                    std::ptr::null_mut::<libc::winsize>(),
                )
            };
            assert_eq!(opened, 0, "{}", std::io::Error::last_os_error());
            let mut master = unsafe { std::fs::File::from_raw_fd(master) };
            let slave = unsafe { OwnedFd::from_raw_fd(slave) };
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "infra::process::tests::failed_launch_returns_the_terminal_to_ocm",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .stdin(Stdio::from(slave.try_clone().unwrap()))
                .stdout(Stdio::from(slave.try_clone().unwrap()))
                .stderr(Stdio::from(slave));
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1
                        || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut child = command.spawn().unwrap();
            drop(command);
            let reader = std::thread::spawn(move || {
                let mut output = Vec::new();
                let mut buffer = [0_u8; 4096];
                while let Ok(count) = master.read(&mut buffer) {
                    if count == 0 {
                        break;
                    }
                    output.extend_from_slice(&buffer[..count]);
                }
                String::from_utf8_lossy(&output).into_owned()
            });
            let status = child.wait().unwrap();
            let output = reader.join().unwrap();
            assert!(status.success(), "{output}");
            return;
        }
        // As a session leader alone in its group, OCM hands the terminal to
        // the command it starts.
        assert!(super::foreground::alone_in_process_group());
        let result = super::run_direct(
            "definitely-not-a-real-command",
            &[],
            &BTreeMap::new(),
            Path::new("/"),
        );
        assert!(result.is_err(), "{result:?}");
        assert_eq!(
            unsafe { libc::tcgetpgrp(libc::STDIN_FILENO) },
            unsafe { libc::getpgrp() },
            "the terminal stayed with the command that failed to launch"
        );
    }
}
