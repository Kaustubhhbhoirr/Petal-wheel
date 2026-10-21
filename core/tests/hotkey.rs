//! Hotkey registration tests. Windows only; they do not press any keys.
#![cfg(windows)]

use flick_core::hotkey::{HotkeyError, HotkeyListener, Shortcut};

#[test]
fn second_registration_of_the_same_shortcut_is_a_conflict() {
    // An unusual combo that no real app is likely to own.
    let shortcut: Shortcut = "Ctrl+Alt+Shift+F24".parse().unwrap();
    let first = HotkeyListener::start(shortcut, |_| {}).expect("first registration");

    match HotkeyListener::start(shortcut, |_| {}) {
        Err(HotkeyError::Conflict(s)) => {
            assert_eq!(s, shortcut);
            assert!(HotkeyError::Conflict(s)
                .to_string()
                .contains("already used"));
        }
        Err(other) => panic!("expected a conflict, got {other}"),
        Ok(_) => panic!("expected a conflict, got a second registration"),
    }

    // Once stopped, the shortcut is free again.
    first.stop();
    HotkeyListener::start(shortcut, |_| {}).expect("re-registration after stop");
}
