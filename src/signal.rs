use crate::error::SysError;
use crate::shim::{self, SigAction, SigMask};
use rustix::process::{self, Signal};
use std::time::Duration;

/// List of signals that generate events which we want to handle.
/// Before starting any threads, the main thread blocks all these signals,
/// and later all created threads inherit the block mask.
/// Then one of the threads fetches signals one by one using sigwait().
/// Signals are only unblocked when we want to deliver them to ourselves
/// in the end of graceful termination or pause.
const EVENT_SIGNALS: [Signal; 10] = [
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

/// Categorize signals into higher-level event types.
fn to_event(sig: Signal) -> SignalEvent {
    match sig {
        Signal::TERM | Signal::INT | Signal::HUP => SignalEvent::Interrupt(sig),
        Signal::QUIT => SignalEvent::Quit(sig),
        Signal::TSTP | Signal::TTIN | Signal::TTOU => SignalEvent::Stop(sig),
        Signal::CONT => SignalEvent::Continue(sig),
        Signal::CHILD => SignalEvent::Child(sig),
        Signal::WINCH => SignalEvent::Resize(sig),
        // all other signals has no special handling outside of this module
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
    // EVENT_SIGNALS
    if let Err(err) = shim::sigmask(&EVENT_SIGNALS, SigMask::Block) {
        return Err(SysError("sigmask()", err));
    }
    for sig in EVENT_SIGNALS {
        let action = if sig == Signal::CHILD {
            SigAction::Noop
        } else {
            SigAction::Default
        };
        if let Err(err) = shim::sigaction(sig, action) {
            return Err(SysError("sigaction()", err));
        }
    }

    // SIGALRM
    if let Err(err) = shim::sigmask(&[Signal::ALARM], SigMask::Block) {
        return Err(SysError("sigmask()", err));
    }
    if let Err(err) = shim::sigaction(Signal::ALARM, SigAction::Noop) {
        return Err(SysError("sigaction()", err));
    }

    // SIGPIPE
    if let Err(err) = shim::sigmask(&[Signal::PIPE], SigMask::Block) {
        return Err(SysError("sigmask()", err));
    }
    if let Err(err) = shim::sigaction(Signal::PIPE, SigAction::Ignore) {
        return Err(SysError("sigaction()", err));
    }

    Ok(())
}

/// Initialize signal handlers and mask in child.
/// This is executed in child after fork() and reverts the changes
/// made by init_parent_signals() and inherited by new process.
pub fn init_child_signals() -> Result<(), SysError> {
    // EVENT_SIGNALS
    if let Err(err) = shim::sigmask(&EVENT_SIGNALS, SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }
    for sig in EVENT_SIGNALS {
        if let Err(err) = shim::sigaction(sig, SigAction::Default) {
            return Err(SysError("sigaction()", err));
        }
    }

    // SIGALRM
    if let Err(err) = shim::sigmask(&[Signal::ALARM], SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }
    if let Err(err) = shim::sigaction(Signal::ALARM, SigAction::Default) {
        return Err(SysError("sigaction()", err));
    }

    // SIGPIPE
    if let Err(err) = shim::sigmask(&[Signal::PIPE], SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }
    if let Err(err) = shim::sigaction(Signal::PIPE, SigAction::Default) {
        return Err(SysError("sigaction()", err));
    }

    Ok(())
}

/// Unblock event signals that we've blocked.
pub fn unblock_signals() -> Result<(), SysError> {
    if let Err(err) = shim::sigmask(&EVENT_SIGNALS, SigMask::Unblock) {
        return Err(SysError("sigmask()", err));
    }

    Ok(())
}

/// Wait next event signal.
pub fn wait_signal(timeout: Option<Duration>) -> Result<SignalEvent, SysError> {
    loop {
        // Wait for any of the processed signals to be trigerred.
        let maybe_sig =
            shim::sigwait(&EVENT_SIGNALS, timeout).map_err(|err| SysError("sigwait()", err))?;

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

/// Drop pending event signal.
pub fn drop_signal(sig: Signal) -> Result<(), SysError> {
    if let Err(err) = shim::sigwait(&[sig], Some(Duration::ZERO)) {
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
