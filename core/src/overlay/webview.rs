//! WebView2 wheel: a transparent Tauri window showing `ui/wheel.html`.
//!
//! Cold mode builds the window on every open and destroys it on close, so an idle Flick has no
//! WebView2 processes. Warm mode keeps one hidden window alive between opens, trading idle memory
//! for open latency. The page reports when its first frame is painted, which is when `show` returns.

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::time::Duration;

use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SW_SHOWNOACTIVATE,
    WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
};

use super::{Overlay, Placement, WheelContent};
use crate::wheel;

const LABEL: &str = "wheel";
/// A first open can include WebView2 starting its browser process.
const PAINT_TIMEOUT: Duration = Duration::from_secs(10);

/// Managed Tauri state that the page's `wheel_painted` call completes.
#[derive(Default)]
pub struct PaintSignal(Mutex<Option<Sender<()>>>);

/// Called by `ui/wheel.js` after the wheel's first frame after a show is on screen.
#[tauri::command]
pub fn wheel_painted(signal: tauri::State<'_, PaintSignal>) {
    if let Some(tx) = signal.0.lock().unwrap().take() {
        let _ = tx.send(());
    }
}

pub struct WebviewOverlay {
    app: AppHandle,
    keep_warm: bool,
}

impl WebviewOverlay {
    /// `app` must manage a [`PaintSignal`] and expose [`wheel_painted`].
    pub fn new(app: AppHandle, keep_warm: bool) -> Self {
        Self { app, keep_warm }
    }

    /// Warm mode only: builds the hidden window now so the first open is fast too.
    pub fn prewarm(&self) -> Result<(), String> {
        if self.keep_warm && self.window().is_none() {
            let empty = WheelContent {
                slices: Vec::new(),
                caption: String::new(),
                theme: std::sync::Arc::new(crate::theme::Theme::built_in()),
            };
            self.build(&empty)?;
        }
        Ok(())
    }

    fn window(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window(LABEL)
    }

    fn build(&self, content: &WheelContent) -> Result<WebviewWindow, String> {
        let initial = serde_json::to_string(content).map_err(|e| e.to_string())?;
        let size = wheel::window_size();
        WebviewWindowBuilder::new(&self.app, LABEL, WebviewUrl::App("wheel.html".into()))
            .title("Flick")
            .initialization_script(format!("window.__FLICK_WHEEL__ = {initial};"))
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .resizable(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .visible(false)
            .inner_size(size, size)
            .build()
            .map_err(|e| e.to_string())
    }
}

impl Overlay for WebviewOverlay {
    fn show(&self, at: Placement, content: &WheelContent) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        *self.app.state::<PaintSignal>().0.lock().unwrap() = Some(tx);

        let window = match self.window() {
            Some(window) => {
                window
                    .emit_to(LABEL, "wheel:content", content)
                    .map_err(|e| e.to_string())?;
                window
            }
            None => self.build(content)?,
        };
        let size = (wheel::window_size() * at.scale).round() as i32;
        window
            .set_size(PhysicalSize::new(size as u32, size as u32))
            .and_then(|_| {
                window.set_position(PhysicalPosition::new(at.x - size / 2, at.y - size / 2))
            })
            .map_err(|e| e.to_string())?;

        rx.recv_timeout(PAINT_TIMEOUT)
            .map_err(|_| "the wheel page did not paint in time".to_string())?;

        let hwnd = window.hwnd().map_err(|e| e.to_string())?;
        unsafe { show_without_focus(hwnd) };
        Ok(())
    }

    fn highlight(&self, index: Option<usize>) {
        let _ = self.app.emit_to(LABEL, "wheel:highlight", index);
    }

    fn hide(&self) {
        if let Some(window) = self.window() {
            if self.keep_warm {
                let _ = window.hide();
            } else {
                let _ = window.destroy();
            }
        }
    }
}

/// Same window behaviour as the native wheel: never takes focus, clicks pass through.
unsafe fn show_without_focus(hwnd: HWND) {
    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
    // No WS_EX_TOOLWINDOW: changing it after creation brings back a title bar, and
    // skip_taskbar already keeps the window off the taskbar.
    let wanted = (WS_EX_NOACTIVATE | WS_EX_TRANSPARENT).0 as isize;
    if style & wanted != wanted {
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | wanted);
    }
    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
}
