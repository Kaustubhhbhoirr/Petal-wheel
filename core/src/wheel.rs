//! Wheel model shared by every renderer: which slices to show and which one the cursor points at.

use std::path::Path;

/// Logical (96 DPI) sizes. Renderers multiply by the monitor scale factor.
pub const OUTER_RADIUS: f64 = 140.0;
pub const INNER_RADIUS: f64 = 84.0;
/// Releasing closer than this to the centre cancels.
pub const DEADZONE: f64 = 28.0;
/// Room around the ring for anti-aliasing and the hover glow.
pub const PADDING: f64 = 12.0;

/// Width and height of the wheel window in logical pixels.
pub fn window_size() -> f64 {
    2.0 * (OUTER_RADIUS + PADDING)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Slice {
    pub id: &'static str,
    pub label: &'static str,
    /// Disabled slices are drawn dimmed and do nothing when chosen (not built yet).
    pub enabled: bool,
}

const fn slice(id: &'static str, label: &'static str, enabled: bool) -> Slice {
    Slice { id, label, enabled }
}

/// The slices for a selection (SPEC 4.3). Only Merge is wired up in the v0.1 spike.
pub fn slices_for(paths: &[impl AsRef<Path>]) -> Vec<Slice> {
    let pdfs = paths
        .iter()
        .filter(|p| has_ext(p.as_ref(), &["pdf"]))
        .count();
    let images = paths
        .iter()
        .filter(|p| has_ext(p.as_ref(), &["png", "jpg", "jpeg", "webp"]))
        .count();
    let all = paths.len();

    let mut slices = match (all, pdfs, images) {
        (0, _, _) => return Vec::new(),
        (1, 1, _) => vec![
            slice("pdf.to_word", "To Word", false),
            slice("pdf.to_images", "To images", false),
            slice("pdf.split", "Split", false),
            slice("pdf.compress", "Compress", false),
            slice("pdf.remove_pages", "Remove pages", false),
            slice("pdf.organize", "Organize", false),
        ],
        (n, p, _) if n == p => vec![
            slice("pdf.merge", "Merge", true),
            slice("pdf.compress_all", "Compress all", false),
            slice("pdf.to_images", "To images", false),
        ],
        (1, _, 1) => vec![
            slice("image.convert", "Convert", false),
            slice("image.to_pdf", "To PDF", false),
            slice("image.shrink", "Shrink", false),
        ],
        (n, _, i) if n == i => vec![
            slice("image.combine_pdf", "Combine PDF", false),
            slice("image.convert_all", "Convert all", false),
            slice("image.shrink_all", "Shrink all", false),
        ],
        _ => Vec::new(),
    };
    slices.push(slice("any.copy_path", "Copy path", false));
    slices.push(slice("any.zip", "Zip", false));
    slices
}

fn has_ext(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Centre angle of slice `index` of `count`, in radians. Slice 0 is at the top; the rest
/// follow clockwise (screen coordinates, y down).
pub fn slice_angle(index: usize, count: usize) -> f64 {
    -std::f64::consts::FRAC_PI_2 + index as f64 * std::f64::consts::TAU / count as f64
}

/// Which slice the cursor points at, given its offset from the wheel centre in logical pixels.
///
/// Distance beyond the ring still counts, so a quick flick past the edge picks the slice in that
/// direction. Inside the dead zone nothing is picked.
pub fn hit_test(dx: f64, dy: f64, count: usize) -> Option<usize> {
    if count == 0 || dx.hypot(dy) < DEADZONE {
        return None;
    }
    let step = std::f64::consts::TAU / count as f64;
    // Rotate so slice 0's centre is at angle 0, then shift by half a slice so boundaries are
    // at multiples of `step`.
    let angle = dy.atan2(dx) + std::f64::consts::FRAC_PI_2 + step / 2.0;
    let index = (angle.rem_euclid(std::f64::consts::TAU) / step) as usize;
    Some(index.min(count - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_and_empty_wheel_pick_nothing() {
        assert_eq!(hit_test(5.0, 5.0, 6), None);
        assert_eq!(hit_test(100.0, 0.0, 0), None);
    }

    #[test]
    fn four_slices_follow_the_compass_clockwise() {
        assert_eq!(hit_test(0.0, -100.0, 4), Some(0)); // up
        assert_eq!(hit_test(100.0, 0.0, 4), Some(1)); // right
        assert_eq!(hit_test(0.0, 100.0, 4), Some(2)); // down
        assert_eq!(hit_test(-100.0, 0.0, 4), Some(3)); // left
    }

    #[test]
    fn boundaries_split_between_neighbours() {
        // With 4 slices the boundary between 0 and 1 is up-right at 45 degrees.
        assert_eq!(hit_test(10.0, -100.0, 4), Some(0));
        assert_eq!(hit_test(100.0, -10.0, 4), Some(1));
        // Just left of straight up wraps to slice 0, not the last slice.
        assert_eq!(hit_test(-10.0, -100.0, 4), Some(0));
    }

    #[test]
    fn flicking_past_the_ring_still_counts() {
        assert_eq!(hit_test(0.0, 900.0, 4), Some(2));
    }

    #[test]
    fn hit_test_agrees_with_slice_angle() {
        for count in 1..=10 {
            for index in 0..count {
                let a = slice_angle(index, count);
                let hit = hit_test(100.0 * a.cos(), 100.0 * a.sin(), count);
                assert_eq!(hit, Some(index), "count {count} index {index}");
            }
        }
    }

    #[test]
    fn slices_depend_on_selection() {
        let ids =
            |paths: &[&str]| -> Vec<&str> { slices_for(paths).iter().map(|s| s.id).collect() };
        assert!(ids(&[]).is_empty());
        assert_eq!(ids(&["a.PDF", "b.pdf"])[0], "pdf.merge");
        assert_eq!(ids(&["a.pdf"])[0], "pdf.to_word");
        assert_eq!(ids(&["a.png"])[0], "image.convert");
        assert_eq!(ids(&["a.png", "b.jpg"])[0], "image.combine_pdf");
        assert_eq!(ids(&["a.pdf", "b.png"]), ["any.copy_path", "any.zip"]);
        // Only merge is enabled during the spike.
        let enabled: Vec<_> = slices_for(&["a.pdf", "b.pdf"])
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| s.id)
            .collect();
        assert_eq!(enabled, ["pdf.merge"]);
    }
}
