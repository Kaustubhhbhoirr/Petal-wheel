//! Wheel geometry shared by every renderer: where slices are and which one the cursor points at.

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

/// What a renderer needs to draw one slice. Which slices exist comes from packs
/// ([`crate::packs::Catalog::slices_for`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Slice {
    pub id: String,
    pub label: String,
}

impl From<&crate::packs::PackSlice> for Slice {
    fn from(slice: &crate::packs::PackSlice) -> Self {
        Self {
            id: slice.id.clone(),
            label: slice.label.clone(),
        }
    }
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
}
