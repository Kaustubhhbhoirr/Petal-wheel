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

/// The main ring holds at most this many slots. With more applicable actions, the last slot
/// becomes [`MORE_ID`] and the rest move to its sub-ring.
pub const MAIN_RING_MAX: usize = 6;
/// Id of the synthetic "More" slice.
pub const MORE_ID: &str = "flick.more";

/// What releasing over a slice does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Kind {
    /// Runs a job straight away (`quick` slices).
    Run,
    /// Opens a sub-ring of children (`choice` slices and More).
    Parent,
    /// Opens the Flick app window (`app` slices).
    App,
}

/// What a renderer needs to draw one slice, and what the controller needs to act on it. Which
/// slices exist comes from packs ([`crate::packs::Catalog::tree_for`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Slice {
    pub id: String,
    pub label: String,
    /// Icon name from the pack manifest (see [`crate::icons::path_for`]).
    pub icon: Option<String>,
    pub kind: Kind,
    /// For parents: the child that releasing over the parent runs, if the manifest names one.
    pub default_child: Option<usize>,
    pub children: Vec<Slice>,
}

impl Slice {
    fn from_tree(node: &crate::packs::Applicable<'_>) -> Self {
        use crate::packs::Tier;
        let children: Vec<Slice> = node.children.iter().map(Slice::from_tree).collect();
        let default_child = node
            .slice
            .default_child
            .as_ref()
            .and_then(|id| children.iter().position(|c| &c.id == id));
        Self {
            id: node.slice.id.clone(),
            label: node.slice.label.clone(),
            icon: node.slice.icon.clone(),
            kind: match node.slice.mode {
                Tier::Quick => Kind::Run,
                Tier::Choice => Kind::Parent,
                Tier::App => Kind::App,
            },
            default_child,
            children,
        }
    }

    /// A plain slice that runs straight away, e.g. for measurements.
    pub fn run(id: &str, label: &str, icon: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: Some(icon.into()),
            kind: Kind::Run,
            default_child: None,
            children: Vec::new(),
        }
    }
}

/// Builds the wheel's slices from the applicable slice tree. Order is exactly the tree's order
/// (the user's order, then manifest order), so every slot stays put between runs. If more than
/// [`MAIN_RING_MAX`] slices apply, the last slot becomes "More" holding the rest in order.
pub fn build_menu(tree: &[crate::packs::Applicable<'_>]) -> Vec<Slice> {
    let mut slices: Vec<Slice> = tree.iter().map(Slice::from_tree).collect();
    if slices.len() > MAIN_RING_MAX {
        let rest = slices.split_off(MAIN_RING_MAX - 1);
        slices.push(Slice {
            id: MORE_ID.into(),
            label: "More".into(),
            icon: Some("more".into()),
            kind: Kind::Parent,
            default_child: None,
            children: rest,
        });
    }
    slices
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

    use crate::packs::{Applicable, PackSlice, Tier};

    fn pack_slice(id: &str, mode: Tier) -> PackSlice {
        PackSlice {
            id: id.into(),
            label: id.to_uppercase(),
            mode,
            ..PackSlice::default()
        }
    }

    fn leaf(slice: &PackSlice) -> Applicable<'_> {
        Applicable {
            slice,
            children: vec![],
        }
    }

    #[test]
    fn small_menus_keep_every_slot_in_order() {
        let slices: Vec<PackSlice> = (0..MAIN_RING_MAX)
            .map(|i| pack_slice(&format!("p.s{i}"), Tier::Quick))
            .collect();
        let tree: Vec<Applicable> = slices.iter().map(leaf).collect();
        let menu = build_menu(&tree);
        let ids: Vec<&str> = menu.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["p.s0", "p.s1", "p.s2", "p.s3", "p.s4", "p.s5"]);
        assert!(menu.iter().all(|s| s.kind == Kind::Run));
    }

    #[test]
    fn overflow_goes_to_a_more_ring_in_order() {
        let slices: Vec<PackSlice> = (0..9)
            .map(|i| pack_slice(&format!("p.s{i}"), Tier::Quick))
            .collect();
        let tree: Vec<Applicable> = slices.iter().map(leaf).collect();
        let menu = build_menu(&tree);
        assert_eq!(menu.len(), MAIN_RING_MAX);
        let more = &menu[MAIN_RING_MAX - 1];
        assert_eq!(more.id, MORE_ID);
        assert_eq!(more.kind, Kind::Parent);
        assert_eq!(more.default_child, None);
        let rest: Vec<&str> = more.children.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(rest, ["p.s5", "p.s6", "p.s7", "p.s8"]);
        // Same input, same menu: positions never move between runs.
        assert_eq!(build_menu(&tree), menu);
    }

    #[test]
    fn tiers_map_to_kinds_and_defaults_to_indexes() {
        let right = pack_slice("p.rotate.right", Tier::Quick);
        let pick = pack_slice("p.rotate.pick", Tier::App);
        let mut rotate = pack_slice("p.rotate", Tier::Choice);
        rotate.default_child = Some("p.rotate.right".into());
        let tree = [Applicable {
            slice: &rotate,
            children: vec![leaf(&pick), leaf(&right)],
        }];
        let menu = build_menu(&tree);
        assert_eq!(menu[0].kind, Kind::Parent);
        assert_eq!(menu[0].children[0].kind, Kind::App);
        // The default points at the child by id, wherever the user moved it.
        assert_eq!(menu[0].default_child, Some(1));
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
