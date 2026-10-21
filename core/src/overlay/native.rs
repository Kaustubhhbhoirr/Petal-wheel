//! Native wheel: a topmost, non-activating layered window drawn with Direct2D and DirectWrite.
//!
//! Frames are rendered into a 32-bit premultiplied DIB through an `ID2D1DCRenderTarget` and pushed
//! with `UpdateLayeredWindow`, which gives per-pixel alpha without a swap chain or GPU device.
//! The window is created once on its own thread and only hidden between uses.

use std::cell::RefCell;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;

use windows::core::{w, Interface, HSTRING};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED,
    D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1Geometry, ID2D1SolidColorBrush,
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassExW,
    ShowWindow, TranslateMessage, UpdateLayeredWindow, MSG, SW_HIDE, SW_SHOWNOACTIVATE, ULW_ALPHA,
    WM_APP, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows_numerics::Vector2;

use super::{Overlay, Placement, WheelContent};
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
                let hwnd = match unsafe { create_window() } {
                    Ok(hwnd) => hwnd,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let renderer = match Renderer::new(hwnd) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
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

unsafe fn create_window() -> windows::core::Result<HWND> {
    let instance = GetModuleHandleW(None)?;
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(window_proc),
        hInstance: instance.into(),
        lpszClassName: w!("FlickWheel"),
        ..Default::default()
    };
    RegisterClassExW(&class);
    // TRANSPARENT: clicks pass through, the wheel is driven by the cursor position and the
    // shortcut. NOACTIVATE: Explorer keeps focus and its selection.
    CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
        w!("FlickWheel"),
        w!("Flick"),
        WS_POPUP,
        0,
        0,
        0,
        0,
        None,
        None,
        Some(instance.into()),
        None,
    )
}

/// A 32-bit DIB selected into a memory DC, the canvas the render target draws into.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    size: i32,
}

impl Canvas {
    unsafe fn new(size: i32) -> windows::core::Result<Self> {
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let previous = SelectObject(dc, bitmap.into());
        Ok(Self {
            dc,
            bitmap,
            previous,
            size,
        })
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

struct Renderer {
    hwnd: HWND,
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    target: ID2D1DCRenderTarget,
    canvas: Option<Canvas>,
    /// Text formats are tied to a font size, so they are rebuilt when the scale changes.
    fonts: Option<(f64, IDWriteTextFormat, IDWriteTextFormat)>,
    placement: Option<Placement>,
    content: Option<WheelContent>,
    highlighted: Option<usize>,
}

impl Renderer {
    fn new(hwnd: HWND) -> windows::core::Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                // Draw in physical pixels; the scale is applied to the geometry.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&properties)?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Self {
                hwnd,
                factory,
                dwrite,
                target,
                canvas: None,
                fonts: None,
                placement: None,
                content: None,
                highlighted: None,
            })
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Show(placement, content, done) => {
                self.placement = Some(placement);
                self.content = Some(content);
                self.highlighted = None;
                let result = unsafe { self.present() }.map_err(|e| e.to_string());
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
                    let _ = unsafe { self.present() };
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
    unsafe fn present(&mut self) -> windows::core::Result<()> {
        let (Some(placement), Some(content)) = (self.placement, self.content.as_ref()) else {
            return Ok(());
        };
        let scale = placement.scale;
        let size = (wheel::window_size() * scale).ceil() as i32;
        if self.canvas.as_ref().map(|c| c.size) != Some(size) {
            self.canvas = Some(Canvas::new(size)?);
        }
        let canvas = self.canvas.as_ref().unwrap();
        let rect = RECT {
            left: 0,
            top: 0,
            right: size,
            bottom: size,
        };
        self.target.BindDC(canvas.dc, &rect)?;

        if self.fonts.as_ref().map(|f| f.0) != Some(scale) {
            let label = self.text_format(12.5 * scale, false)?;
            let caption = self.text_format(13.5 * scale, true)?;
            self.fonts = Some((scale, label, caption));
        }
        let (_, label_font, caption_font) = self.fonts.as_ref().unwrap();

        let t = &self.target;
        let c = (size as f32) / 2.0;
        let outer = (wheel::OUTER_RADIUS * scale) as f32;
        let inner = (wheel::INNER_RADIUS * scale) as f32;
        let ring_mid = (outer + inner) / 2.0;

        let backdrop = t.CreateSolidColorBrush(&rgba(0x14, 0x14, 0x1a, 0.88), None)?;
        let slice_fill = t.CreateSolidColorBrush(&rgba(0xff, 0xff, 0xff, 0.08), None)?;
        let hover_fill = t.CreateSolidColorBrush(&rgba(0x7c, 0xc4, 0xff, 0.92), None)?;
        let text = t.CreateSolidColorBrush(&rgba(0xf2, 0xf2, 0xf2, 1.0), None)?;
        let dim_text = t.CreateSolidColorBrush(&rgba(0xf2, 0xf2, 0xf2, 0.4), None)?;
        let hover_text = t.CreateSolidColorBrush(&rgba(0x0b, 0x15, 0x20, 1.0), None)?;

        t.BeginDraw();
        t.Clear(Some(&rgba(0, 0, 0, 0.0)));

        let count = content.slices.len();
        // A 2 px gap between slices, measured at the outer edge.
        let gap = (2.0 * scale) as f32 / outer;
        for (index, slice) in content.slices.iter().enumerate() {
            let angle = slice_angle(index, count) as f32;
            let half = std::f32::consts::PI / count as f32 - gap / 2.0;
            let segment = self.ring_segment(c, inner, outer, angle - half, angle + half)?;
            let hovered = self.highlighted == Some(index);
            t.FillGeometry(&segment, &backdrop, None);
            t.FillGeometry(
                &segment,
                if hovered { &hover_fill } else { &slice_fill },
                None,
            );

            let (x, y) = (c + ring_mid * angle.cos(), c + ring_mid * angle.sin());
            let w = (outer - inner) * 1.7;
            let h = (outer - inner) * 0.9;
            let brush = match (hovered, slice.enabled) {
                (true, _) => &hover_text,
                (false, true) => &text,
                (false, false) => &dim_text,
            };
            draw_text(t, slice.label, label_font, x, y, w, h, brush);
        }

        let centre = D2D1_ELLIPSE {
            point: Vector2 { X: c, Y: c },
            radiusX: inner - 6.0 * scale as f32,
            radiusY: inner - 6.0 * scale as f32,
        };
        t.FillEllipse(&centre, &backdrop);
        let caption = match self.highlighted.and_then(|i| content.slices.get(i)) {
            Some(slice) if !slice.enabled => format!("{}\n(coming soon)", slice.label),
            Some(slice) => slice.label.to_string(),
            None => content.caption.clone(),
        };
        let box_size = inner * 1.5;
        draw_text(t, &caption, caption_font, c, c, box_size, box_size, &text);

        t.EndDraw(None, None)?;

        let screen = GetDC(None);
        let position = POINT {
            x: placement.x - size / 2,
            y: placement.y - size / 2,
        };
        let extent = SIZE { cx: size, cy: size };
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let result = UpdateLayeredWindow(
            self.hwnd,
            Some(screen),
            Some(&position),
            Some(&extent),
            Some(canvas.dc),
            Some(&origin),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(None, screen);
        result
    }

    /// A ring segment between two angles (radians, clockwise from +x, y down).
    unsafe fn ring_segment(
        &self,
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
        let path = self.factory.CreatePathGeometry()?;
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

    unsafe fn text_format(
        &self,
        size: f64,
        bold: bool,
    ) -> windows::core::Result<IDWriteTextFormat> {
        let format = self.dwrite.CreateTextFormat(
            &HSTRING::from("Segoe UI Variable Text"),
            None,
            if bold {
                DWRITE_FONT_WEIGHT_SEMI_BOLD
            } else {
                DWRITE_FONT_WEIGHT_NORMAL
            },
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size as f32,
            w!("en-us"),
        )?;
        format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(format)
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn draw_text(
    target: &ID2D1DCRenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    brush: &ID2D1SolidColorBrush,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    let rect = D2D_RECT_F {
        left: x - width / 2.0,
        top: y - height / 2.0,
        right: x + width / 2.0,
        bottom: y + height / 2.0,
    };
    target.DrawText(
        &utf16,
        format,
        &rect,
        brush,
        D2D1_DRAW_TEXT_OPTIONS_NONE,
        DWRITE_MEASURING_MODE_NATURAL,
    );
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}
