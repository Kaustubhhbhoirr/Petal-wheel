//! Flick's own toast: a small native card in the bottom-right corner of the screen.
//!
//! Windows notifications can be switched off system-wide (they were on the development PC), so
//! job progress and results get their own window instead. Like the wheel it never takes focus.
//! It shows one toast at a time; a new one replaces the current one.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use windows::core::{w, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::D2D1_ROUNDED_RECT;
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{
    ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, LoadCursorW,
    PostMessageW, SetCursor, SetTimer, ShowWindow, TranslateMessage, IDC_ARROW, IDC_HAND,
    MA_NOACTIVATE, MSG, SW_HIDE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WM_APP,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_SETCURSOR, WM_TIMER,
};

use crate::controller::scale_at;
use crate::gfx::{create_layered_window, draw_text, rgba, Surface, TextStyle};
use crate::log::log;
use crate::supervisor::CancelToken;

const WM_COMMAND_READY: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const HIDE_TIMER: usize = 1;

/// Logical (96 DPI) layout.
const WIDTH: f32 = 360.0;
const HEIGHT: f32 = 96.0;
const MARGIN: f32 = 16.0;
const PAD: f32 = 16.0;

/// What to show.
#[derive(Clone)]
pub enum Toast {
    /// A job is running. Cancel sets `cancel`.
    Running {
        title: String,
        detail: String,
        cancel: CancelToken,
    },
    /// A job finished; offers to open the result or show it in its folder.
    Done {
        title: String,
        output: PathBuf,
    },
    Failed {
        title: String,
        message: String,
    },
    /// A message with one action button.
    Notice {
        title: String,
        message: String,
        button: String,
        on_click: Arc<dyn Fn() + Send + Sync>,
    },
}

/// Anything that can show toasts. The controller only knows this trait.
pub trait Toaster: Send + Sync {
    fn show(&self, toast: Toast);
    fn hide(&self);
}

enum Command {
    Show(Toast),
    Hide,
}

/// The native toast window, running on its own thread.
pub struct ToastWindow {
    commands: Mutex<Sender<Command>>,
    /// Stored as an integer because HWND is not Send.
    hwnd: isize,
}

impl ToastWindow {
    pub fn start() -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Command>();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("flick-toast".into())
            .spawn(move || {
                // "Show in folder" uses the shell, which wants COM on this thread.
                unsafe {
                    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                }
                let created = unsafe {
                    create_layered_window(w!("FlickToast"), Some(window_proc), WINDOW_EX_STYLE(0))
                }
                .and_then(|hwnd| Ok((hwnd, Surface::new()?)));
                let (hwnd, surface) = match created {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let card = Card {
                    hwnd,
                    surface,
                    fonts: None,
                    toast: None,
                    scale: 1.0,
                    origin: POINT::default(),
                    hover: None,
                    tracking: false,
                };
                STATE.with(|s| *s.borrow_mut() = Some((card, rx)));
                let _ = ready_tx.send(Ok(hwnd.0 as isize));
                unsafe {
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let hwnd = ready_rx.recv().map_err(|e| e.to_string())??;
        Ok(Self {
            commands: Mutex::new(tx),
            hwnd,
        })
    }

    fn send(&self, command: Command) {
        let _ = self.commands.lock().unwrap().send(command);
        unsafe {
            let _ = PostMessageW(
                Some(HWND(self.hwnd as *mut _)),
                WM_COMMAND_READY,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }
}

impl Toaster for ToastWindow {
    fn show(&self, toast: Toast) {
        self.send(Command::Show(toast));
    }

    fn hide(&self) {
        self.send(Command::Hide);
    }
}

thread_local! {
    static STATE: RefCell<Option<(Card, mpsc::Receiver<Command>)>> = const { RefCell::new(None) };
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = STATE.with(|state| {
        // ShowWindow, SetTimer and friends send messages back into this procedure while the
        // card is already borrowed. Those re-entrant messages get the default handling.
        let mut state = state.try_borrow_mut().ok()?;
        let (card, rx) = state.as_mut()?;
        match message {
            WM_COMMAND_READY => {
                while let Ok(command) = rx.try_recv() {
                    card.handle(command);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                card.mouse_move(lparam);
                Some(LRESULT(0))
            }
            WM_MOUSELEAVE => {
                card.mouse_leave();
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                card.click(lparam);
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                let cursor = if card.hover.is_some() {
                    IDC_HAND
                } else {
                    IDC_ARROW
                };
                if let Ok(c) = LoadCursorW(None, cursor) {
                    SetCursor(Some(c));
                }
                Some(LRESULT(1))
            }
            WM_TIMER if wparam.0 == HIDE_TIMER => {
                card.hide();
                Some(LRESULT(0))
            }
            WM_MOUSEACTIVATE => Some(LRESULT(MA_NOACTIVATE as isize)),
            _ => None,
        }
    });
    handled.unwrap_or_else(|| DefWindowProcW(hwnd, message, wparam, lparam))
}

/// Something on the card that reacts to a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Close,
    /// The n-th button, counted from the left.
    Button(usize),
    /// The card itself (opens the result of a finished job).
    Body,
}

struct Card {
    hwnd: HWND,
    surface: Surface,
    /// (scale, title, detail, button) formats, rebuilt when the scale changes.
    fonts: Option<(f64, IDWriteTextFormat, IDWriteTextFormat, IDWriteTextFormat)>,
    toast: Option<Toast>,
    scale: f64,
    origin: POINT,
    hover: Option<Target>,
    tracking: bool,
}

impl Card {
    fn handle(&mut self, command: Command) {
        match command {
            Command::Show(toast) => self.show(toast),
            Command::Hide => self.hide(),
        }
    }

    fn show(&mut self, toast: Toast) {
        let hide_after = match &toast {
            Toast::Running { .. } => None,
            Toast::Done { .. } => Some(8_000),
            Toast::Failed { .. } => Some(12_000),
            Toast::Notice { .. } => Some(20_000),
        };
        self.toast = Some(toast);
        self.hover = None;
        self.place();
        unsafe {
            if let Err(e) = self.render() {
                log(&format!("toast failed to draw: {e}"));
                return;
            }
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = KillTimer(Some(self.hwnd), HIDE_TIMER);
            if let Some(ms) = hide_after {
                SetTimer(Some(self.hwnd), HIDE_TIMER, ms, None);
            }
        }
    }

    fn hide(&mut self) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), HIDE_TIMER);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.toast = None;
        self.hover = None;
    }

    /// Bottom-right of the work area of the monitor the cursor is on.
    fn place(&mut self) {
        unsafe {
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            self.scale = scale_at(cursor);
            let mut info = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
            let work = if GetMonitorInfoW(monitor, &mut info).as_bool() {
                info.rcWork
            } else {
                RECT {
                    right: 1280,
                    bottom: 720,
                    ..Default::default()
                }
            };
            let s = self.scale as f32;
            self.origin = POINT {
                x: work.right - ((WIDTH + MARGIN) * s) as i32,
                y: work.bottom - ((HEIGHT + MARGIN) * s) as i32,
            };
        }
    }

    fn buttons(&self) -> Vec<&str> {
        match &self.toast {
            Some(Toast::Running { .. }) => vec!["Cancel"],
            Some(Toast::Done { .. }) => vec!["Open", "Show in folder"],
            Some(Toast::Notice { button, .. }) => vec![button.as_str()],
            Some(Toast::Failed { .. }) | None => vec![],
        }
    }

    /// Button rectangles in logical pixels, right-aligned along the bottom.
    fn button_rects(&self) -> Vec<D2D_RECT_F> {
        let mut right = WIDTH - PAD + 6.0;
        let mut rects: Vec<D2D_RECT_F> = self
            .buttons()
            .iter()
            .rev()
            .map(|label| {
                // Roughly 7 px per character at 12 px Segoe UI, plus padding.
                let width = 20.0 + 6.8 * label.chars().count() as f32;
                let rect = D2D_RECT_F {
                    left: right - width,
                    top: HEIGHT - 32.0,
                    right,
                    bottom: HEIGHT - 8.0,
                };
                right -= width + 6.0;
                rect
            })
            .collect();
        rects.reverse();
        rects
    }

    fn close_rect() -> D2D_RECT_F {
        D2D_RECT_F {
            left: WIDTH - 34.0,
            top: 6.0,
            right: WIDTH - 8.0,
            bottom: 32.0,
        }
    }

    fn target_at(&self, lparam: LPARAM) -> Option<Target> {
        self.toast.as_ref()?;
        let s = self.scale as f32;
        let x = (lparam.0 & 0xffff) as i16 as f32 / s;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as f32 / s;
        let inside = |r: &D2D_RECT_F| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
        if inside(&Self::close_rect()) {
            return Some(Target::Close);
        }
        if let Some(i) = self.button_rects().iter().position(inside) {
            return Some(Target::Button(i));
        }
        matches!(self.toast, Some(Toast::Done { .. })).then_some(Target::Body)
    }

    fn mouse_move(&mut self, lparam: LPARAM) {
        unsafe {
            if !self.tracking {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                self.tracking = TrackMouseEvent(&mut track).is_ok();
                // Keep the toast up while the user is looking at it.
                let _ = KillTimer(Some(self.hwnd), HIDE_TIMER);
            }
        }
        let hover = self.target_at(lparam);
        if hover != self.hover {
            self.hover = hover;
            let _ = unsafe { self.render() };
        }
    }

    fn mouse_leave(&mut self) {
        self.tracking = false;
        if self.hover.take().is_some() {
            let _ = unsafe { self.render() };
        }
        // Restart the countdown for toasts that hide on their own.
        if matches!(
            self.toast,
            Some(Toast::Done { .. } | Toast::Failed { .. } | Toast::Notice { .. })
        ) {
            unsafe {
                SetTimer(Some(self.hwnd), HIDE_TIMER, 4_000, None);
            }
        }
    }

    fn click(&mut self, lparam: LPARAM) {
        let (Some(target), Some(toast)) = (self.target_at(lparam), self.toast.clone()) else {
            return;
        };
        match (target, toast) {
            (Target::Close, _) => {}
            (Target::Button(0), Toast::Running { cancel, title, .. }) => {
                cancel.cancel();
                log(&format!("cancel requested: {title}"));
            }
            (Target::Button(0) | Target::Body, Toast::Done { output, .. }) => open(&output),
            (Target::Button(1), Toast::Done { output, .. }) => show_in_folder(&output),
            (Target::Button(0), Toast::Notice { on_click, .. }) => on_click(),
            _ => return,
        }
        self.hide();
    }

    unsafe fn render(&mut self) -> windows::core::Result<()> {
        let Some(toast) = self.toast.as_ref() else {
            return Ok(());
        };
        let scale = self.scale;
        if self.fonts.as_ref().map(|f| f.0) != Some(scale) {
            let title = self
                .surface
                .text_format(13.5 * scale, true, TextStyle::Line)?;
            let detail = self
                .surface
                .text_format(12.0 * scale, false, TextStyle::Wrapped)?;
            let button = self
                .surface
                .text_format(12.0 * scale, false, TextStyle::Centred)?;
            self.fonts = Some((scale, title, detail, button));
        }
        let (_, title_font, detail_font, button_font) = self.fonts.as_ref().unwrap();

        let s = scale as f32;
        let px = |r: D2D_RECT_F| D2D_RECT_F {
            left: r.left * s,
            top: r.top * s,
            right: r.right * s,
            bottom: r.bottom * s,
        };
        let (title, detail, accent) = match toast {
            Toast::Running { title, detail, .. } => {
                (title.as_str(), detail.clone(), (0x7c, 0xc4, 0xff))
            }
            Toast::Done { title, output } => (
                title.as_str(),
                output
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                (0x6c, 0xd8, 0x8f),
            ),
            Toast::Failed { title, message } => {
                (title.as_str(), message.clone(), (0xff, 0x7a, 0x7a))
            }
            Toast::Notice { title, message, .. } => {
                (title.as_str(), message.clone(), (0xff, 0xc8, 0x57))
            }
        };
        // Leave room for buttons, or let a failure message use two full lines.
        let detail_bottom = if self.buttons().is_empty() {
            HEIGHT - 10.0
        } else {
            HEIGHT - 36.0
        };

        let width = (WIDTH * s).ceil() as i32;
        let height = (HEIGHT * s).ceil() as i32;
        self.surface.begin(width, height)?;
        let surf = &self.surface;
        let t = &surf.target;

        let card = D2D1_ROUNDED_RECT {
            rect: px(D2D_RECT_F {
                left: 0.5,
                top: 0.5,
                right: WIDTH - 0.5,
                bottom: HEIGHT - 0.5,
            }),
            radiusX: 8.0 * s,
            radiusY: 8.0 * s,
        };
        t.FillRoundedRectangle(&card, &surf.brush(rgba(0x1f, 0x1f, 0x26, 0.98))?);
        t.DrawRoundedRectangle(&card, &surf.brush(rgba(0xff, 0xff, 0xff, 0.12))?, s, None);
        let accent_bar = D2D1_ROUNDED_RECT {
            rect: px(D2D_RECT_F {
                left: 6.0,
                top: 12.0,
                right: 9.0,
                bottom: HEIGHT - 12.0,
            }),
            radiusX: 1.5 * s,
            radiusY: 1.5 * s,
        };
        t.FillRoundedRectangle(
            &accent_bar,
            &surf.brush(rgba(accent.0, accent.1, accent.2, 1.0))?,
        );

        let text = surf.brush(rgba(0xf2, 0xf2, 0xf2, 1.0))?;
        let muted = surf.brush(rgba(0xc8, 0xc8, 0xd2, 1.0))?;
        let hover_fill = surf.brush(rgba(0xff, 0xff, 0xff, 0.14))?;
        let button_fill = surf.brush(rgba(0xff, 0xff, 0xff, 0.07))?;

        let title_rect = D2D_RECT_F {
            left: PAD + 2.0,
            top: 10.0,
            right: WIDTH - 40.0,
            bottom: 32.0,
        };
        draw_text(t, title, title_font, px(title_rect), &text);
        let detail_rect = D2D_RECT_F {
            left: PAD + 2.0,
            top: 34.0,
            right: WIDTH - PAD,
            bottom: detail_bottom,
        };
        draw_text(t, &detail, detail_font, px(detail_rect), &muted);

        let close = Self::close_rect();
        if self.hover == Some(Target::Close) {
            let r = D2D1_ROUNDED_RECT {
                rect: px(close),
                radiusX: 4.0 * s,
                radiusY: 4.0 * s,
            };
            t.FillRoundedRectangle(&r, &hover_fill);
        }
        draw_text(t, "\u{2715}", button_font, px(close), &muted);

        for (i, (label, rect)) in self.buttons().iter().zip(self.button_rects()).enumerate() {
            let r = D2D1_ROUNDED_RECT {
                rect: px(rect),
                radiusX: 4.0 * s,
                radiusY: 4.0 * s,
            };
            let fill = if self.hover == Some(Target::Button(i)) {
                &hover_fill
            } else {
                &button_fill
            };
            t.FillRoundedRectangle(&r, fill);
            draw_text(t, label, button_font, px(rect), &text);
        }

        self.surface
            .present(self.hwnd, self.origin.x, self.origin.y)
    }
}

fn open(path: &Path) {
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(path),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Opens the folder in Explorer with the file selected.
fn show_in_folder(path: &Path) {
    unsafe {
        let pidl = ILCreateFromPathW(&HSTRING::from(path));
        if pidl.is_null() {
            log(&format!("show in folder: {} not found", path.display()));
            return;
        }
        if let Err(e) = SHOpenFolderAndSelectItems(pidl, None, 0) {
            log(&format!("show in folder failed: {e}"));
        }
        ILFree(Some(pidl));
    }
}
