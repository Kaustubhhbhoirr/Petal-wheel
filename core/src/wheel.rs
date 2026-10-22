//! Wheel geometry shared by every renderer: where slices are and which one the cursor points at.

/// Logical (96 DPI) sizes. Renderers multiply by the monitor scale factor.
pub const OUTER_RADIUS: f64 = 140.0;
pub const INNER_RADIUS: f64 = 84.0;
/// Releasing closer than this to the centre cancels.
pub const DEADZONE: f64 = 28.0;
/// Room around the ring for anti-aliasing and the hover glow.
pub const PADDING: f64 = 12.0;

/// Width and height of the WebView2 wheel window in logical pixels.
pub fn window_size() -> f64 {
    2.0 * (OUTER_RADIUS + PADDING)
}

/// Width and height of the native wheel window for a theme style, in logical pixels: room for
/// the wheel, labels drawn outside it, and the shadow.
pub fn window_size_for(style: crate::theme::Style) -> f64 {
    use crate::theme::Style;
    match style {
        Style::Orbit => 470.0,
        Style::Pie => 370.0,
        Style::Ring => 490.0,
    }
}

/// What a renderer needs to draw one slice. Which slices exist comes from packs
/// ([`crate::packs::Catalog::slices_for`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Slice {
    pub id: String,
    pub label: String,
    /// Icon name from the pack manifest (see [`crate::icons::path_for`]).
    pub icon: Option<String>,
}

impl From<&crate::packs::PackSlice> for Slice {
    fn from(slice: &crate::packs::PackSlice) -> Self {
        Self {
            id: slice.id.clone(),
            label: slice.label.clone(),
            icon: slice.icon.clone(),
        }
    }
}

/// Centre angle of slice `index` of `count`, in radians. Slice 0 is at the top; the rest
/// follow clockwise (screen coordinates, y down).
pub fn slice_angle(index: usize, count: usize) -> f64 {
    -std::f64::consts::FRAC_PI_2 + index as f64 * std::f64::consts::TAU / count as f64
}

/// How far from the centre releasing still cancels, in logical pixels: the visible centre circle
/// of each layout, so "back to the middle" means what it looks like.
pub fn cancel_radius(style: crate::theme::Style) -> f64 {
    use crate::theme::Style;
    match style {
        // The centre circle is 34 px; a little extra makes it easy to hit.
        Style::Orbit => 50.0,
        // The inner circle, where the slices start.
        Style::Pie => 60.0,
        // The large centre disc inside the ring.
        Style::Ring => 96.0,
    }
}

/// Which slice the cursor points at, given its offset from the wheel centre in logical pixels.
///
/// Distance beyond the ring still counts, so a quick flick past the edge picks the slice in that
/// direction. Closer to the centre than `cancel_radius` nothing is picked, and releasing cancels.
pub fn hit_test(dx: f64, dy: f64, count: usize, cancel_radius: f64) -> Option<usize> {
    if count == 0 || dx.hypot(dy) < cancel_radius {
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
        assert_eq!(hit_test(5.0, 5.0, 6, DEADZONE), None);
        assert_eq!(hit_test(100.0, 0.0, 0, DEADZONE), None);
    }

    #[test]
    fn four_slices_follow_the_compass_clockwise() {
        assert_eq!(hit_test(0.0, -100.0, 4, DEADZONE), Some(0)); // up
        assert_eq!(hit_test(100.0, 0.0, 4, DEADZONE), Some(1)); // right
        assert_eq!(hit_test(0.0, 100.0, 4, DEADZONE), Some(2)); // down
        assert_eq!(hit_test(-100.0, 0.0, 4, DEADZONE), Some(3)); // left
    }

    #[test]
    fn boundaries_split_between_neighbours() {
        // With 4 slices the boundary between 0 and 1 is up-right at 45 degrees.
        assert_eq!(hit_test(10.0, -100.0, 4, DEADZONE), Some(0));
        assert_eq!(hit_test(100.0, -10.0, 4, DEADZONE), Some(1));
        // Just left of straight up wraps to slice 0, not the last slice.
        assert_eq!(hit_test(-10.0, -100.0, 4, DEADZONE), Some(0));
    }

    #[test]
    fn releasing_inside_the_visible_centre_cancels() {
        use crate::theme::Style;
        // Back near the middle but not exactly on it: still a cancel, in every layout.
        for style in [Style::Orbit, Style::Pie, Style::Ring] {
            let zone = cancel_radius(style);
            assert_eq!(hit_test(30.0, 25.0, 3, zone), None, "{style:?}");
            assert_eq!(hit_test(0.0, -(zone + 10.0), 3, zone), Some(0), "{style:?}");
        }
        // Orbit's centre circle is 34 px: anywhere on it cancels.
        assert_eq!(hit_test(0.0, 34.0, 3, cancel_radius(Style::Orbit)), None);
    }

    #[test]
    fn flicking_past_the_ring_still_counts() {
        assert_eq!(hit_test(0.0, 900.0, 4, DEADZONE), Some(2));
    }

    #[test]
    fn hit_test_agrees_with_slice_angle() {
        for count in 1..=10 {
            for index in 0..count {
                let a = slice_angle(index, count);
                let hit = hit_test(100.0 * a.cos(), 100.0 * a.sin(), count, DEADZONE);
                assert_eq!(hit, Some(index), "count {count} index {index}");
            }
        }
    }
}
