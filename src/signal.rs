use crate::error::SysError;
use crate::shim::{self, SigAction, SigMask};
use rustix::process::{self, Signal};
use std::time::Duration;

/// Signals groupped into event categories.
#[derive(Debug, PartialEq)]
pub enum SignalEvent {
    Interrupt(Signal),
    Quit(Signal),
    Stop(Signal),
    Continue(Signal),
    Child(Signal),
    Resize(Signal),
    Unknown(Signal),
    Timeout,
}

/// List of all signals we're handling.
const PROCESSED_SIGNALS: [Signal; 10] = [
    // graceful termination
    Signal::TERM,
    Signal::INT,
    Signal::HUP,
    // emergency termination
    Signal::QUIT,
    // stop
    Signal::TSTP,
    Signal::TTIN,
    Signal::TTOU,
    // continue
    Signal::CONT,
    // child exited/stopped/resumed
    Signal::CHILD,
    // tty resize
    Signal::WINCH,
];

/// Categorize signals into higher-level event types.
fn to_event(sig: Signal) -> SignalEvent {
    match sig {
        Signal::TERM | Signal::INT | Signal::HUP => SignalEvent::Interrupt(sig),
        Signal::QUIT => SignalEvent::Quit(sig),
        Signal::TSTP | Signal::TTIN | Signal::TTOU => SignalEvent::Stop(sig),
        Signal::CONT => SignalEvent::Continue(sig),
        Signal::CHILD => SignalEvent::Child(sig),
        Signal::WINCH => SignalEvent::Resize(sig),
        _ => SignalEvent::Unknown(sig),
    }
}

/// Get human-readable name for signal.
pub fn display_name(sig: Signal) -> String {
    if let Some(sig_name) = Signal::from_named_raw(sig.as_raw()) {
        format!("{:?}", sig_name).replace("Signal::", "SIG")
    } else {
        format!("[{}]", sig.as_raw())
    }
}

/// Initialize signal handlers and mask in parent.
pub fn init_parent_signals() -> Result<(), SysError> {
    // Block processed signals. We normally will fetch them with sigwait().
    // Each thread has its own block mask.
    // New thread initially inherits mask of its parent.
    // Since we call this in main() before creating any threads, all our
    // threads will have this mask.
    if let Err(err) = shim::sigmask(&PROCESSED_SIGNALS, SigMask::Block) {
        return Err(SysError("sigmask()", err));
    }

    // Ensure processed signals have their default dispositions.
    for sig in PROCESSED_SIGNALS {
        if let Err(err) = shim::sigaction(sig, SigAction::Default) {
            return Err(SysError("sigaction()", err));
        }
    }

    // Ensure SIGPIPE is ignored and EPIPE is generated instead.
    if let Err(err) = shim::sigaction(Signal::PIPE, SigAction::Ignore) {
        return Err(SysError("sigaction()", err));
    }

    Ok(())
}

/// Initialize signal handlers and mask in child.
pub fn init_child_signals() -> Result<(), SysError> {
    // Default dispositions for everything.
    if let Err(err) = shim::sigaction(Signal::PIPE, SigAction::Default) {
        return Err(SysError("sigaction()", err));
    }
    for sig in PROCESSED_SIGNALS {
        if let Err(err) = shim::sigaction(sig, SigAction::Default) {
            return Err(SysError("sigaction()", err));
        }
    }

    // Unblock what we've blocked in parent.
    if let Err(err) = shim::sigmask(&PROCESSED_SIGNALS, SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }

    Ok(())
}

/// Unblock all signals.
pub fn unblock_all_signals() -> Result<(), SysError> {
    // Default dispositions for everything.
    for sig in PROCESSED_SIGNALS {
        if let Err(err) = shim::sigaction(sig, SigAction::Default) {
            return Err(SysError("sigaction()", err));
        }
    }

    // Unblock what we've blocked in parent.
    if let Err(err) = shim::sigmask(&PROCESSED_SIGNALS, SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }

    Ok(())
}

/// Wait next signal.
pub fn wait_signal(timeout: Option<Duration>) -> Result<SignalEvent, SysError> {
    loop {
        // Wait for any of the processed signals to be trigerred.
        let maybe_sig = shim::sigwait(&PROCESSED_SIGNALS, timeout)
            .map_err(|err| SysError("sigwait()", err))?;

        if let Some(sig) = maybe_sig {
            let event = to_event(sig);
            if let SignalEvent::Unknown(_) = event {
                continue;
            }
            return Ok(event);
        }

        return Ok(SignalEvent::Timeout);
    }
}

/// Drop pending signal.
pub fn drop_signal(sig: Signal) -> Result<(), SysError> {
    if let Err(err) = shim::sigwait(&[sig], Some(Duration::from_millis(0))) {
        return Err(SysError("sigwait()", err));
    }

    Ok(())
}

/// Unblock and deliver signal to current process.
pub fn deliver_signal(sig: Signal) -> Result<(), SysError> {
    // Unblock signal.
    if let Err(err) = shim::sigmask(&[sig], SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }

    // Send signal to current process to trigger its default handling.
    if let Err(err) = process::kill_process(process::getpid(), sig) {
        return Err(SysError("kill()", err));
    }

    // Block signal again.
    // This can happen only if this was not a termination signal.
    if let Err(err) = shim::sigmask(&[sig], SigMask::Block) {
        return Err(SysError("sigmask()", err));
    }

    Ok(())
}
