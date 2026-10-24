use std::collections::BTreeMap;
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
    Ok(status.code().unwrap_or(1))
}

pub fn run_shell(command: &str, env: &BTreeMap<String, String>, cwd: &Path) -> Result<i32, String> {
    if cfg!(windows) {
        run_direct("cmd", &["/C".to_string(), command.to_string()], env, cwd)
    } else {
        run_direct("sh", &["-lc".to_string(), command.to_string()], env, cwd)
    }
}

/// Make `process_group` the foreground group of the terminal on stdin.
#[cfg(unix)]
pub(crate) fn set_terminal_foreground_process_group(
    process_group: libc::pid_t,
) -> Result<(), String> {
    assign_terminal_foreground(process_group)
        .map_err(|error| format!("failed assigning terminal ownership: {error}"))
}

/// Async-signal-safe: it does not allocate, so a child can call it between
/// fork and exec.
#[cfg(unix)]
fn assign_terminal_foreground(process_group: libc::pid_t) -> std::io::Result<()> {
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
    let assigned = unsafe { libc::tcsetpgrp(libc::STDIN_FILENO, process_group) };
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
    use std::os::unix::process::CommandExt;
    use std::process::{Command, ExitStatus};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
    use std::time::Duration;

    use crate::infra::process_identity::{observe_process, process_group_members};

    /// Signals that ask a foreground command to stop. SIGKILL cannot be relayed.
    const RELAYED: [libc::c_int; 4] = [libc::SIGHUP, libc::SIGINT, libc::SIGQUIT, libc::SIGTERM];
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    // Signal dispositions are process-wide, so one foreground child is owned at a time.
    static OWNERSHIP: Mutex<()> = Mutex::new(());
    static OWNER: AtomicI32 = AtomicI32::new(0);
    static RECEIVED: AtomicU64 = AtomicU64::new(0);

    /// The child leads its own process group, as a shell job does, so a stop
    /// request reaches launcher shells and their descendants together. When
    /// OCM owns the terminal, the group takes the terminal while it runs and
    /// terminal keys reach it once, directly.
    pub(super) fn run(process: &mut Command) -> io::Result<ExitStatus> {
        let _ownership = OWNERSHIP
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        OWNER.store(unsafe { libc::getpid() }, Ordering::SeqCst);
        RECEIVED.store(0, Ordering::SeqCst);
        let relay = Relay::install()?;
        let result = spawn_and_wait(process);
        drop(relay);
        // What arrived after the command finished was meant for OCM itself.
        for signal in take_received() {
            unsafe {
                libc::raise(signal);
            }
        }
        result
    }

    fn spawn_and_wait(process: &mut Command) -> io::Result<ExitStatus> {
        let mut terminal = Terminal::observe();
        let take_terminal = terminal.owned;
        unsafe {
            process.pre_exec(move || {
                if libc::setpgid(0, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                if take_terminal {
                    super::assign_terminal_foreground(libc::getpid())?;
                }
                Ok(())
            });
        }
        let mut child = process.spawn()?;
        let group = child.id() as libc::pid_t;
        terminal.assigned = take_terminal;
        // Also set it here so a relay cannot precede the child's own setpgid;
        // the child may already have done so and exec'd.
        unsafe {
            libc::setpgid(group, group);
        }

        let mut relayed = false;
        let mut outcome = Ok(());
        loop {
            relayed |= relay_received(group);
            // The exited leader stays unreaped until relaying stops, so its
            // process group ID cannot be reused by an unrelated process.
            match has_exited(group) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => {
                    outcome = Err(error);
                    break;
                }
            }
            if let Err(error) = terminal.follow_stopped_child(group) {
                outcome = Err(error);
                break;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        let restored = terminal.restore();
        // A shell that exits on a relayed signal can leave the processes it
        // started still shutting down; they were signalled with it.
        while outcome.is_ok() && relayed {
            relay_received(group);
            match process_group_members(group as u32) {
                Ok(members) if members.is_empty() => break,
                Ok(_) => std::thread::sleep(POLL_INTERVAL),
                Err(error) => outcome = Err(io::Error::other(error)),
            }
        }
        let status = child.wait();
        outcome?;
        restored?;
        status
    }

    fn relay_received(group: libc::pid_t) -> bool {
        let mut relayed = false;
        for signal in take_received() {
            unsafe {
                libc::kill(-group, signal);
            }
            relayed = true;
        }
        relayed
    }

    fn has_exited(pid: libc::pid_t) -> io::Result<bool> {
        let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if waited == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            return Err(error);
        }
        // Darwin can report a stopped child despite WEXITED.
        Ok(unsafe { info.si_pid() } == pid
            && matches!(
                info.si_code,
                libc::CLD_EXITED | libc::CLD_KILLED | libc::CLD_DUMPED
            ))
    }

    struct Terminal {
        /// OCM's process group is the terminal's foreground group.
        owned: bool,
        /// The child's group holds the terminal on OCM's behalf.
        assigned: bool,
    }

    impl Terminal {
        fn observe() -> Self {
            Self {
                owned: in_terminal_foreground(),
                assigned: false,
            }
        }

        /// Job control stops the child's group without OCM. Stop OCM too so
        /// the shell regains the terminal, then resume the group with it.
        fn follow_stopped_child(&mut self, group: libc::pid_t) -> io::Result<()> {
            let stopped = observe_process(group as u32)
                .map_err(io::Error::other)?
                .is_some_and(|process| process.stopped);
            if !stopped || unsafe { libc::isatty(libc::STDIN_FILENO) } != 1 {
                return Ok(());
            }
            self.restore()?;
            if unsafe { libc::kill(libc::getpid(), libc::SIGSTOP) } == -1 {
                return Err(io::Error::last_os_error());
            }
            if in_terminal_foreground() {
                super::assign_terminal_foreground(group)?;
                self.assigned = true;
            }
            unsafe {
                libc::kill(-group, libc::SIGCONT);
            }
            Ok(())
        }

        fn restore(&mut self) -> io::Result<()> {
            if std::mem::take(&mut self.assigned) {
                super::assign_terminal_foreground(unsafe { libc::getpgrp() })?;
            }
            Ok(())
        }
    }

    fn in_terminal_foreground() -> bool {
        unsafe {
            libc::isatty(libc::STDIN_FILENO) == 1
                && libc::tcgetpgrp(libc::STDIN_FILENO) == libc::getpgrp()
        }
    }

    fn take_received() -> impl Iterator<Item = libc::c_int> {
        let received = RECEIVED.swap(0, Ordering::SeqCst);
        RELAYED
            .into_iter()
            .filter(move |signal| received & (1 << signal) != 0)
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
        RECEIVED.fetch_or(1 << signal, Ordering::SeqCst);
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
