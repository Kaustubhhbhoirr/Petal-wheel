//! Prints the selection of every open Explorer window and the Desktop, with timings.
//!
//!     cargo run -p flick-core --example selection_probe
//!     cargo run -p flick-core --example selection_probe -- --foreground 3
//!
//! `--foreground N` waits N seconds, then reads only the window in front, like the hotkey does.

use std::time::Instant;

use flick_core::selection::{foreground_window, selected_paths};
use windows::core::{w, BOOL};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowW, GetClassNameW};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--foreground") {
        let secs: u64 = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(3);
        println!("Focus an Explorer window or the Desktop within {secs} s...");
        std::thread::sleep(std::time::Duration::from_secs(secs));
        report("foreground", foreground_window());
        return;
    }

    for window in explorer_windows() {
        report("explorer", window);
    }
    match unsafe { FindWindowW(w!("Progman"), None) } {
        Ok(desktop) => report("desktop", desktop),
        Err(e) => println!("desktop: not found ({e})"),
    }
}

fn report(label: &str, window: HWND) {
    let started = Instant::now();
    let result = selected_paths(window);
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    match result {
        Ok(selection) => println!(
            "{label} {window:?}: {:?}, {} path(s), {} skipped, {ms:.1} ms\n  {:#?}",
            selection.source,
            selection.paths.len(),
            selection.skipped,
            selection.paths
        ),
        Err(e) => println!("{label} {window:?}: error {e} ({ms:.1} ms)"),
    }
}

fn explorer_windows() -> Vec<HWND> {
    unsafe extern "system" fn visit(window: HWND, found: LPARAM) -> BOOL {
        let mut buf = [0u16; 64];
        let len = GetClassNameW(window, &mut buf) as usize;
        if String::from_utf16_lossy(&buf[..len]) == "CabinetWClass" {
            (*(found.0 as *mut Vec<HWND>)).push(window);
        }
        true.into()
    }
    let mut found = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&mut found as *mut _ as isize));
    }
    found
}
