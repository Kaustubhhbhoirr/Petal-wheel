//! Flick's native image worker.
//!
//! The worker reads one JSON [`Request`] from stdin and writes one JSON [`Response`] to stdout.
//! It only ever writes to the `output` path it was given and refuses to touch an existing file,
//! so an original can never be overwritten, even if the core sends a bad request.
//!
//! Photos are turned upright using their EXIF orientation, since the new files do not carry it.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Cursor, Write};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::{JpegDecoder, JpegEncoder};
use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, GrayImage, ImageDecoder, ImageError, ImageReader, Luma, Rgb, RgbImage};
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};
use serde::{Deserialize, Serialize};

/// Images become pages at this resolution, so a 96 DPI screenshot prints at its on-screen size.
const PAGE_DPI: f32 = 96.0;
/// PDF readers reject pages larger than 200 inches; bigger images are scaled down to fit.
const MAX_PAGE_POINTS: f32 = 14_400.0;
const JPG_QUALITY: u8 = 90;
const SHRINK_QUALITY: u8 = 82;
/// Shrink makes the longest side at most this many pixels.
const SHRINK_MAX_SIDE: u32 = 1600;

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// All inputs, in order, as one PDF with a page per image.
    ToPdf {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
    ToPng {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
    ToJpg {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
    /// A smaller JPG copy, for upload limits.
    Shrink {
        inputs: Vec<PathBuf>,
        output: PathBuf,
    },
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Response {
    Ok {
        ok: bool,
        output: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        pages: Option<u32>,
    },
    Err {
        ok: bool,
        error: WorkerError,
    },
}

#[derive(Debug, Serialize, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct WorkerError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    NotFound,
    Unreadable,
    TooLarge,
    NotSmaller,
    OutputExists,
    WriteFailed,
}

impl WorkerError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Runs a request and converts the result into the wire response.
pub fn handle(request: Request) -> Response {
    let result = match request {
        Request::ToPdf { inputs, output } => to_pdf(&inputs, &output).map(|n| (output, Some(n))),
        Request::ToPng { inputs, output } => one_input(&inputs, "To PNG")
            .and_then(|input| to_png(input, &output))
            .map(|()| (output, None)),
        Request::ToJpg { inputs, output } => one_input(&inputs, "To JPG")
            .and_then(|input| to_jpg(input, &output))
            .map(|()| (output, None)),
        Request::Shrink { inputs, output } => one_input(&inputs, "Shrink")
            .and_then(|input| shrink(input, &output))
            .map(|()| (output, None)),
    };
    match result {
        Ok((output, pages)) => Response::Ok {
            ok: true,
            output,
            pages,
        },
        Err(error) => Response::Err { ok: false, error },
    }
}

/// Conversions work on one file per run; the core runs them once per selected file.
fn one_input<'a>(inputs: &'a [PathBuf], op: &str) -> Result<&'a Path, WorkerError> {
    match inputs {
        [input] => Ok(input),
        _ => Err(WorkerError::new(
            ErrorCode::BadRequest,
            format!("{op} takes one image at a time."),
        )),
    }
}

/// Saves `input` as a PNG at `output`. Transparency is kept.
pub fn to_png(input: &Path, output: &Path) -> Result<(), WorkerError> {
    check_output(&[input], output)?;
    let image = open(input)?;
    write_image(output, |file| {
        let encoder =
            PngEncoder::new_with_quality(file, CompressionType::Default, PngFilter::Adaptive);
        Ok(image.write_with_encoder(encoder)?)
    })
}

/// Saves `input` as a JPG at `output`. Transparent areas become white.
pub fn to_jpg(input: &Path, output: &Path) -> Result<(), WorkerError> {
    check_output(&[input], output)?;
    let image = flatten(open(input)?);
    write_image(output, |file| {
        Ok(image.write_with_encoder(JpegEncoder::new_with_quality(file, JPG_QUALITY))?)
    })
}

/// Saves a smaller JPG copy of `input` at `output`: at most 1600 pixels on the longest side.
/// Fails without writing anything if the copy would not be smaller than the original.
pub fn shrink(input: &Path, output: &Path) -> Result<(), WorkerError> {
    check_output(&[input], output)?;
    let original_size = std::fs::metadata(input).map(|m| m.len()).unwrap_or(0);
    let mut image = flatten(open(input)?);
    if image.width().max(image.height()) > SHRINK_MAX_SIDE {
        // Triangle (bilinear, widened when shrinking) is smooth and much faster than Lanczos.
        image = image.resize(SHRINK_MAX_SIDE, SHRINK_MAX_SIDE, FilterType::Triangle);
    }
    let mut encoded = Vec::new();
    image
        .write_with_encoder(JpegEncoder::new_with_quality(&mut encoded, SHRINK_QUALITY))
        .map_err(|e| write_error(output, e))?;
    if encoded.len() as u64 >= original_size {
        return Err(WorkerError::new(
            ErrorCode::NotSmaller,
            format!(
                "{} is already small; a shrunk copy would not be any smaller.",
                display_name(input)
            ),
        ));
    }
    write_image(output, |file| Ok(file.write_all(&encoded)?))
}

/// Puts `inputs` into one new PDF at `output`, one page per image in the given order, each page
/// the size of its image at 96 DPI. Returns the page count.
///
/// JPGs are embedded byte for byte, so they lose no quality. Other images are stored losslessly
/// (Flate), with transparent areas made white.
pub fn to_pdf(inputs: &[PathBuf], output: &Path) -> Result<u32, WorkerError> {
    if inputs.is_empty() {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "Select at least one image.",
        ));
    }
    let input_refs: Vec<&Path> = inputs.iter().map(PathBuf::as_path).collect();
    check_output(&input_refs, output)?;

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for input in inputs {
        let picture = Picture::load(input)?;
        let (width, height) = picture.display_size();
        // Page size in points (1/72 inch).
        let mut scale = 72.0 / PAGE_DPI;
        let longest = width.max(height) as f32 * scale;
        if longest > MAX_PAGE_POINTS {
            scale *= MAX_PAGE_POINTS / longest;
        }
        let (page_w, page_h) = (width as f32 * scale, height as f32 * scale);

        let image_id = doc.add_object(picture.stream);
        let matrix = placement(picture.orientation, picture.width, picture.height, scale);
        let content = Content {
            operations: vec![
                Operation::new("q", vec![]),
                Operation::new("cm", matrix.iter().map(|&v| Object::Real(v)).collect()),
                Operation::new("Do", vec!["Im0".into()]),
                Operation::new("Q", vec![]),
            ],
        };
        let content = content.encode().map_err(|e| write_error(output, e))?;
        let content_id = doc.add_object(Stream::new(dictionary! {}, content));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(page_w), Object::Real(page_h)],
            "Resources" => dictionary! {
                "XObject" => dictionary! { "Im0" => image_id },
            },
            "Contents" => content_id,
        });
        kids.push(Object::Reference(page_id));
    }
    let count = kids.len() as u32;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count as i64,
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    write_image(output, |file| {
        let mut writer = BufWriter::new(file);
        doc.save_to(&mut writer)?;
        writer.flush()?;
        Ok(())
    })?;
    Ok(count)
}

/// One image ready to be placed on a PDF page.
struct Picture {
    /// The image XObject.
    stream: Stream,
    /// Stored size, before `orientation` is applied.
    width: u32,
    height: u32,
    /// How the stored pixels must be turned to look right (only JPGs are stored unturned).
    orientation: Orientation,
}

impl Picture {
    fn load(path: &Path) -> Result<Self, WorkerError> {
        let bytes = read(path)?;
        if let Some(picture) = Self::jpeg_as_is(path, &bytes)? {
            return Ok(picture);
        }
        let image = flatten(decode(path, &bytes)?);
        let (width, height) = (image.width(), image.height());
        let (color_space, pixels) = match image {
            DynamicImage::ImageLuma8(gray) => ("DeviceGray", gray.into_raw()),
            other => ("DeviceRGB", other.into_rgb8().into_raw()),
        };
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let compressed = encoder
            .write_all(&pixels)
            .and_then(|()| encoder.finish())
            .map_err(|e| write_error(path, e))?;
        let dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width as i64,
            "Height" => height as i64,
            "ColorSpace" => color_space,
            "BitsPerComponent" => 8,
            "Filter" => "FlateDecode",
        };
        Ok(Self {
            stream: Stream::new(dict, compressed),
            width,
            height,
            orientation: Orientation::NoTransforms,
        })
    }

    /// A JPG that PDF readers can show directly (8-bit baseline or progressive), embedded as it is.
    fn jpeg_as_is(path: &Path, bytes: &[u8]) -> Result<Option<Self>, WorkerError> {
        let Some(info) = jpeg_info(bytes) else {
            return Ok(None);
        };
        let color_space = match info.components {
            1 => "DeviceGray",
            3 => "DeviceRGB",
            4 => "DeviceCMYK",
            _ => return Ok(None),
        };
        // The decoder checks the headers and reads the EXIF orientation.
        let orientation = JpegDecoder::new(Cursor::new(bytes))
            .and_then(|mut decoder| decoder.orientation())
            .map_err(|e| image_error(path, e))?;
        let mut dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => info.width as i64,
            "Height" => info.height as i64,
            "ColorSpace" => color_space,
            "BitsPerComponent" => 8,
            "Filter" => "DCTDecode",
        };
        if info.components == 4 && info.adobe {
            // Adobe apps write CMYK JPGs inverted.
            let decode: Vec<Object> = [1, 0, 1, 0, 1, 0, 1, 0].map(Object::Integer).to_vec();
            dict.set("Decode", decode);
        }
        Ok(Some(Self {
            stream: Stream::new(dict, bytes.to_vec()),
            width: info.width,
            height: info.height,
            orientation,
        }))
    }

    /// Width and height once the orientation is applied.
    fn display_size(&self) -> (u32, u32) {
        if swaps_sides(self.orientation) {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

fn swaps_sides(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    )
}

/// The `cm` matrix that draws a `width` x `height` image (PDF draws images into a unit square)
/// filling the page, turned by `orientation`, with `scale` points per pixel.
fn placement(orientation: Orientation, width: u32, height: u32, scale: f32) -> [f32; 6] {
    let (w, h) = (width as f32, height as f32);
    let page_h = if swaps_sides(orientation) { w } else { h };
    // Where a point of the stored image (x right, y down, in pixels) ends up on the page
    // (y up), following the EXIF orientation numbers.
    let place = |x: f32, y: f32| -> (f32, f32) {
        let (px, py) = match orientation.to_exif() {
            2 => (w - x, y),
            3 => (w - x, h - y),
            4 => (x, h - y),
            5 => (y, x),
            6 => (h - y, x),
            7 => (h - y, w - x),
            8 => (y, w - x),
            _ => (x, y),
        };
        (px * scale, (page_h - py) * scale)
    };
    // The unit square's (0,0) is the image's bottom-left corner, (1,0) its bottom-right and
    // (0,1) its top-left.
    let origin = place(0.0, h);
    let right = place(w, h);
    let up = place(0.0, 0.0);
    [
        right.0 - origin.0,
        right.1 - origin.1,
        up.0 - origin.0,
        up.1 - origin.1,
        origin.0,
        origin.1,
    ]
}

struct JpegInfo {
    width: u32,
    height: u32,
    components: u8,
    /// Has an Adobe APP14 segment, which marks inverted CMYK.
    adobe: bool,
}

/// Reads a JPG's frame header. `None` if this is not a JPG that can go into a PDF untouched.
fn jpeg_info(bytes: &[u8]) -> Option<JpegInfo> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut adobe = false;
    let mut i = 2;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let length = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        let segment = bytes.get(i + 4..i + 2 + length.max(2))?;
        match marker {
            0xEE if segment.starts_with(b"Adobe") => adobe = true,
            // Baseline, extended and progressive frames. Lossless and arithmetic-coded JPGs
            // are rare and many PDF readers cannot show them.
            0xC0..=0xC2 => {
                let [precision, h1, h0, w1, w0, components, ..] = *segment else {
                    return None;
                };
                let (width, height) = (
                    u16::from_be_bytes([w1, w0]) as u32,
                    u16::from_be_bytes([h1, h0]) as u32,
                );
                return (precision == 8 && width > 0 && height > 0).then_some(JpegInfo {
                    width,
                    height,
                    components,
                    adobe,
                });
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xDA => return None,
            _ => {}
        }
        i += 2 + length;
    }
    None
}

/// Opens an image of any supported format (by content, not extension), turned upright.
fn open(path: &Path) -> Result<DynamicImage, WorkerError> {
    decode(path, &read(path)?)
}

fn decode(path: &Path, bytes: &[u8]) -> Result<DynamicImage, WorkerError> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| image_error(path, ImageError::IoError(e)))?;
    if reader.format().is_none() {
        return Err(WorkerError::new(
            ErrorCode::Unreadable,
            format!(
                "{} is not a PNG, JPG, WebP or BMP image.",
                display_name(path)
            ),
        ));
    }
    let mut decoder = reader.into_decoder().map_err(|e| image_error(path, e))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| image_error(path, e))?;
    image.apply_orientation(orientation);
    Ok(image)
}

/// The image with 8 bits per channel and no transparency: transparent areas become white.
fn flatten(image: DynamicImage) -> DynamicImage {
    // Alpha-blend one channel onto white, rounding.
    fn on_white(value: u8, alpha: u8) -> u8 {
        let (v, a) = (value as u32, alpha as u32);
        ((v * a + 255 * (255 - a) + 127) / 255) as u8
    }
    let color = image.color();
    match (color.has_color(), color.has_alpha()) {
        (false, false) => DynamicImage::ImageLuma8(image.into_luma8()),
        (true, false) => DynamicImage::ImageRgb8(image.into_rgb8()),
        (false, true) => {
            let la = image.into_luma_alpha8();
            DynamicImage::ImageLuma8(GrayImage::from_fn(la.width(), la.height(), |x, y| {
                let [v, a] = la.get_pixel(x, y).0;
                Luma([on_white(v, a)])
            }))
        }
        (true, true) => {
            let rgba = image.into_rgba8();
            DynamicImage::ImageRgb8(RgbImage::from_fn(rgba.width(), rgba.height(), |x, y| {
                let [r, g, b, a] = rgba.get_pixel(x, y).0;
                Rgb([on_white(r, a), on_white(g, a), on_white(b, a)])
            }))
        }
    }
}

fn read(path: &Path) -> Result<Vec<u8>, WorkerError> {
    if !path.is_file() {
        return Err(WorkerError::new(
            ErrorCode::NotFound,
            format!("{} was not found.", display_name(path)),
        ));
    }
    std::fs::read(path).map_err(|e| {
        WorkerError::new(
            ErrorCode::Unreadable,
            format!("{} could not be read ({e}).", display_name(path)),
        )
    })
}

fn image_error(path: &Path, error: ImageError) -> WorkerError {
    let name = display_name(path);
    match error {
        ImageError::Limits(_) => WorkerError::new(
            ErrorCode::TooLarge,
            format!("{name} is too large to open safely."),
        ),
        ImageError::Unsupported(e) => WorkerError::new(
            ErrorCode::Unreadable,
            format!("{name} uses an image format Flick cannot read ({e})."),
        ),
        e => WorkerError::new(
            ErrorCode::Unreadable,
            format!("{name} could not be read as an image ({e})."),
        ),
    }
}

fn write_error(path: &Path, error: impl std::fmt::Display) -> WorkerError {
    WorkerError::new(
        ErrorCode::WriteFailed,
        format!("Could not write {} ({error}).", display_name(path)),
    )
}

/// Refuses an output that is one of the inputs or already exists.
fn check_output(inputs: &[&Path], output: &Path) -> Result<(), WorkerError> {
    if inputs.iter().any(|input| same_file(input, output)) {
        return Err(WorkerError::new(
            ErrorCode::BadRequest,
            "The output path is one of the inputs.",
        ));
    }
    if output.exists() {
        return Err(output_exists(output));
    }
    Ok(())
}

fn output_exists(output: &Path) -> WorkerError {
    WorkerError::new(
        ErrorCode::OutputExists,
        format!("{} already exists.", display_name(output)),
    )
}

/// Creates `output`, which must not exist yet (`create_new` makes the check and the create
/// atomic), and lets `write` fill it. On any failure the new file is removed again.
fn write_image(
    output: &Path,
    write: impl FnOnce(&mut File) -> Result<(), ImageErrorOrPdf>,
) -> Result<(), WorkerError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                output_exists(output)
            } else {
                WorkerError::new(
                    ErrorCode::WriteFailed,
                    format!("Could not create {} ({e}).", display_name(output)),
                )
            }
        })?;
    let result = write(&mut file)
        .map_err(|e| e.to_string())
        .and_then(|()| file.sync_all().map_err(|e| e.to_string()));
    if let Err(e) = result {
        drop(file);
        let _ = std::fs::remove_file(output);
        return Err(write_error(output, e));
    }
    Ok(())
}

/// The errors the writers in [`write_image`] can return.
#[derive(Debug, thiserror::Error)]
enum ImageErrorOrPdf {
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Pdf(#[from] lopdf::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}
