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

/// Width and height of the native wheel window in logical pixels: room for the main ring, a
/// sub-ring of children outside it, labels and the shadow. The same for every style, so a
/// sub-ring never needs the window to grow.
pub fn window_size_for(_style: crate::theme::Style) -> f64 {
    740.0
}

/// How far from the centre the wheel reaches once a sub-ring is open (buttons, not labels), in
/// logical pixels. The wheel is moved inward so this much always fits on the screen.
pub fn reach(style: crate::theme::Style) -> f64 {
    sub_ring_radius(style) + 40.0
}

/// Radius of the sub-ring of children for each style, in logical pixels.
pub fn sub_ring_radius(style: crate::theme::Style) -> f64 {
    use crate::theme::Style;
    match style {
        Style::Orbit => 215.0,
        Style::Pie => 220.0,
        Style::Ring => 240.0,
    }
}

/// Beyond this distance from the centre, with a sub-ring open, the cursor points at children.
/// It sits just outside the main ring's buttons or slices.
pub fn main_ring_reach(style: crate::theme::Style) -> f64 {
    use crate::theme::Style;
    match style {
        Style::Orbit => 170.0,
        Style::Pie => 170.0,
        Style::Ring => 168.0,
    }
}

/// Angle between neighbouring children on a sub-ring: one button width plus a gap, measured
/// along the sub-ring.
pub fn child_step(style: crate::theme::Style) -> f64 {
    72.0 / sub_ring_radius(style)
}

/// Centre angle of child `index` of `count`, fanned out symmetrically around its parent's angle.
pub fn child_angle(parent_angle: f64, index: usize, count: usize, step: f64) -> f64 {
    parent_angle + (index as f64 - (count as f64 - 1.0) / 2.0) * step
}

/// Which child of a fan the cursor points at, by angle. Only within half a step beyond the
/// outermost children, so pointing far to the side picks nothing.
pub fn hit_child(dx: f64, dy: f64, parent_angle: f64, count: usize, step: f64) -> Option<usize> {
    let angle = dy.atan2(dx);
    (0..count)
        .map(|i| {
            let diff = (angle - child_angle(parent_angle, i, count, step) + std::f64::consts::PI)
                .rem_euclid(std::f64::consts::TAU)
                - std::f64::consts::PI;
            (i, diff.abs())
        })
        .filter(|(_, diff)| *diff <= step * 0.55)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(i, _)| i)
}

/// What the wheel shows: the hovered main slice, which slice's sub-ring is open, and the
/// hovered child in it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WheelState {
    pub hovered: Option<usize>,
    pub open: Option<usize>,
    pub child: Option<usize>,
}

/// Turns cursor movement into wheel state, including opening a sub-ring after a short hover and
/// closing it again. Pure logic (time is passed in), so it is unit-tested without a screen.
///
/// - Hovering a parent for `delay` opens its children as a sub-ring outside it.
/// - With a sub-ring open, pointing beyond the main ring picks a child; pointing back at the
///   main ring keeps the sub-ring until another main slice has been hovered for `delay`.
/// - Entering the centre hub closes one level; [`Navigator::back`] (Esc, right-click) does too.
/// - Releasing: a hovered child runs; a hovered parent runs its default child, if any; the hub
///   cancels.
pub struct Navigator {
    style: crate::theme::Style,
    delay: std::time::Duration,
    state: WheelState,
    dwell: Option<(usize, std::time::Instant)>,
    in_hub: bool,
}

impl Navigator {
    pub fn new(style: crate::theme::Style, delay: std::time::Duration) -> Self {
        Self {
            style,
            delay,
            state: WheelState::default(),
            dwell: None,
            in_hub: true,
        }
    }

    pub fn state(&self) -> WheelState {
        self.state
    }

    /// Updates the state from the cursor's offset from the centre, in logical pixels.
    pub fn track(
        &mut self,
        menu: &[Slice],
        dx: f64,
        dy: f64,
        now: std::time::Instant,
    ) -> WheelState {
        let distance = dx.hypot(dy);
        let cancel = cancel_radius(self.style);
        if distance < cancel {
            // Entering the hub closes one level; staying in it does nothing more.
            if !self.in_hub {
                self.in_hub = true;
                self.state.open = None;
            }
            self.state.hovered = None;
            self.state.child = None;
            self.dwell = None;
            return self.state;
        }
        self.in_hub = false;

        if let Some(parent) = self.state.open {
            if distance >= main_ring_reach(self.style) {
                let kids = menu[parent].children.len();
                let angle = slice_angle(parent, menu.len());
                self.state.hovered = Some(parent);
                self.state.child = hit_child(dx, dy, angle, kids, child_step(self.style));
                self.dwell = None;
                return self.state;
            }
        }

        let hit = hit_test(dx, dy, menu.len(), cancel);
        self.state.hovered = hit;
        self.state.child = None;
        match hit {
            Some(i) if Some(i) == self.state.open => self.dwell = None,
            Some(i) => match self.dwell {
                Some((j, since)) if j == i => {
                    if now.duration_since(since) >= self.delay {
                        // Opens this parent's children, or closes another parent's.
                        self.state.open = (menu[i].kind == Kind::Parent).then_some(i);
                        self.dwell = None;
                    }
                }
                _ => {
                    self.dwell = Some((i, now));
                    if self.delay.is_zero() && menu[i].kind == Kind::Parent {
                        self.state.open = Some(i);
                    }
                }
            },
            None => self.dwell = None,
        }
        self.state
    }

    /// Closes the open sub-ring (Esc or right-click). Returns false when none is open, in which
    /// case the caller cancels the whole wheel.
    pub fn back(&mut self) -> bool {
        if self.state.open.take().is_some() {
            self.state.child = None;
            self.dwell = None;
            true
        } else {
            false
        }
    }

    /// The slice that releasing now would act on, if any.
    pub fn picked<'a>(&self, menu: &'a [Slice]) -> Option<&'a Slice> {
        let main = &menu[self.state.hovered?];
        if let Some(child) = self.state.child {
            return main.children.get(child);
        }
        match main.kind {
            Kind::Parent => main.default_child.and_then(|d| main.children.get(d)),
            _ => Some(main),
        }
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

    mod navigation {
        use std::time::{Duration, Instant};

        use super::super::*;
        use crate::theme::Style;

        const DELAY: Duration = Duration::from_millis(200);

        /// Merge (runs), Rotate (parent: right / left / 180, default right), Remove (app).
        fn menu() -> Vec<Slice> {
            let child = |id: &str| Slice::run(id, id, "rotate");
            let mut rotate = Slice::run("rotate", "Rotate", "rotate");
            rotate.kind = Kind::Parent;
            rotate.children = vec![child("right"), child("left"), child("180")];
            rotate.default_child = Some(0);
            let mut remove = Slice::run("remove", "Remove", "remove-pages");
            remove.kind = Kind::App;
            vec![Slice::run("merge", "Merge", "merge"), rotate, remove]
        }

        /// Cursor offset pointing at slice `index` of `count`, `distance` px out.
        fn toward(index: usize, count: usize, distance: f64) -> (f64, f64) {
            let a = slice_angle(index, count);
            (distance * a.cos(), distance * a.sin())
        }

        fn toward_child(child: usize, kids: usize, distance: f64) -> (f64, f64) {
            let a = child_angle(slice_angle(1, 3), child, kids, child_step(Style::Orbit));
            (distance * a.cos(), distance * a.sin())
        }

        #[test]
        fn hovering_a_parent_opens_its_sub_ring_after_the_delay() {
            let menu = menu();
            let mut nav = Navigator::new(Style::Orbit, DELAY);
            let t0 = Instant::now();
            let (x, y) = toward(1, 3, 118.0);
            assert_eq!(nav.track(&menu, x, y, t0).open, None);
            assert_eq!(nav.track(&menu, x, y, t0 + DELAY / 2).open, None);
            let state = nav.track(&menu, x, y, t0 + DELAY);
            assert_eq!((state.hovered, state.open), (Some(1), Some(1)));

            // Out to the second child.
            let (x, y) = toward_child(1, 3, 215.0);
            let state = nav.track(&menu, x, y, t0 + DELAY * 2);
            assert_eq!(state.child, Some(1));
            assert_eq!(nav.picked(&menu).unwrap().id, "left");
        }

        #[test]
        fn passing_over_a_parent_quickly_does_not_open_it() {
            let menu = menu();
            let mut nav = Navigator::new(Style::Orbit, DELAY);
            let t0 = Instant::now();
            let (x, y) = toward(1, 3, 118.0);
            nav.track(&menu, x, y, t0);
            let (x, y) = toward(2, 3, 118.0);
            let state = nav.track(&menu, x, y, t0 + DELAY / 2);
            assert_eq!(
                state,
                WheelState {
                    hovered: Some(2),
                    open: None,
                    child: None
                }
            );
            assert_eq!(nav.picked(&menu).unwrap().kind, Kind::App);
        }

        #[test]
        fn releasing_over_a_parent_runs_its_default_child() {
            let menu = menu();
            let mut nav = Navigator::new(Style::Orbit, DELAY);
            let (x, y) = toward(1, 3, 118.0);
            nav.track(&menu, x, y, Instant::now());
            assert_eq!(nav.picked(&menu).unwrap().id, "right");

            // A parent without a default does nothing.
            let mut menu = menu.clone();
            menu[1].default_child = None;
            assert!(nav.picked(&menu).is_none());
        }

        #[test]
        fn the_hub_back_and_other_slices_close_one_level() {
            let menu = menu();
            let mut nav = Navigator::new(Style::Orbit, DELAY);
            let t0 = Instant::now();
            let (x, y) = toward(1, 3, 118.0);
            nav.track(&menu, x, y, t0);
            nav.track(&menu, x, y, t0 + DELAY);
            assert_eq!(nav.state().open, Some(1));

            // Back to the hub: closed, and releasing there cancels.
            let state = nav.track(&menu, 3.0, 4.0, t0 + DELAY * 2);
            assert_eq!(state, WheelState::default());
            assert!(nav.picked(&menu).is_none());

            // Open again, then Esc / right-click closes it; a second back has nothing to close.
            nav.track(&menu, x, y, t0 + DELAY * 3);
            nav.track(&menu, x, y, t0 + DELAY * 4);
            assert_eq!(nav.state().open, Some(1));
            assert!(nav.back());
            assert_eq!(nav.state().open, None);
            assert!(!nav.back());

            // Open again; resting on another main slice for the delay closes it.
            nav.track(&menu, x, y, t0 + DELAY * 5);
            nav.track(&menu, x, y, t0 + DELAY * 6);
            let (mx, my) = toward(0, 3, 118.0);
            nav.track(&menu, mx, my, t0 + DELAY * 7);
            assert_eq!(nav.state().open, Some(1));
            nav.track(&menu, mx, my, t0 + DELAY * 8);
            assert_eq!(nav.state().open, None);
            assert_eq!(nav.picked(&menu).unwrap().id, "merge");
        }

        #[test]
        fn pointing_beside_the_fan_picks_no_child() {
            let menu = menu();
            let mut nav = Navigator::new(Style::Orbit, DELAY);
            let t0 = Instant::now();
            let (x, y) = toward(1, 3, 118.0);
            nav.track(&menu, x, y, t0);
            nav.track(&menu, x, y, t0 + DELAY);
            // Far out but pointing at Merge's side of the wheel: no child.
            let (x, y) = toward(0, 3, 230.0);
            let state = nav.track(&menu, x, y, t0 + DELAY * 2);
            assert_eq!((state.open, state.child), (Some(1), None));
        }

        #[test]
        fn child_fan_is_symmetric_around_the_parent() {
            let step = child_step(Style::Orbit);
            assert!((child_angle(1.0, 1, 3, step) - 1.0).abs() < 1e-12);
            assert!(
                (child_angle(1.0, 0, 2, step) - 1.0 + (child_angle(1.0, 1, 2, step) - 1.0)).abs()
                    < 1e-12
            );
            for i in 0..5 {
                let a = child_angle(0.3, i, 5, step);
                assert_eq!(hit_child(a.cos(), a.sin(), 0.3, 5, step), Some(i));
            }
        }
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
