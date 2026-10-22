use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::{DynamicImage, GenericImageView, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use image_tools::{shrink, to_jpg, to_pdf, to_png, ErrorCode};
use lopdf::{Document, Object};
use tempfile::TempDir;

/// A photo-like test image: a gradient with some noise, so it does not compress to nothing.
fn photo(width: u32, height: u32) -> RgbImage {
    let mut seed = 12345u32;
    RgbImage::from_fn(width, height, |x, y| {
        seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
        let noise = (seed >> 24) as u8 / 4;
        Rgb([
            (x * 255 / width) as u8 / 2 + noise,
            (y * 255 / height) as u8 / 2 + noise,
            128 + noise,
        ])
    })
}

/// Left half opaque red, right half fully transparent.
fn half_transparent(width: u32, height: u32) -> RgbaImage {
    RgbaImage::from_fn(width, height, |x, _| {
        if x < width / 2 {
            Rgba([255, 0, 0, 255])
        } else {
            Rgba([0, 0, 0, 0])
        }
    })
}

fn save_as(
    dir: &TempDir,
    name: &str,
    image: impl Into<DynamicImage>,
    format: ImageFormat,
) -> PathBuf {
    let path = dir.path().join(name);
    image.into().save_with_format(&path, format).unwrap();
    path
}

fn jpeg_bytes(image: &RgbImage, quality: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, quality))
        .unwrap();
    bytes
}

/// Adds an EXIF block that says "rotate 90 degrees clockwise to view", like a phone photo
/// taken in portrait.
fn with_exif_rotate_90(jpeg: &[u8]) -> Vec<u8> {
    let tiff: [u8; 26] = [
        b'I', b'I', 0x2A, 0, 8, 0, 0, 0, // little-endian TIFF header, first IFD at 8
        1, 0, // one entry
        0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, // Orientation (SHORT) = 6
        0, 0, 0, 0, // no next IFD
    ];
    let mut app1 = vec![0xFF, 0xE1, 0, (2 + 6 + tiff.len()) as u8];
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The image XObject on each page, and each page's MediaBox width and height.
fn pdf_pages(path: &Path) -> Vec<(lopdf::Stream, f32, f32)> {
    let doc = Document::load(path).unwrap();
    doc.get_pages()
        .into_values()
        .map(|id| {
            let page = doc.get_dictionary(id).unwrap();
            let media: Vec<f32> = page
                .get(b"MediaBox")
                .and_then(Object::as_array)
                .unwrap()
                .iter()
                .map(|v| v.as_float().unwrap())
                .collect();
            let resources = page.get(b"Resources").and_then(Object::as_dict).unwrap();
            let xobjects = resources.get(b"XObject").and_then(Object::as_dict).unwrap();
            let image_id = xobjects.get(b"Im0").and_then(Object::as_reference).unwrap();
            let stream = doc
                .get_object(image_id)
                .and_then(Object::as_stream)
                .unwrap();
            (stream.clone(), media[2], media[3])
        })
        .collect()
}

fn filter(stream: &lopdf::Stream) -> String {
    let name = stream
        .dict
        .get(b"Filter")
        .and_then(Object::as_name)
        .unwrap();
    String::from_utf8_lossy(name).into_owned()
}

#[test]
fn images_become_one_pdf_page_each_at_96_dpi() {
    let dir = TempDir::new().unwrap();
    let jpeg = jpeg_bytes(&photo(200, 100), 85);
    let a = write(&dir, "a.jpg", &jpeg);
    let b = save_as(&dir, "b.png", half_transparent(40, 80), ImageFormat::Png);
    let out = dir.path().join("a - images.pdf");

    assert_eq!(to_pdf(&[a, b], &out).unwrap(), 2);

    let pages = pdf_pages(&out);
    let (jpg, w, h) = &pages[0];
    assert_eq!((*w, *h), (150.0, 75.0));
    assert_eq!(filter(jpg), "DCTDecode");
    // Embedded byte for byte: no quality lost.
    assert_eq!(jpg.content, jpeg);

    let (png, w, h) = &pages[1];
    assert_eq!((*w, *h), (30.0, 60.0));
    assert_eq!(filter(png), "FlateDecode");
    let mut pixels = Vec::new();
    flate2::read::ZlibDecoder::new(&png.content[..])
        .read_to_end(&mut pixels)
        .unwrap();
    assert_eq!(pixels.len(), 40 * 80 * 3);
    // First pixel is opaque red; the last one was transparent and is now white.
    assert_eq!(pixels[..3], [255, 0, 0]);
    assert_eq!(pixels[pixels.len() - 3..], [255, 255, 255]);
}

#[test]
fn portrait_phone_photos_get_portrait_pages() {
    let dir = TempDir::new().unwrap();
    // Stored 200 x 100, but EXIF says to turn it: it is shown 100 x 200.
    let jpeg = with_exif_rotate_90(&jpeg_bytes(&photo(200, 100), 85));
    let input = write(&dir, "portrait.jpg", &jpeg);
    let out = dir.path().join("out.pdf");

    to_pdf(std::slice::from_ref(&input), &out).unwrap();
    let pages = pdf_pages(&out);
    assert_eq!((pages[0].1, pages[0].2), (75.0, 150.0));
    assert_eq!(pages[0].0.content, jpeg);

    // Conversions turn the pixels themselves, since the new file has no EXIF.
    let png = dir.path().join("portrait.png");
    to_png(&input, &png).unwrap();
    assert_eq!(image::open(&png).unwrap().dimensions(), (100, 200));
}

#[test]
fn converts_between_formats() {
    let dir = TempDir::new().unwrap();
    let webp = dir.path().join("pic.webp");
    let image = DynamicImage::ImageRgba8(half_transparent(64, 32));
    image
        .write_with_encoder(WebPEncoder::new_lossless(
            std::fs::File::create(&webp).unwrap(),
        ))
        .unwrap();
    let bmp = save_as(&dir, "pic.bmp", photo(30, 20), ImageFormat::Bmp);
    let jpg = write(&dir, "photo.jpg", &jpeg_bytes(&photo(50, 40), 85));

    // PNG keeps transparency.
    let png = dir.path().join("pic.png");
    to_png(&webp, &png).unwrap();
    let back = image::open(&png).unwrap();
    assert_eq!(back.dimensions(), (64, 32));
    assert_eq!(back.get_pixel(63, 0).0[3], 0);

    // JPG has none: transparent areas turn white.
    let out = dir.path().join("pic.jpg");
    to_jpg(&webp, &out).unwrap();
    let back = image::open(&out).unwrap().into_rgb8();
    assert!(
        back.get_pixel(60, 16).0.iter().all(|&c| c > 245),
        "{:?}",
        back.get_pixel(60, 16)
    );
    assert!(back.get_pixel(4, 16).0[0] > 200);

    let out = dir.path().join("from bmp.png");
    to_png(&bmp, &out).unwrap();
    assert_eq!(image::open(&out).unwrap().dimensions(), (30, 20));

    let out = dir.path().join("from jpg.png");
    to_png(&jpg, &out).unwrap();
    assert_eq!(
        image::ImageReader::open(&out)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .format(),
        Some(ImageFormat::Png)
    );
}

#[test]
fn shrink_limits_the_longest_side_and_saves_space() {
    let dir = TempDir::new().unwrap();
    let big = save_as(&dir, "big.png", photo(3000, 2000), ImageFormat::Png);
    let out = dir.path().join("big - small.jpg");

    shrink(&big, &out).unwrap();

    assert_eq!(image::open(&out).unwrap().dimensions(), (1600, 1067));
    assert!(std::fs::metadata(&out).unwrap().len() < std::fs::metadata(&big).unwrap().len());
}

#[test]
fn shrink_refuses_when_it_would_not_get_smaller() {
    let dir = TempDir::new().unwrap();
    let small = write(&dir, "tiny.jpg", &jpeg_bytes(&photo(64, 64), 40));
    let out = dir.path().join("tiny - small.jpg");

    let err = shrink(&small, &out).unwrap_err();
    assert_eq!(err.code, ErrorCode::NotSmaller);
    assert!(err.message.contains("tiny.jpg"), "{}", err.message);
    assert!(!out.exists());
}

#[test]
fn rejects_corrupt_and_missing_images() {
    let dir = TempDir::new().unwrap();
    let junk = write(&dir, "junk.png", b"this is not an image at all");
    let png = std::fs::read(save_as(&dir, "ok.png", photo(64, 64), ImageFormat::Png)).unwrap();
    let truncated = write(&dir, "truncated.png", &png[..png.len() / 2]);
    let jpeg = jpeg_bytes(&photo(64, 64), 85);
    let bad_jpeg = write(&dir, "bad.jpg", &jpeg[..20]);
    let missing = dir.path().join("missing.png");
    let out = dir.path().join("out.pdf");

    for input in [&junk, &truncated, &bad_jpeg] {
        let err = to_png(input, &dir.path().join("x.png")).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::Unreadable,
            "{input:?}: {}",
            err.message
        );
        let err = to_pdf(std::slice::from_ref(input), &out).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::Unreadable,
            "{input:?}: {}",
            err.message
        );
    }
    assert_eq!(
        to_jpg(&missing, &dir.path().join("x.jpg"))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(
        shrink(&junk, &dir.path().join("x.jpg")).unwrap_err().code,
        ErrorCode::Unreadable
    );
    assert!(!out.exists());
    assert!(!dir.path().join("x.png").exists());
}

#[test]
fn reads_by_content_not_extension() {
    let dir = TempDir::new().unwrap();
    let disguised = write(&dir, "actually a jpg.png", &jpeg_bytes(&photo(20, 10), 85));
    let out = dir.path().join("out.png");
    to_png(&disguised, &out).unwrap();
    assert_eq!(image::open(&out).unwrap().dimensions(), (20, 10));
}

#[test]
fn never_overwrites_an_existing_file() {
    let dir = TempDir::new().unwrap();
    let input = write(&dir, "a.jpg", &jpeg_bytes(&photo(20, 10), 85));
    let original = std::fs::read(&input).unwrap();
    let existing = write(&dir, "a.png", b"keep me");

    assert_eq!(
        to_png(&input, &existing).unwrap_err().code,
        ErrorCode::OutputExists
    );
    assert_eq!(
        to_jpg(&input, &input).unwrap_err().code,
        ErrorCode::BadRequest
    );
    assert_eq!(
        to_pdf(std::slice::from_ref(&input), &existing)
            .unwrap_err()
            .code,
        ErrorCode::OutputExists
    );
    assert_eq!(
        to_pdf(&[], &dir.path().join("x.pdf")).unwrap_err().code,
        ErrorCode::BadRequest
    );

    assert_eq!(std::fs::read(&existing).unwrap(), b"keep me");
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

fn run_worker(stdin: &str) -> serde_json::Value {
    let mut child = Command::new(env!("CARGO_BIN_EXE_image-tools"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn speaks_json_over_stdio() {
    let dir = TempDir::new().unwrap();
    let a = write(&dir, "a.jpg", &jpeg_bytes(&photo(40, 30), 40));
    let b = save_as(&dir, "b.png", photo(30, 40), ImageFormat::Png);

    let out = dir.path().join("a - images.pdf");
    let request = serde_json::json!({ "op": "to_pdf", "inputs": [a, b], "output": out });
    let response = run_worker(&request.to_string());
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["pages"], 2);

    for (op, name) in [
        ("to_png", "a.png"),
        ("to_jpg", "b.jpg"),
        ("shrink", "c.jpg"),
    ] {
        let input = if op == "to_jpg" { &b } else { &a };
        let out = dir.path().join(name);
        let request = serde_json::json!({ "op": op, "inputs": [input], "output": out });
        let response = run_worker(&request.to_string());
        if op == "shrink" {
            // A tiny JPG cannot get smaller: a clear error, and no file.
            assert_eq!(response["error"]["code"], "not_smaller", "{response}");
            assert!(!out.exists());
        } else {
            assert_eq!(response["ok"], true, "{response}");
            assert!(out.exists());
        }
    }

    let request = serde_json::json!({ "op": "to_png", "inputs": [a, b], "output": "x.png" });
    assert_eq!(
        run_worker(&request.to_string())["error"]["code"],
        "bad_request"
    );
    assert_eq!(run_worker("{ not json")["error"]["code"], "bad_request");
}
