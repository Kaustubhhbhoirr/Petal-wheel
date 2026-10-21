//! Wheel overlay renderers. The spike builds two and measures them (SPEC 6.5):
//!
//! - [`native`]: a layered window drawn with Direct2D, created once and kept hidden.
//! - [`webview`]: a transparent WebView2 window, created on open and destroyed on close
//!   (or kept warm and hidden, to measure what that costs).
//!
//! Renderers only draw. The controller decides which slice is under the cursor.

pub mod native;
pub mod webview;

use crate::wheel::Slice;

/// What the renderer draws: the slices and a caption for the centre.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WheelContent {
    pub slices: Vec<Slice>,
    /// Shown in the centre when no slice is highlighted, e.g. "2 PDFs".
    pub caption: String,
}

/// Where to put the wheel: the cursor position in physical pixels and the monitor scale.
#[derive(Debug, Clone, Copy)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    /// Monitor DPI / 96.
    pub scale: f64,
}

pub trait Overlay: Send + Sync {
    /// Shows the wheel centred on `at`. Returns once the first frame is on screen.
    fn show(&self, at: Placement, content: &WheelContent) -> Result<(), String>;
    fn highlight(&self, index: Option<usize>);
    fn hide(&self);
}
