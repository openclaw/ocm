//! Keep application signal ownership while bounded private commands settle.
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

type Handler = Arc<dyn Fn() -> bool + Send + Sync>;

struct State {
    handler: Option<Handler>,
    generation: u64,
    dispatching: bool,
    captures: usize,
    exit_requested: bool,
    terminating: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    handler: None,
    generation: 0,
    dispatching: false,
    captures: 0,
    exit_requested: false,
    terminating: false,
});
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
            set_hup_action(action)?;
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
        let action = hup_action()?;
        (action.sa_sigaction == libc::SIG_IGN).then_some(action)
    };
    #[cfg(not(unix))]
    let _ = application_owner;

    ctrlc::set_handler(dispatch).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    let private_hup_handler = if let Some(inherited) = ignored_hup {
        // Detached workers deliberately ignore HUP across exec. A private
        // lookup must preserve that policy; a later application owner retains
        // the ordinary ctrlc termination semantics it had before this helper.
        let dispatcher = hup_action();
        let restore = set_hup_action(&inherited);
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
fn hup_action() -> Result<libc::sigaction, String> {
    let mut action = std::mem::MaybeUninit::<libc::sigaction>::uninit();
    // SAFETY: sigaction initializes the provided action on success; a null
    // replacement queries the current disposition without changing it.
    if unsafe { libc::sigaction(libc::SIGHUP, std::ptr::null(), action.as_mut_ptr()) } == -1 {
        return Err(format!(
            "failed reading SIGHUP handler: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the successful query above initialized the complete action.
    Ok(unsafe { action.assume_init() })
}

#[cfg(unix)]
fn set_hup_action(action: &libc::sigaction) -> Result<(), String> {
    // SAFETY: action came from a successful query in this process and remains
    // valid for the duration of this call. No previous-action output is needed.
    if unsafe { libc::sigaction(libc::SIGHUP, action, std::ptr::null_mut()) } == -1 {
        return Err(format!(
            "failed restoring SIGHUP handler: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
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

fn dispatch() {
    let handler = {
        let mut state = STATE.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.dispatching = true;
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

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
        assert_eq!(hup_action().unwrap().sa_sigaction, libc::SIG_IGN);
        drop(first);

        let signals = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&signals);
        set_handler(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            true
        })
        .unwrap();
        assert_ne!(hup_action().unwrap().sa_sigaction, libc::SIG_IGN);
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
            assert_eq!(unsafe { libc::raise(signal) }, 0);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !capture.interrupted() {
                assert!(Instant::now() < deadline, "signal was not dispatched");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(signals.load(Ordering::SeqCst), index + 1);
            drop(capture); // A handled signal must return to the owning recovery path.
            assert!(
                CaptureInterrupt::enter(operation_generation).is_err(),
                "a signal during operation preparation must prevent capture admission"
            );
        }
    }
}
