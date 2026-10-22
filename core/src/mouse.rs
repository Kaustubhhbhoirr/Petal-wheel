//! Right-clicks while the wheel is open close a sub-ring. The wheel never takes focus and lets
//! clicks through, so without this a right-click would also open Explorer's context menu. A
//! low-level mouse hook swallows right-clicks, and it exists only while the wheel is open: no hook
//! runs while Flick idles.

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, MSG, PM_NOREMOVE, WH_MOUSE_LL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
};

/// Where the hook sends right-clicks. Only one wheel is ever open, so one slot is enough.
static SINK: Mutex<Option<Sender<()>>> = Mutex::new(None);

/// Swallows right-clicks and reports each press on the channel given to [`RightClicks::start`],
/// until dropped.
pub struct RightClicks {
    thread_id: u32,
    thread: Option<JoinHandle<()>>,
}

impl RightClicks {
    /// Installs the hook on its own thread. Returns `None` if Windows refuses it, in which case
    /// right-clicks simply pass through as before.
    pub fn start(clicks: Sender<()>) -> Option<Self> {
        *SINK.lock().unwrap() = Some(clicks);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("flick-right-click".into())
            .spawn(move || unsafe {
                let mut msg = MSG::default();
                let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                let module = GetModuleHandleW(None).ok();
                let hook =
                    SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), module.map(Into::into), 0);
                let ok = hook.is_ok();
                let _ = ready_tx.send(ok.then(|| GetCurrentThreadId()));
                let Ok(hook) = hook else {
                    return;
                };
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
                let _ = UnhookWindowsHookEx(hook);
            })
            .ok()?;
        match ready_rx.recv().ok().flatten() {
            Some(thread_id) => Some(Self {
                thread_id,
                thread: Some(thread),
            }),
            None => {
                let _ = thread.join();
                *SINK.lock().unwrap() = None;
                None
            }
        }
    }
}

impl Drop for RightClicks {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            let _ = thread.join();
        }
        *SINK.lock().unwrap() = None;
    }
}

/// Runs for every mouse event while installed, so it only checks the message and returns.
unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        let message = wparam.0 as u32;
        if message == WM_RBUTTONDOWN || message == WM_RBUTTONUP {
            if message == WM_RBUTTONDOWN {
                if let Ok(sink) = SINK.try_lock() {
                    if let Some(tx) = sink.as_ref() {
                        let _ = tx.send(());
                    }
                }
            }
            // Swallow both halves, so the window underneath sees no right-click at all.
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
