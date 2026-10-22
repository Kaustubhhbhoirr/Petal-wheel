//! Native wheel: a topmost, non-activating layered window drawn with Direct2D (see [`crate::gfx`]).
//!
//! The window is created once on its own thread and only hidden between uses.

use std::cell::RefCell;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;

use windows::core::{w, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Geometry, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_ELLIPSE,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};
use windows::Win32::Graphics::DirectWrite::IDWriteTextFormat;
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, ShowWindow, TranslateMessage, MSG,
    SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WS_EX_TRANSPARENT,
};
use windows_numerics::Vector2;

use super::{Overlay, Placement, WheelContent};
use crate::gfx::{centred_rect, create_layered_window, draw_text, rgba, Surface, TextStyle};
use crate::wheel::{self, slice_angle};

const WM_COMMAND_READY: u32 = WM_APP + 1;

enum Command {
    Show(Placement, WheelContent, Sender<Result<(), String>>),
    Highlight(Option<usize>),
    Hide,
}

pub struct NativeOverlay {
    commands: Mutex<Sender<Command>>,
    /// Stored as an integer because HWND is not Send.
    hwnd: isize,
}

impl NativeOverlay {
    /// Creates the window and its thread. Nothing is shown until [`Overlay::show`].
    pub fn start() -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Command>();
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("flick-wheel".into())
            .spawn(move || {
                // TRANSPARENT: clicks pass through; the wheel follows the cursor and the shortcut.
                let created = unsafe {
                    create_layered_window(w!("FlickWheel"), Some(window_proc), WS_EX_TRANSPARENT)
                }
                .and_then(|hwnd| Ok((hwnd, Surface::new()?)));
                let (hwnd, surface) = match created {
                    Ok(pair) => pair,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let renderer = Renderer {
                    hwnd,
                    surface,
                    fonts: None,
                    placement: None,
                    content: None,
                    highlighted: None,
                };
                STATE.with(|s| *s.borrow_mut() = Some((renderer, rx)));
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

impl Overlay for NativeOverlay {
    fn show(&self, at: Placement, content: &WheelContent) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(Command::Show(at, content.clone(), tx));
        rx.recv().map_err(|e| e.to_string())?
    }

    fn highlight(&self, index: Option<usize>) {
        self.send(Command::Highlight(index));
    }

    fn hide(&self) {
        self.send(Command::Hide);
    }
}

thread_local! {
    static STATE: RefCell<Option<(Renderer, mpsc::Receiver<Command>)>> = const { RefCell::new(None) };
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_COMMAND_READY {
        STATE.with(|state| {
            if let Some((renderer, rx)) = state.borrow_mut().as_mut() {
                while let Ok(command) = rx.try_recv() {
                    renderer.handle(command);
                }
            }
        });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

struct Renderer {
    hwnd: HWND,
    surface: Surface,
    /// Text formats are tied to a font size, so they are rebuilt when the scale changes.
    fonts: Option<(f64, IDWriteTextFormat, IDWriteTextFormat)>,
    placement: Option<Placement>,
    content: Option<WheelContent>,
    highlighted: Option<usize>,
}

impl Renderer {
    fn handle(&mut self, command: Command) {
        match command {
            Command::Show(placement, content, done) => {
                self.placement = Some(placement);
                self.content = Some(content);
                self.highlighted = None;
                let result = unsafe { self.render() }.map_err(|e| e.to_string());
                if result.is_ok() {
                    unsafe {
                        let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                    }
                }
                let _ = done.send(result);
            }
            Command::Highlight(index) => {
                if index != self.highlighted && self.content.is_some() {
                    self.highlighted = index;
                    let _ = unsafe { self.render() };
                }
            }
            Command::Hide => {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.content = None;
            }
        }
    }

    /// Renders the current state and pushes it to the layered window.
    unsafe fn render(&mut self) -> windows::core::Result<()> {
        let (Some(placement), Some(content)) = (self.placement, self.content.as_ref()) else {
            return Ok(());
        };
        let scale = placement.scale;
        let size = (wheel::window_size() * scale).ceil() as i32;

        if self.fonts.as_ref().map(|f| f.0) != Some(scale) {
            let label = self
                .surface
                .text_format(12.5 * scale, false, TextStyle::Centred)?;
            let caption = self
                .surface
                .text_format(13.5 * scale, true, TextStyle::Centred)?;
            self.fonts = Some((scale, label, caption));
        }
        let (_, label_font, caption_font) = self.fonts.as_ref().unwrap();

        self.surface.begin(size, size)?;
        let s = &self.surface;
        let t = &s.target;
        let c = (size as f32) / 2.0;
        let outer = (wheel::OUTER_RADIUS * scale) as f32;
        let inner = (wheel::INNER_RADIUS * scale) as f32;
        let ring_mid = (outer + inner) / 2.0;

        let backdrop = s.brush(rgba(0x14, 0x14, 0x1a, 0.96))?;
        let slice_fill = s.brush(rgba(0xff, 0xff, 0xff, 0.08))?;
        let hover_fill = s.brush(rgba(0x7c, 0xc4, 0xff, 0.92))?;
        let text = s.brush(rgba(0xf2, 0xf2, 0xf2, 1.0))?;
        let hover_text = s.brush(rgba(0x0b, 0x15, 0x20, 1.0))?;

        let count = content.slices.len();
        // A 2 px gap between slices, measured at the outer edge.
        let gap = (2.0 * scale) as f32 / outer;
        for (index, slice) in content.slices.iter().enumerate() {
            let angle = slice_angle(index, count) as f32;
            let half = std::f32::consts::PI / count as f32 - gap / 2.0;
            let segment = ring_segment(s, c, inner, outer, angle - half, angle + half)?;
            let hovered = self.highlighted == Some(index);
            t.FillGeometry(&segment, &backdrop, None);
            t.FillGeometry(
                &segment,
                if hovered { &hover_fill } else { &slice_fill },
                None,
            );

            let (x, y) = (c + ring_mid * angle.cos(), c + ring_mid * angle.sin());
            let rect = centred_rect(x, y, (outer - inner) * 1.7, (outer - inner) * 0.9);
            let brush = if hovered { &hover_text } else { &text };
            draw_text(t, &slice.label, label_font, rect, brush);
        }

        let centre = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: inner - 6.0 * scale as f32,
            radiusY: inner - 6.0 * scale as f32,
        };
        t.FillEllipse(&centre, &backdrop);
        let caption = match self.highlighted.and_then(|i| content.slices.get(i)) {
            Some(slice) => &slice.label,
            None => &content.caption,
        };
        let box_size = inner * 1.5;
        draw_text(
            t,
            caption,
            caption_font,
            centred_rect(c, c, box_size, box_size),
            &text,
        );

        self.surface
            .present(self.hwnd, placement.x - size / 2, placement.y - size / 2)
    }
}

/// A ring segment between two angles (radians, clockwise from +x, y down).
unsafe fn ring_segment(
    surface: &Surface,
    c: f32,
    inner: f32,
    outer: f32,
    from: f32,
    to: f32,
) -> windows::core::Result<ID2D1Geometry> {
    let at = |r: f32, a: f32| Vector2 {
        X: c + r * a.cos(),
        Y: c + r * a.sin(),
    };
    let arc_size = if to - from > std::f32::consts::PI {
        D2D1_ARC_SIZE_LARGE
    } else {
        D2D1_ARC_SIZE_SMALL
    };
    let path = surface.factory.CreatePathGeometry()?;
    let sink = path.Open()?;
    sink.BeginFigure(at(outer, from), D2D1_FIGURE_BEGIN_FILLED);
    sink.AddArc(&D2D1_ARC_SEGMENT {
        point: at(outer, to),
        size: D2D_SIZE_F {
            width: outer,
            height: outer,
        },
        rotationAngle: 0.0,
        sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
        arcSize: arc_size,
    });
    sink.AddLine(at(inner, to));
    sink.AddArc(&D2D1_ARC_SEGMENT {
        point: at(inner, from),
        size: D2D_SIZE_F {
            width: inner,
            height: inner,
        },
        rotationAngle: 0.0,
        sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
        arcSize: arc_size,
    });
    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
    sink.Close()?;
    path.cast()
}
