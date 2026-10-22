//! Shared drawing for Flick's native windows (the wheel and the toast).
//!
//! Frames are rendered into a 32-bit premultiplied DIB through a software `ID2D1DCRenderTarget`
//! and pushed with `UpdateLayeredWindow`, which gives per-pixel alpha without a swap chain or GPU
//! device. The software target matters: a hardware one loads the GPU driver (~50 MB) into Flick.

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
    D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS,
    HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, LoadCursorW, RegisterClassExW, UpdateLayeredWindow, IDC_ARROW, ULW_ALPHA,
    WINDOW_EX_STYLE, WNDCLASSEXW, WNDPROC, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

/// Creates a topmost, layered, non-activating popup window. Flick's windows never take focus,
/// so Explorer keeps its selection. `extra` adds styles such as `WS_EX_TRANSPARENT`.
///
/// # Safety
/// `proc_` must be a valid window procedure. Call on the thread that will run the window's
/// message loop.
pub unsafe fn create_layered_window(
    class: PCWSTR,
    proc_: WNDPROC,
    extra: WINDOW_EX_STYLE,
) -> windows::core::Result<HWND> {
    let instance = GetModuleHandleW(None)?;
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: proc_,
        hInstance: instance.into(),
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        lpszClassName: class,
        ..Default::default()
    };
    RegisterClassExW(&wc);
    CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | extra,
        class,
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
    width: i32,
    height: i32,
}

impl Canvas {
    unsafe fn new(width: i32, height: i32) -> windows::core::Result<Self> {
        let dc = CreateCompatibleDC(None);
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
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
            width,
            height,
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

/// A render target plus the canvas it draws into, for one layered window.
pub struct Surface {
    pub factory: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    pub target: ID2D1DCRenderTarget,
    canvas: Option<Canvas>,
}

impl Surface {
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let properties = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                // Draw in physical pixels; callers apply the monitor scale to their geometry.
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&properties)?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Self {
                factory,
                dwrite,
                target,
                canvas: None,
            })
        }
    }

    /// Binds a transparent `width` x `height` canvas and starts a frame.
    ///
    /// # Safety
    /// A surface is single-threaded: use it only on the thread that created it.
    pub unsafe fn begin(&mut self, width: i32, height: i32) -> windows::core::Result<()> {
        if self.canvas.as_ref().map(|c| (c.width, c.height)) != Some((width, height)) {
            self.canvas = Some(Canvas::new(width, height)?);
        }
        let canvas = self.canvas.as_ref().unwrap();
        let rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        self.target.BindDC(canvas.dc, &rect)?;
        self.target.BeginDraw();
        self.target.Clear(Some(&rgba(0, 0, 0, 0.0)));
        Ok(())
    }

    /// Ends the frame and shows it in `hwnd` with its top-left corner at (`x`, `y`).
    ///
    /// # Safety
    /// Call after [`Surface::begin`], on the surface's thread, with a layered window handle.
    pub unsafe fn present(&mut self, hwnd: HWND, x: i32, y: i32) -> windows::core::Result<()> {
        self.target.EndDraw(None, None)?;
        let canvas = self.canvas.as_ref().expect("begin before present");
        let screen = GetDC(None);
        let position = POINT { x, y };
        let extent = SIZE {
            cx: canvas.width,
            cy: canvas.height,
        };
        let origin = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let result = UpdateLayeredWindow(
            hwnd,
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

    /// # Safety
    /// Use only on the surface's thread.
    pub unsafe fn brush(&self, color: D2D1_COLOR_F) -> windows::core::Result<ID2D1SolidColorBrush> {
        self.target.CreateSolidColorBrush(&color, None)
    }

    /// A Segoe UI text format in the given style.
    ///
    /// # Safety
    /// Use only on the surface's thread.
    pub unsafe fn text_format(
        &self,
        size: f64,
        bold: bool,
        style: TextStyle,
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
        if style == TextStyle::Centred {
            format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        } else {
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            if style == TextStyle::Line {
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            } else {
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?;
            }
            let ellipsis = self.dwrite.CreateEllipsisTrimmingSign(&format)?;
            let trimming = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            format.SetTrimming(&trimming, &ellipsis)?;
        }
        Ok(format)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextStyle {
    /// Centred both ways, wrapping (wheel labels).
    Centred,
    /// One left-aligned line, vertically centred, ending in "..." when too long.
    Line,
    /// Left-aligned from the top, wrapping, ending in "..." when it runs out of room.
    Wrapped,
}

/// Draws `text` inside `rect`, clipped to it.
///
/// # Safety
/// Call between [`Surface::begin`] and [`Surface::present`] on the surface's thread.
pub unsafe fn draw_text(
    target: &ID2D1DCRenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    rect: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let utf16: Vec<u16> = text.encode_utf16().collect();
    target.DrawText(
        &utf16,
        format,
        &rect,
        brush,
        D2D1_DRAW_TEXT_OPTIONS_CLIP,
        DWRITE_MEASURING_MODE_NATURAL,
    );
}

/// A `width` x `height` rectangle centred on (`x`, `y`).
pub fn centred_rect(x: f32, y: f32, width: f32, height: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: x - width / 2.0,
        top: y - height / 2.0,
        right: x + width / 2.0,
        bottom: y + height / 2.0,
    }
}

pub fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}
