//! Keep the PC from sleeping while remote access is on: a phone can't reach a PC that is asleep.
//! Off by default; the user turns it on (`prevent_sleep` in settings, or the Remote access row).
//! Windows only. Elsewhere there is nothing to hold, and `hold` does nothing.

use std::sync::mpsc;

/// Whether the PC should be kept awake: the setting is on and the remote listener is up.
pub fn wanted(prevent_sleep: bool, remote_listening: bool) -> bool {
    prevent_sleep && remote_listening
}

/// Holds the "system required" request on a thread of its own: Windows ties it to the thread
/// that made it, and drops it when the thread ends or asks for it to stop.
pub struct Awake {
    tx: mpsc::Sender<bool>,
}

impl Awake {
    pub fn new() -> Awake {
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::spawn(move || {
            for on in rx {
                hold(on);
            }
            hold(false);
        });
        Awake { tx }
    }

    /// Start or stop keeping the PC awake.
    pub fn set(&self, on: bool) {
        let _ = self.tx.send(on);
    }
}

#[cfg(windows)]
fn hold(on: bool) {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }
    let flags = if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED } else { ES_CONTINUOUS };
    // SAFETY: a plain call with a flags value and no pointers.
    unsafe {
        SetThreadExecutionState(flags);
    }
}

#[cfg(not(windows))]
fn hold(_on: bool) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_while_remote_access_is_on_and_the_user_asked() {
        assert!(wanted(true, true));
        assert!(!wanted(true, false), "nothing to keep awake for when remote access is off");
        assert!(!wanted(false, true), "off unless the user turns it on");
        assert!(!wanted(false, false));
    }

    #[test]
    fn can_be_switched_on_and_off() {
        let a = Awake::new();
        a.set(true);
        a.set(false);
    }
}
