//! The global shortcut that opens the wheel, with press and release detection.
//!
//! The press comes from `RegisterHotKey`: event-driven, zero cost while idle, the keystroke is not
//! passed on to the focused app, and registration fails if another app already owns the combo,
//! which is how conflicts are detected. `RegisterHotKey` does not report the release, so while
//! (and only while) the shortcut is held, the listener checks the trigger key every 10 ms.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

/// A shortcut such as `Ctrl+Alt+Space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Windows virtual-key code of the trigger key.
    pub vk: u16,
}

impl Default for Shortcut {
    fn default() -> Self {
        "Ctrl+Alt+Space".parse().expect("valid default shortcut")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShortcutError {
    #[error("\"{0}\" is not a key Flick knows.")]
    UnknownKey(String),
    #[error("A shortcut needs exactly one non-modifier key.")]
    NeedsOneKey,
    #[error("A shortcut needs at least one of Ctrl, Alt, Shift or Win.")]
    NeedsModifier,
}

impl FromStr for Shortcut {
    type Err = ShortcutError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut shortcut = Shortcut {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            vk: 0,
        };
        for part in text.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => shortcut.ctrl = true,
                "alt" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                "win" | "windows" | "super" | "meta" => shortcut.win = true,
                _ => {
                    let vk = vk_from_name(part)
                        .ok_or_else(|| ShortcutError::UnknownKey(part.to_string()))?;
                    if shortcut.vk != 0 {
                        return Err(ShortcutError::NeedsOneKey);
                    }
                    shortcut.vk = vk;
                }
            }
        }
        if shortcut.vk == 0 {
            return Err(ShortcutError::NeedsOneKey);
        }
        if !(shortcut.ctrl || shortcut.alt || shortcut.shift || shortcut.win) {
            return Err(ShortcutError::NeedsModifier);
        }
        Ok(shortcut)
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
            (self.win, "Win+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        f.write_str(&vk_name(self.vk))
    }
}

const NAMED_KEYS: &[(&str, u16)] = &[
    ("Space", 0x20),
    ("Tab", 0x09),
    ("Enter", 0x0D),
    ("PageUp", 0x21),
    ("PageDown", 0x22),
    ("End", 0x23),
    ("Home", 0x24),
    ("Left", 0x25),
    ("Up", 0x26),
    ("Right", 0x27),
    ("Down", 0x28),
    ("Insert", 0x2D),
    ("Delete", 0x2E),
    ("`", 0xC0),
];

fn vk_from_name(name: &str) -> Option<u16> {
    if let Some((_, vk)) = NAMED_KEYS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
    {
        return Some(*vk);
    }
    let upper = name.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    match bytes {
        // Letters and digits share their ASCII codes with their virtual-key codes.
        [c] if c.is_ascii_uppercase() || c.is_ascii_digit() => Some(*c as u16),
        [b'F', rest @ ..] => match std::str::from_utf8(rest).ok()?.parse::<u16>() {
            Ok(n @ 1..=24) => Some(0x70 + n - 1),
            _ => None,
        },
        _ => None,
    }
}

fn vk_name(vk: u16) -> String {
    if let Some((name, _)) = NAMED_KEYS.iter().find(|(_, code)| *code == vk) {
        return (*name).to_string();
    }
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from(vk as u8).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        _ => format!("VK{vk:#04x}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Pressed,
    /// The trigger key went up. `held` is how long the shortcut was held, which tells a quick
    /// tap (click mode) from a hold (hold-to-open).
    Released {
        held: Duration,
    },
}

#[cfg(windows)]
pub use listener::{HotkeyError, HotkeyListener};

#[cfg(windows)]
mod listener {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT,
        MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_HOTKEY, WM_QUIT,
    };

    use super::{HotkeyEvent, Shortcut};

    const HOTKEY_ID: i32 = 1;
    const RELEASE_POLL: Duration = Duration::from_millis(10);
    /// Safety net: never poll for longer than this, even if a key-up is somehow missed.
    const MAX_HOLD: Duration = Duration::from_secs(120);

    #[derive(Debug, thiserror::Error)]
    pub enum HotkeyError {
        #[error("{0} is already used by another app. Pick a different shortcut in Settings.")]
        Conflict(Shortcut),
        #[error("Could not register {0} ({1}).")]
        Os(Shortcut, windows::core::Error),
    }

    /// Listens for the shortcut on its own thread until stopped or dropped.
    pub struct HotkeyListener {
        thread_id: u32,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl HotkeyListener {
        /// Registers `shortcut` and calls `on_event` from the listener thread. Keep the callback
        /// short: hand the event to another thread or event loop.
        pub fn start<F>(shortcut: Shortcut, mut on_event: F) -> Result<Self, HotkeyError>
        where
            F: FnMut(HotkeyEvent) + Send + 'static,
        {
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = stop.clone();
            let (ready_tx, ready_rx) = mpsc::sync_channel(1);

            let thread = std::thread::Builder::new()
                .name("flick-hotkey".into())
                .spawn(move || unsafe {
                    // Make sure this thread has a message queue before anyone posts to it.
                    let mut msg = MSG::default();
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);

                    let registered = RegisterHotKey(
                        None,
                        HOTKEY_ID,
                        modifiers(&shortcut) | MOD_NOREPEAT,
                        shortcut.vk as u32,
                    );
                    let failed = registered.is_err();
                    let _ = ready_tx.send(registered.map(|_| GetCurrentThreadId()));
                    if failed {
                        return;
                    }

                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        if msg.message != WM_HOTKEY || msg.wParam.0 != HOTKEY_ID as usize {
                            continue;
                        }
                        let pressed_at = Instant::now();
                        on_event(HotkeyEvent::Pressed);
                        while key_down(shortcut.vk)
                            && pressed_at.elapsed() < MAX_HOLD
                            && !thread_stop.load(Ordering::Relaxed)
                        {
                            std::thread::sleep(RELEASE_POLL);
                        }
                        on_event(HotkeyEvent::Released {
                            held: pressed_at.elapsed(),
                        });
                    }
                    let _ = UnregisterHotKey(None, HOTKEY_ID);
                })
                .expect("spawn hotkey thread");

            match ready_rx.recv() {
                Ok(Ok(thread_id)) => Ok(Self {
                    thread_id,
                    stop,
                    thread: Some(thread),
                }),
                Ok(Err(e)) => {
                    let _ = thread.join();
                    if e.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
                        Err(HotkeyError::Conflict(shortcut))
                    } else {
                        Err(HotkeyError::Os(shortcut, e))
                    }
                }
                Err(_) => panic!("hotkey thread exited before registering"),
            }
        }

        pub fn stop(mut self) {
            self.shutdown();
        }

        fn shutdown(&mut self) {
            if let Some(thread) = self.thread.take() {
                self.stop.store(true, Ordering::Relaxed);
                unsafe {
                    let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                let _ = thread.join();
            }
        }
    }

    impl Drop for HotkeyListener {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    fn modifiers(shortcut: &Shortcut) -> HOT_KEY_MODIFIERS {
        let mut m = HOT_KEY_MODIFIERS(0);
        if shortcut.ctrl {
            m |= MOD_CONTROL;
        }
        if shortcut.alt {
            m |= MOD_ALT;
        }
        if shortcut.shift {
            m |= MOD_SHIFT;
        }
        if shortcut.win {
            m |= MOD_WIN;
        }
        m
    }

    fn key_down(vk: u16) -> bool {
        // The high bit is set while the key is physically down.
        unsafe { GetAsyncKeyState(vk as i32) < 0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_the_default() {
        let s = Shortcut::default();
        assert!(s.ctrl && s.alt && !s.shift && !s.win);
        assert_eq!(s.vk, 0x20);
        assert_eq!(s.to_string(), "Ctrl+Alt+Space");
    }

    #[test]
    fn parsing_is_forgiving_about_case_and_spaces() {
        let s: Shortcut = " control + SHIFT + f12 ".parse().unwrap();
        assert_eq!(s.to_string(), "Ctrl+Shift+F12");
        let s: Shortcut = "win+alt+k".parse().unwrap();
        assert_eq!(s.to_string(), "Alt+Win+K");
        let s: Shortcut = "Ctrl+`".parse().unwrap();
        assert_eq!(s.vk, 0xC0);
    }

    #[test]
    fn rejects_bad_shortcuts() {
        assert_eq!(
            "Space".parse::<Shortcut>(),
            Err(ShortcutError::NeedsModifier)
        );
        assert_eq!(
            "Ctrl+Alt".parse::<Shortcut>(),
            Err(ShortcutError::NeedsOneKey)
        );
        assert_eq!(
            "Ctrl+A+B".parse::<Shortcut>(),
            Err(ShortcutError::NeedsOneKey)
        );
        assert_eq!(
            "Ctrl+F25".parse::<Shortcut>(),
            Err(ShortcutError::UnknownKey("F25".into()))
        );
    }
}
