//! Keep application signal ownership while bounded private commands settle.
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

type Handler = Arc<dyn Fn() -> bool + Send + Sync>;

struct State {
    handler: Option<Handler>,
    generation: u64,
    dispatching: bool,
    // ARRIVED as the latest dispatch began; every signal counted up to it
    // has been handed to the owner once that dispatch finishes.
    arrivals_dispatched: usize,
    captures: usize,
    exit_requested: bool,
    terminating: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    handler: None,
    generation: 0,
    dispatching: false,
    arrivals_dispatched: 0,
    captures: 0,
    exit_requested: false,
    terminating: false,
});
// ctrlc's OS handler only wakes its own thread, which dispatches later. The
// OS handler counts each signal here first, so an observer can tell that a
// signal reached OCM before its dispatch has started.
static ARRIVED: AtomicUsize = AtomicUsize::new(0);
// Each signal ctrlc dispatches, with the ctrlc OS handler its wrapper forwards to.
#[cfg(unix)]
static FORWARD_HANDLERS: [(libc::c_int, AtomicUsize); 3] = [
    (libc::SIGINT, AtomicUsize::new(0)),
    (libc::SIGTERM, AtomicUsize::new(0)),
    (libc::SIGHUP, AtomicUsize::new(0)),
];
static DISPATCHED: Condvar = Condvar::new();
static INSTALLED: OnceLock<Result<Installation, String>> = OnceLock::new();

struct Installation {
    #[cfg(unix)]
    private_hup_handler: Option<libc::sigaction>,
}

// Both callers hold STATE, serializing first installation with owner adoption.
fn ensure_installed(application_owner: bool) -> Result<(), String> {
    let installation = INSTALLED
        .get_or_init(|| install_dispatcher(application_owner))
        .as_ref()
        .map_err(Clone::clone)?;
    #[cfg(unix)]
    if application_owner {
        if let Some(action) = &installation.private_hup_handler {
            set_signal_action(libc::SIGHUP, action)?;
        }
    }
    #[cfg(not(unix))]
    let _ = installation;
    Ok(())
}

fn install_dispatcher(application_owner: bool) -> Result<Installation, String> {
    #[cfg(unix)]
    let ignored_hup = if application_owner {
        None
    } else {
        let action = signal_action(libc::SIGHUP)?;
        (action.sa_sigaction == libc::SIG_IGN).then_some(action)
    };
    #[cfg(not(unix))]
    let _ = application_owner;

    ctrlc::set_handler(dispatch).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    count_arrivals()?;
    #[cfg(unix)]
    let private_hup_handler = if let Some(inherited) = ignored_hup {
        // Detached workers deliberately ignore HUP across exec. A private
        // lookup must preserve that policy; a later application owner retains
        // the ordinary ctrlc termination semantics it had before this helper.
        let dispatcher = signal_action(libc::SIGHUP);
        let restore = set_signal_action(libc::SIGHUP, &inherited);
        match (dispatcher, restore) {
            (Ok(action), Ok(())) => Some(action),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => return Err(error),
            (Err(query), Err(restore)) => return Err(format!("{query}; {restore}")),
        }
    } else {
        None
    };
    Ok(Installation {
        #[cfg(unix)]
        private_hup_handler,
    })
}

#[cfg(unix)]
fn signal_action(signal: libc::c_int) -> Result<libc::sigaction, String> {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: sigaction initializes the provided action on success; a null
    // replacement queries the current disposition without changing it.
    if unsafe { libc::sigaction(signal, std::ptr::null(), action.as_mut_ptr()) } == -1 {
        return Err(format!(
            "failed reading the handler for signal {signal}: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the successful query above initialized the complete action.
    Ok(unsafe { action.assume_init() })
}

#[cfg(unix)]
fn set_signal_action(signal: libc::c_int, action: &libc::sigaction) -> Result<(), String> {
    // SAFETY: action came from a successful query in this process and remains
    // valid for the duration of this call. No previous-action output is needed.
    if unsafe { libc::sigaction(signal, action, std::ptr::null_mut()) } == -1 {
        return Err(format!(
            "failed setting the handler for signal {signal}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

// Wrap ctrlc's OS handler for each signal it dispatches, keeping its flags and
// mask, so every arrival is counted before ctrlc wakes its thread.
#[cfg(unix)]
fn count_arrivals() -> Result<(), String> {
    for (signal, forward) in &FORWARD_HANDLERS {
        let signal = *signal;
        let mut action = signal_action(signal)?;
        if matches!(action.sa_sigaction, libc::SIG_DFL | libc::SIG_IGN)
            || action.sa_flags & libc::SA_SIGINFO != 0
        {
            return Err(format!(
                "signal {signal} has no plain dispatcher handler to forward to"
            ));
        }
        forward.store(action.sa_sigaction, Ordering::SeqCst);
        action.sa_sigaction = count_arrival as extern "C" fn(libc::c_int) as libc::sighandler_t;
        set_signal_action(signal, &action)?;
    }
    Ok(())
}

#[cfg(unix)]
extern "C" fn count_arrival(signal: libc::c_int) {
    // Only async-signal-safe work: atomics and the forwarded handler.
    ARRIVED.fetch_add(1, Ordering::SeqCst);
    let forward = FORWARD_HANDLERS
        .iter()
        .find(|(dispatched, _)| *dispatched == signal)
        .map_or(0, |(_, forward)| forward.load(Ordering::SeqCst));
    if forward != 0 {
        // SAFETY: count_arrivals stored this signal's plain (non-SA_SIGINFO)
        // handler before installing this wrapper in its place.
        let forward = unsafe {
            std::mem::transmute::<libc::sighandler_t, extern "C" fn(libc::c_int)>(forward)
        };
        forward(signal);
    }
}

/// Register the application's single signal owner. Returning false requests the
/// ordinary exit, deferred only while a private command still owns its children.
pub(crate) fn set_handler(
    handler: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<(), String> {
    let mut state = STATE.lock().unwrap();
    if state.handler.is_some() {
        return Err("an application signal handler is already registered".to_string());
    }
    state.handler = Some(Arc::new(handler));
    if let Err(error) = ensure_installed(true) {
        state.handler = None;
        return Err(error);
    }
    Ok(())
}

/// Register the owner like `set_handler`, keeping each of SIGINT, SIGTERM and
/// SIGHUP that the process inherited as ignored, as `nohup` leaves SIGHUP. The
/// owner then sees only the signals the caller allowed to stop OCM. Call it
/// before OCM starts other threads: the signals stay pending on this thread
/// while the owner is installed, and restoring an ignore discards a pending
/// signal instead of handing it to the owner.
pub(crate) fn set_handler_keeping_ignored(
    handler: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let unblocked = block_dispatched_signals()?;
        let installed = install_keeping_ignored(handler);
        let restored = set_signal_mask(&unblocked);
        installed.and(restored)
    }
    #[cfg(not(unix))]
    set_handler(handler)
}

#[cfg(unix)]
fn install_keeping_ignored(
    handler: impl Fn() -> bool + Send + Sync + 'static,
) -> Result<(), String> {
    let mut ignored = Vec::new();
    for (signal, _) in &FORWARD_HANDLERS {
        let action = signal_action(*signal)?;
        if action.sa_sigaction == libc::SIG_IGN {
            ignored.push((*signal, action));
        }
    }
    set_handler(handler)?;
    for (signal, action) in &ignored {
        set_signal_action(*signal, action)?;
    }
    Ok(())
}

/// Blocks the dispatched signals on this thread and returns the previous mask.
#[cfg(unix)]
fn block_dispatched_signals() -> Result<libc::sigset_t, String> {
    let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
    // SAFETY: sigemptyset initializes the set before sigaddset and
    // pthread_sigmask read it; pthread_sigmask initializes previous on success.
    unsafe {
        libc::sigemptyset(blocked.as_mut_ptr());
        for (signal, _) in &FORWARD_HANDLERS {
            libc::sigaddset(blocked.as_mut_ptr(), *signal);
        }
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, blocked.as_ptr(), previous.as_mut_ptr());
        if error != 0 {
            return Err(format!(
                "failed blocking signals: {}",
                std::io::Error::from_raw_os_error(error)
            ));
        }
        Ok(previous.assume_init())
    }
}

#[cfg(unix)]
fn set_signal_mask(mask: &libc::sigset_t) -> Result<(), String> {
    // SAFETY: mask came from a successful pthread_sigmask query.
    let error = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, mask, std::ptr::null_mut()) };
    if error != 0 {
        return Err(format!(
            "failed restoring the signal mask: {}",
            std::io::Error::from_raw_os_error(error)
        ));
    }
    Ok(())
}

fn dispatch() {
    let handler = {
        let mut state = STATE.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.dispatching = true;
        state.arrivals_dispatched = ARRIVED.load(Ordering::SeqCst);
        state.handler.clone()
    };
    // Owners only record cancellation or arbitrate an upgrade's rollback fence.
    // Never invoke them under the custody lock.
    let handled = handler.is_some_and(|handler| handler());
    let exit = {
        let mut state = STATE.lock().unwrap();
        state.dispatching = false;
        state.exit_requested |= !handled;
        let exit = state.exit_requested && state.captures == 0;
        state.terminating |= exit;
        DISPATCHED.notify_all();
        exit
    };
    if exit {
        std::process::exit(130);
    }
}

fn settled_dispatch() -> MutexGuard<'static, State> {
    let mut state = STATE.lock().unwrap();
    // A cancellation observer must not unwind an upgrade fence before its
    // signal owner records the interruption. Recovery snapshots likewise wait
    // until the signal that requested recovery has finished dispatching.
    while state.dispatching {
        state = DISPATCHED.wait(state).unwrap();
    }
    state
}

/// Wait until every signal that has reached OCM has been handed to the
/// application owner, so a check run after this sees its recorded effect.
/// Arrivals are counted on Unix only; elsewhere this waits out a dispatch
/// already in progress, as `settled_dispatch` does.
pub(crate) fn settle() {
    let mut state = STATE.lock().unwrap();
    while state.dispatching || state.arrivals_dispatched != ARRIVED.load(Ordering::SeqCst) {
        state = DISPATCHED.wait(state).unwrap();
    }
}

pub(crate) fn generation() -> u64 {
    settled_dispatch().generation
}

pub(crate) fn interrupted_since(generation: u64) -> bool {
    self::generation() != generation
}

/// Declare custody before spawn and retain it until command capture, cleanup,
/// and reader destruction finish. Use the caller's operation generation so a
/// signal during preparation cannot be mistaken for an earlier recovery signal.
pub(crate) struct CaptureInterrupt {
    generation: u64,
}

impl CaptureInterrupt {
    pub(crate) fn enter(expected_generation: u64) -> Result<Self, String> {
        let mut state = settled_dispatch();
        ensure_installed(false)?;
        if state.exit_requested || state.terminating || state.generation != expected_generation {
            return Err("command interrupted before it could start".to_string());
        }
        state.captures += 1;
        Ok(Self {
            generation: expected_generation,
        })
    }

    pub(crate) fn interrupted(&self) -> bool {
        interrupted_since(self.generation)
    }
}

impl Drop for CaptureInterrupt {
    fn drop(&mut self) {
        let exit = {
            let mut state = settled_dispatch();
            state.captures -= 1;
            let exit = state.exit_requested && state.captures == 0;
            state.terminating |= exit;
            exit
        };
        if exit {
            std::process::exit(130);
        }
    }
}

static CANCELLATION_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Register the application signal owner for a command that stops its own work
/// when SIGINT, SIGTERM or SIGHUP arrives, so the cleanup its error path
/// already performs still runs. The signal only requests cancellation; the
/// command's long-running steps observe it through [`cancellation_requested`]
/// and return an error. A signal that was ignored when OCM started, as under
/// `nohup` or for a background job, stays ignored.
pub(crate) fn request_cancellation_on_signal() -> Result<(), String> {
    let install = || {
        set_handler(|| {
            CANCELLATION_REQUESTED.store(true, Ordering::SeqCst);
            true
        })
    };
    #[cfg(not(unix))]
    return install();
    #[cfg(unix)]
    {
        let ignored = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP]
            .into_iter()
            .map(|signal| signal_action(signal).map(|action| (signal, action)))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|(_, action)| action.sa_sigaction == libc::SIG_IGN)
            .collect::<Vec<_>>();
        // Block the ignored signals until their disposition is restored, so
        // one arriving meanwhile stays pending and is then discarded instead
        // of reaching the new handler. Threads started meanwhile inherit the
        // block, which is moot for signals that stay ignored.
        let mut blocked = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        let mut previous = std::mem::MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: sigemptyset initializes the set before it is read, and
        // pthread_sigmask writes the previous mask before it is restored.
        unsafe {
            libc::sigemptyset(blocked.as_mut_ptr());
            for (signal, _) in &ignored {
                libc::sigaddset(blocked.as_mut_ptr(), *signal);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, blocked.as_ptr(), previous.as_mut_ptr());
        }
        let result = install().and_then(|()| {
            ignored
                .iter()
                .try_for_each(|(signal, action)| set_signal_action(*signal, action))
        });
        // SAFETY: previous was written by the pthread_sigmask call above.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, previous.as_ptr(), std::ptr::null_mut());
        }
        result
    }
}

/// Whether a signal has asked the command to stop. Cheap enough to check on
/// every buffered write of a long copy.
pub(crate) fn cancellation_requested() -> bool {
    CANCELLATION_REQUESTED.load(Ordering::SeqCst)
}

/// [`cancellation_requested`], counting a signal that already reached OCM but
/// is still being dispatched, for the decision after the work returned.
pub(crate) fn cancellation_settled() -> bool {
    settle();
    cancellation_requested()
}

/// The error a long-running step returns once cancellation is requested.
pub(crate) fn cancelled_error() -> io::Error {
    io::Error::other("interrupted by a signal")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn owner_keeping_ignored_signals_leaves_them_ignored() {
        const CHILD: &str = "OCM_TEST_SIGNAL_KEEP_IGNORED_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Signal registration is process-wide; isolate it from other tests.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "infra::interrupt::tests::owner_keeping_ignored_signals_leaves_them_ignored",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let signals = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&signals);
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
        }
        set_handler_keeping_ignored(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .unwrap();
        assert_eq!(
            signal_action(libc::SIGHUP).unwrap().sa_sigaction,
            libc::SIG_IGN
        );
        for signal in [libc::SIGINT, libc::SIGTERM] {
            assert!(!matches!(
                signal_action(signal).unwrap().sa_sigaction,
                libc::SIG_DFL | libc::SIG_IGN
            ));
        }
        // The installation leaves this thread's mask as it found it, so the
        // owner still receives the signals it handles.
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        settle();
        assert_eq!(signals.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn private_capture_preserves_owner_adoption_and_recovery_generations() {
        const CHILD: &str = "OCM_TEST_SIGNAL_DISPATCH_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Signal registration is process-wide; isolate it from other tests.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "infra::interrupt::tests::private_capture_preserves_owner_adoption_and_recovery_generations", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
        let first = CaptureInterrupt::enter(generation()).unwrap();
        assert_eq!(
            signal_action(libc::SIGHUP).unwrap().sa_sigaction,
            libc::SIG_IGN
        );
        drop(first);

        let signals = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&signals);
        set_handler(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .unwrap();
        assert_ne!(
            signal_action(libc::SIGHUP).unwrap().sa_sigaction,
            libc::SIG_IGN
        );
        assert!(
            set_handler(|| true).is_err(),
            "application ownership must remain exclusive"
        );
        for (index, signal) in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM]
            .into_iter()
            .enumerate()
        {
            let operation_generation = generation();
            let capture = CaptureInterrupt::enter(operation_generation).unwrap();
            assert!(
                !capture.interrupted(),
                "an earlier signal cancelled recovery work"
            );
            // raise returns only after the OS handler ran, so the signal has
            // reached the process; settle must then wait out ctrlc's thread.
            assert_eq!(unsafe { libc::raise(signal) }, 0);
            let (settled, done) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                settle();
                let _ = settled.send(());
            });
            done.recv_timeout(std::time::Duration::from_secs(10))
                .expect("settle did not return after the signal arrived");
            assert_eq!(
                signals.load(Ordering::SeqCst),
                index + 1,
                "settle returned before the arrived signal was dispatched"
            );
            assert!(capture.interrupted(), "signal was not dispatched");
            drop(capture); // A handled signal must return to the owning recovery path.
            assert!(
                CaptureInterrupt::enter(operation_generation).is_err(),
                "a signal during operation preparation must prevent capture admission"
            );
        }
    }
}
