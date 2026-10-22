//! Native wheel: a topmost, non-activating layered window drawn with Direct2D (see [`crate::gfx`]).
//!
//! The window is created once on its own thread and only hidden between uses. The layout comes
//! from the theme's style (orbit, pie or ring) and the colours from the theme.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;

use windows::core::{w, Interface};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    ID2D1Geometry, ID2D1PathGeometry, ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ARC_SEGMENT,
    D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_CAP_STYLE_ROUND, D2D1_DASH_STYLE_CUSTOM,
    D2D1_DASH_STYLE_SOLID, D2D1_ELLIPSE, D2D1_LINE_JOIN_ROUND, D2D1_ROUNDED_RECT,
    D2D1_STROKE_STYLE_PROPERTIES, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextFormat, DWRITE_TEXT_METRICS};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, ShowWindow, TranslateMessage, MSG,
    SW_HIDE, SW_SHOWNOACTIVATE, WM_APP, WS_EX_TRANSPARENT,
};
use windows_numerics::{Matrix3x2, Vector2};

use super::{Overlay, Placement, WheelContent};
use crate::gfx::{centred_rect, create_layered_window, draw_text, Surface, TextStyle};
use crate::icons::{self, Segment};
use crate::theme::{Rgba, Style, Theme};
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
                    strokes: None,
                    icons: HashMap::new(),
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
            // ShowWindow can send messages back here while the renderer is borrowed.
            if let Ok(mut state) = state.try_borrow_mut() {
                if let Some((renderer, rx)) = state.as_mut() {
                    while let Ok(command) = rx.try_recv() {
                        renderer.handle(command);
                    }
                }
            }
        });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, message, wparam, lparam)
}

/// Text formats for one scale factor.
struct Fonts {
    scale: f64,
    label: IDWriteTextFormat,
    label_bold: IDWriteTextFormat,
    title: IDWriteTextFormat,
    small: IDWriteTextFormat,
    count: IDWriteTextFormat,
}

/// Round-capped solid and dashed strokes.
struct Strokes {
    round: ID2D1StrokeStyle,
    dashed: ID2D1StrokeStyle,
}

struct Renderer {
    hwnd: HWND,
    surface: Surface,
    fonts: Option<Fonts>,
    strokes: Option<Strokes>,
    /// Icon geometry by path data, built once and drawn with a transform.
    icons: HashMap<&'static str, ID2D1PathGeometry>,
    placement: Option<Placement>,
    content: Option<WheelContent>,
    highlighted: Option<usize>,
}

/// Everything a layout needs while drawing one frame.
struct Frame<'a> {
    surface: &'a Surface,
    fonts: &'a Fonts,
    strokes: &'a Strokes,
    icons: &'a HashMap<&'static str, ID2D1PathGeometry>,
    theme: &'a Theme,
    content: &'a WheelContent,
    hovered: Option<usize>,
    /// Logical pixels to physical pixels.
    s: f32,
    /// Centre of the canvas in physical pixels.
    c: f32,
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

    unsafe fn prepare(&mut self, scale: f64) -> windows::core::Result<()> {
        if self.fonts.as_ref().map(|f| f.scale) != Some(scale) {
            let f = |size: f64, bold: bool| {
                self.surface
                    .text_format(size * scale, bold, TextStyle::Centred)
            };
            self.fonts = Some(Fonts {
                scale,
                label: f(12.0, false)?,
                label_bold: f(12.0, true)?,
                title: f(16.0, true)?,
                small: f(11.5, false)?,
                count: f(19.0, true)?,
            });
        }
        if self.strokes.is_none() {
            let props = |dash| D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                dashCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                miterLimit: 10.0,
                dashStyle: dash,
                dashOffset: 0.0,
            };
            let factory = &self.surface.factory;
            self.strokes = Some(Strokes {
                round: factory.CreateStrokeStyle(&props(D2D1_DASH_STYLE_SOLID), None)?,
                // Dash lengths are in stroke widths: short dots with wide gaps.
                dashed: factory
                    .CreateStrokeStyle(&props(D2D1_DASH_STYLE_CUSTOM), Some(&[0.1, 4.0]))?,
            });
        }
        if let Some(content) = &self.content {
            for slice in &content.slices {
                let data = icons::path_for(slice.icon.as_deref());
                if !self.icons.contains_key(data) {
                    self.icons.insert(data, icon_geometry(&self.surface, data)?);
                }
            }
        }
        Ok(())
    }

    /// Renders the current state and pushes it to the layered window.
    unsafe fn render(&mut self) -> windows::core::Result<()> {
        let Some(placement) = self.placement else {
            return Ok(());
        };
        let Some(style) = self.content.as_ref().map(|c| c.theme.style) else {
            return Ok(());
        };
        self.prepare(placement.scale)?;
        let size = (wheel::window_size_for(style) * placement.scale).ceil() as i32;
        self.surface.begin(size, size)?;
        let content = self.content.as_ref().unwrap();
        let frame = Frame {
            surface: &self.surface,
            fonts: self.fonts.as_ref().unwrap(),
            strokes: self.strokes.as_ref().unwrap(),
            icons: &self.icons,
            theme: &content.theme,
            content,
            hovered: self.highlighted,
            s: placement.scale as f32,
            c: size as f32 / 2.0,
        };
        let drawn = match style {
            Style::Orbit => frame.orbit(),
            Style::Pie => frame.pie(),
            Style::Ring => frame.ring(),
        };
        // Always end the frame, even if a layout failed part-way.
        let shown = self
            .surface
            .present(self.hwnd, placement.x - size / 2, placement.y - size / 2);
        drawn.and(shown)
    }
}

impl Frame<'_> {
    /// Round buttons on a dotted orbit; labels outside; file count in the centre.
    unsafe fn orbit(&self) -> windows::core::Result<()> {
        let colors = &self.theme.colors;
        let t = &self.surface.target;
        let (s, c) = (self.s, self.c);
        let orbit = 118.0;
        let count = self.content.slices.len();

        self.circle_shadow(c, c, 34.0, colors.shadow)?;
        for i in 0..count {
            let (x, y) = self.at(orbit, i, count);
            let r = if self.hovered == Some(i) { 36.0 } else { 29.0 };
            self.circle_shadow(x, y, r, colors.shadow)?;
        }

        let track = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: orbit * s,
            radiusY: orbit * s,
        };
        t.DrawEllipse(
            &track,
            &self.brush(colors.outline)?,
            1.5 * s,
            &self.strokes.dashed,
        );

        if let Some(i) = self.hovered {
            let a = slice_angle(i, count) as f32;
            let from = Vector2 {
                X: c + 36.0 * s * a.cos(),
                Y: c + 36.0 * s * a.sin(),
            };
            let to = Vector2 {
                X: c + (orbit - 40.0) * s * a.cos(),
                Y: c + (orbit - 40.0) * s * a.sin(),
            };
            t.DrawLine(
                from,
                to,
                &self.brush(colors.accent)?,
                2.5 * s,
                &self.strokes.round,
            );
        }

        let centre = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: 34.0 * s,
            radiusY: 34.0 * s,
        };
        t.FillEllipse(&centre, &self.brush(colors.centre)?);
        t.DrawEllipse(&centre, &self.brush(colors.outline)?, s, None);

        for (i, slice) in self.content.slices.iter().enumerate() {
            let hot = self.hovered == Some(i);
            let (x, y) = self.at(orbit, i, count);
            let r = if hot { 36.0 } else { 29.0 };
            let button = D2D1_ELLIPSE {
                point: Vector2 { X: x, Y: y },
                radiusX: r * s,
                radiusY: r * s,
            };
            if hot {
                t.FillEllipse(&button, &self.brush(colors.accent)?);
            } else {
                t.FillEllipse(&button, &self.brush(colors.surface)?);
                t.DrawEllipse(&button, &self.brush(colors.outline)?, s, None);
            }
            let ink = if hot { colors.on_accent } else { colors.text };
            self.icon(
                slice.icon.as_deref(),
                x,
                y,
                if hot { 26.0 } else { 22.0 },
                ink,
            )?;

            let (lx, ly) = self.at(orbit + r + 22.0, i, count);
            self.label(&slice.label, lx, ly, hot)?;
        }

        // "2 PDFs" -> a big "2" over a small "PDFs". Anything else goes under the centre.
        let caption = &self.content.caption;
        match caption.split_once(' ') {
            Some((number, rest)) if number.chars().all(|ch| ch.is_ascii_digit()) => {
                self.text(
                    number,
                    &self.fonts.count,
                    c,
                    c - 7.0 * s,
                    60.0,
                    24.0,
                    colors.text,
                )?;
                self.text(
                    rest,
                    &self.fonts.small,
                    c,
                    c + 12.0 * s,
                    60.0,
                    16.0,
                    colors.muted,
                )?;
            }
            _ => {
                self.text(
                    caption,
                    &self.fonts.small,
                    c,
                    c + 54.0 * s,
                    220.0,
                    18.0,
                    colors.muted,
                )?;
            }
        }
        Ok(())
    }

    /// A solid disc cut into slices, with icon and label inside each.
    unsafe fn pie(&self) -> windows::core::Result<()> {
        let colors = &self.theme.colors;
        let t = &self.surface.target;
        let (s, c) = (self.s, self.c);
        let count = self.content.slices.len();
        let (inner, outer, disc) = (62.0, 148.0, 152.0);

        self.circle_shadow(c, c, disc, colors.shadow)?;
        let disc_shape = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: disc * s,
            radiusY: disc * s,
        };
        t.FillEllipse(&disc_shape, &self.brush(colors.surface)?);
        t.DrawEllipse(&disc_shape, &self.brush(colors.outline)?, s, None);

        let step = std::f32::consts::TAU / count.max(1) as f32;
        for i in 0..count {
            let a = slice_angle(i, count) as f32;
            let gap = 2.0 / outer;
            let wedge = self.wedge(inner, outer, a - step / 2.0 + gap, a + step / 2.0 - gap)?;
            let fill = if self.hovered == Some(i) {
                colors.accent
            } else {
                colors.slice
            };
            t.FillGeometry(&wedge, &self.brush(fill)?, None);
        }
        if let Some(i) = self.hovered {
            let a = slice_angle(i, count) as f32;
            let arc = self.arc(160.0, a - step / 2.0 + 0.06, a + step / 2.0 - 0.06)?;
            t.DrawGeometry(
                &arc,
                &self.brush(colors.accent)?,
                4.0 * s,
                &self.strokes.round,
            );
        }

        let centre = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: 58.0 * s,
            radiusY: 58.0 * s,
        };
        t.FillEllipse(&centre, &self.brush(colors.centre)?);
        t.DrawEllipse(&centre, &self.brush(colors.outline)?, s, None);

        for (i, slice) in self.content.slices.iter().enumerate() {
            let hot = self.hovered == Some(i);
            let (x, y) = self.at(104.0, i, count);
            let ink = if hot { colors.on_accent } else { colors.text };
            self.icon(slice.icon.as_deref(), x, y - 11.0 * s, 22.0, ink)?;
            let font = if hot {
                &self.fonts.label_bold
            } else {
                &self.fonts.label
            };
            self.text(&slice.label, font, x, y + 12.0 * s, 92.0, 16.0, ink)?;
        }

        self.centre_text(110.0)
    }

    /// A thin segmented ring with labels outside and a large icon of the action in the centre.
    unsafe fn ring(&self) -> windows::core::Result<()> {
        let colors = &self.theme.colors;
        let t = &self.surface.target;
        let (s, c) = (self.s, self.c);
        let count = self.content.slices.len();
        let (r0, r1) = (112.0, 146.0);

        self.circle_shadow(c, c, 96.0, colors.shadow)?;
        let centre = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: 96.0 * s,
            radiusY: 96.0 * s,
        };
        t.FillEllipse(&centre, &self.brush(colors.centre)?);
        t.DrawEllipse(&centre, &self.brush(colors.outline)?, s, None);

        let step = std::f32::consts::TAU / count.max(1) as f32;
        for (i, slice) in self.content.slices.iter().enumerate() {
            let hot = self.hovered == Some(i);
            let a = slice_angle(i, count) as f32;
            let segment = self.wedge(r0, r1, a - step / 2.0 + 0.035, a + step / 2.0 - 0.035)?;
            let (fill, edge) = if hot {
                (colors.accent, colors.accent)
            } else {
                (colors.surface, colors.outline)
            };
            t.FillGeometry(&segment, &self.brush(fill)?, None);
            t.DrawGeometry(&segment, &self.brush(edge)?, s, None);

            let (x, y) = self.at((r0 + r1) / 2.0, i, count);
            let ink = if hot { colors.on_accent } else { colors.text };
            self.icon(slice.icon.as_deref(), x, y, 20.0, ink)?;
            let (lx, ly) = self.at(r1 + 30.0, i, count);
            self.label(&slice.label, lx, ly, hot)?;
        }

        match self.hovered.and_then(|i| self.content.slices.get(i)) {
            Some(slice) => {
                self.icon(slice.icon.as_deref(), c, c - 26.0 * s, 38.0, colors.accent)?;
                let title = format!("{} {}", slice.label, self.content.caption);
                self.text(
                    &title,
                    &self.fonts.title,
                    c,
                    c + 12.0 * s,
                    160.0,
                    22.0,
                    colors.text,
                )?;
                self.text(
                    "Release to run",
                    &self.fonts.small,
                    c,
                    c + 32.0 * s,
                    160.0,
                    16.0,
                    colors.muted,
                )?;
            }
            None => {
                let caption = &self.content.caption;
                self.text(
                    caption,
                    &self.fonts.title,
                    c,
                    c - 8.0 * s,
                    160.0,
                    44.0,
                    colors.text,
                )?;
                if count > 0 {
                    self.text(
                        "Move toward an action",
                        &self.fonts.small,
                        c,
                        c + 22.0 * s,
                        160.0,
                        16.0,
                        colors.muted,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// The hovered action's name over the caption, or just the caption (pie centre).
    unsafe fn centre_text(&self, width: f32) -> windows::core::Result<()> {
        let colors = &self.theme.colors;
        let c = self.c;
        let s = self.s;
        match self.hovered.and_then(|i| self.content.slices.get(i)) {
            Some(slice) => {
                self.text(
                    &slice.label,
                    &self.fonts.title,
                    c,
                    c - 8.0 * s,
                    width,
                    22.0,
                    colors.text,
                )?;
                self.text(
                    &self.content.caption,
                    &self.fonts.small,
                    c,
                    c + 12.0 * s,
                    width,
                    16.0,
                    colors.muted,
                )
            }
            None => self.text(
                &self.content.caption,
                &self.fonts.title,
                c,
                c,
                width,
                44.0,
                colors.text,
            ),
        }
    }

    /// A label drawn outside the wheel: bold and bright when hovered, muted otherwise, on a small
    /// pill in the centre colour so it stays readable over any window or wallpaper.
    unsafe fn label(&self, text: &str, x: f32, y: f32, hot: bool) -> windows::core::Result<()> {
        let colors = &self.theme.colors;
        let (font, color) = if hot {
            (&self.fonts.label_bold, colors.text)
        } else {
            (&self.fonts.label, colors.muted)
        };
        let utf16: Vec<u16> = text.encode_utf16().collect();
        let layout =
            self.surface
                .dwrite
                .CreateTextLayout(&utf16, font, 400.0 * self.s, 40.0 * self.s)?;
        let mut metrics = DWRITE_TEXT_METRICS::default();
        layout.GetMetrics(&mut metrics)?;
        let (w, h) = (metrics.width + 16.0 * self.s, 20.0 * self.s);
        let pill = D2D1_ROUNDED_RECT {
            rect: centred_rect(x, y, w, h),
            radiusX: h / 2.0,
            radiusY: h / 2.0,
        };
        let backing = Rgba {
            a: colors.centre.a.min(0.88),
            ..colors.centre
        };
        self.surface
            .target
            .FillRoundedRectangle(&pill, &self.brush(backing)?);
        self.text(text, font, x, y, 160.0, 18.0, color)
    }

    /// Text centred on (`x`, `y`) in a `w` x `h` logical box.
    #[allow(clippy::too_many_arguments)]
    unsafe fn text(
        &self,
        text: &str,
        font: &IDWriteTextFormat,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgba,
    ) -> windows::core::Result<()> {
        let rect = centred_rect(x, y, w * self.s, h * self.s);
        draw_text(&self.surface.target, text, font, rect, &self.brush(color)?);
        Ok(())
    }

    /// Draws an icon of `size` logical pixels centred on (`x`, `y`).
    unsafe fn icon(
        &self,
        name: Option<&str>,
        x: f32,
        y: f32,
        size: f32,
        color: Rgba,
    ) -> windows::core::Result<()> {
        let Some(geometry) = self.icons.get(icons::path_for(name)) else {
            return Ok(());
        };
        let k = size * self.s / 24.0;
        let transform = Matrix3x2 {
            M11: k,
            M12: 0.0,
            M21: 0.0,
            M22: k,
            M31: x - 12.0 * k,
            M32: y - 12.0 * k,
        };
        let t = &self.surface.target;
        t.SetTransform(&transform);
        t.DrawGeometry(geometry, &self.brush(color)?, 1.8, &self.strokes.round);
        t.SetTransform(&Matrix3x2::identity());
        Ok(())
    }

    /// A soft shadow under a circle: stacked translucent rings, offset down.
    unsafe fn circle_shadow(
        &self,
        x: f32,
        y: f32,
        r: f32,
        color: Rgba,
    ) -> windows::core::Result<()> {
        const STEPS: usize = 8;
        let brush = self.brush(Rgba {
            a: color.a / STEPS as f32 * 0.9,
            ..color
        })?;
        let dy = (r * 0.07).clamp(4.0, 10.0) * self.s;
        for k in (0..STEPS).rev() {
            let spread = (k as f32 + 1.0) * 1.6 * self.s;
            let e = D2D1_ELLIPSE {
                point: Vector2 { X: x, Y: y + dy },
                radiusX: r * self.s + spread,
                radiusY: r * self.s + spread,
            };
            self.surface.target.FillEllipse(&e, &brush);
        }
        Ok(())
    }

    /// Position on a circle of logical radius `r` at slice `index` of `count`.
    fn at(&self, r: f32, index: usize, count: usize) -> (f32, f32) {
        let a = slice_angle(index, count) as f32;
        (self.c + r * self.s * a.cos(), self.c + r * self.s * a.sin())
    }

    /// A ring segment between two angles (radians, clockwise from +x, y down).
    unsafe fn wedge(
        &self,
        inner: f32,
        outer: f32,
        from: f32,
        to: f32,
    ) -> windows::core::Result<ID2D1Geometry> {
        let (c, s) = (self.c, self.s);
        let (inner, outer) = (inner * s, outer * s);
        let at = |r: f32, a: f32| Vector2 {
            X: c + r * a.cos(),
            Y: c + r * a.sin(),
        };
        let arc_size = if to - from > std::f32::consts::PI {
            D2D1_ARC_SIZE_LARGE
        } else {
            D2D1_ARC_SIZE_SMALL
        };
        let path = self.surface.factory.CreatePathGeometry()?;
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

    /// An open arc of logical radius `r` between two angles.
    unsafe fn arc(&self, r: f32, from: f32, to: f32) -> windows::core::Result<ID2D1Geometry> {
        let (c, r) = (self.c, r * self.s);
        let at = |a: f32| Vector2 {
            X: c + r * a.cos(),
            Y: c + r * a.sin(),
        };
        let path = self.surface.factory.CreatePathGeometry()?;
        let sink = path.Open()?;
        sink.BeginFigure(at(from), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: at(to),
            size: D2D_SIZE_F {
                width: r,
                height: r,
            },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: D2D1_ARC_SIZE_SMALL,
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        path.cast()
    }

    unsafe fn brush(&self, color: Rgba) -> windows::core::Result<ID2D1SolidColorBrush> {
        self.surface.brush(D2D1_COLOR_F {
            r: color.r,
            g: color.g,
            b: color.b,
            a: color.a,
        })
    }
}

/// Builds a stroke-only geometry from an icon's path data (24x24 units).
unsafe fn icon_geometry(surface: &Surface, data: &str) -> windows::core::Result<ID2D1PathGeometry> {
    let segments = icons::parse(data).unwrap_or_default();
    let path = surface.factory.CreatePathGeometry()?;
    let sink = path.Open()?;
    let mut open = false;
    let v = |x: f32, y: f32| Vector2 { X: x, Y: y };
    for segment in segments {
        match segment {
            Segment::Move(x, y) => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
                sink.BeginFigure(v(x, y), D2D1_FIGURE_BEGIN_HOLLOW);
                open = true;
            }
            Segment::Line(x, y) => sink.AddLine(v(x, y)),
            Segment::Cubic(p) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                point1: v(p[0], p[1]),
                point2: v(p[2], p[3]),
                point3: v(p[4], p[5]),
            }),
            Segment::Arc {
                rx,
                ry,
                rotation,
                large,
                sweep,
                to,
            } => sink.AddArc(&D2D1_ARC_SEGMENT {
                point: v(to.0, to.1),
                size: D2D_SIZE_F {
                    width: rx,
                    height: ry,
                },
                rotationAngle: rotation,
                // SVG's sweep flag 1 is clockwise on screen, as is Direct2D's CLOCKWISE.
                sweepDirection: if sweep {
                    D2D1_SWEEP_DIRECTION_CLOCKWISE
                } else {
                    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                },
                arcSize: if large {
                    D2D1_ARC_SIZE_LARGE
                } else {
                    D2D1_ARC_SIZE_SMALL
                },
            }),
            Segment::Close => {
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
            }
        }
    }
    if open {
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
    }
    sink.Close()?;
    Ok(path)
}
