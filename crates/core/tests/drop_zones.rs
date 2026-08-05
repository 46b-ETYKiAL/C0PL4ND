//! Drop-zone tree-edit resolution tests.
//!
//! The drop-zone *geometry* (cursor → zone) lives in the app crate's `drag`
//! module (pure, tested there). These tests cover the *core* tree edit that an
//! edge-zone drop triggers: [`Layout::move_leaf`] — detaching a pane and
//! re-attaching it beside the drop target on the requested side.

use c0pl4nd_core::layout::{Axis, Direction, Layout, LeafId, Rect, SplitOutcome};

/// Grow a layout to `n` leaves via the guarded action layer, returning their
/// ids in DFS order after a squarest rebalance.
fn grid(n: usize) -> Layout {
    let mut l = Layout::new();
    while l.leaf_count() < n {
        let f = l.focused;
        // `usize::is_multiple_of` requires Rust 1.87+; this crate's MSRV is 1.80.
        // Suppress the Rust-1.95 clippy lint locally until the MSRV moves up.
        #[allow(clippy::manual_is_multiple_of)]
        let axis = if l.leaf_count() % 2 == 0 {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        assert!(matches!(l.try_split(f, axis), SplitOutcome::Split(_)));
    }
    l.rebalance_squarest();
    l
}

#[test]
fn move_leaf_to_right_edge_places_source_after_target() {
    // 1 | 2  → drag 1 onto 2's RIGHT edge → 2 | 1.
    let mut l = Layout::new();
    let b = match l.try_split(LeafId(0), Axis::Horizontal) {
        SplitOutcome::Split(id) => id,
        o => panic!("{o:?}"),
    };
    let win = Rect::new(0, 0, 800, 600);
    // Move leaf 0 to the right of leaf b.
    assert!(l.move_leaf(LeafId(0), b, Axis::Horizontal, /*before=*/ false));
    let rects = l.cascade(win);
    assert_eq!(rects.len(), 2, "leaf count preserved");
    // Order is now [b, 0] left→right.
    assert_eq!(rects[0].0, b);
    assert_eq!(rects[1].0, LeafId(0));
    // The moved leaf takes focus.
    assert_eq!(l.focused, LeafId(0));
}

#[test]
fn move_leaf_to_left_edge_places_source_before_target() {
    // 1 | 2 → drag 2 onto 1's LEFT edge → 2 | 1.
    let mut l = Layout::new();
    let b = match l.try_split(LeafId(0), Axis::Horizontal) {
        SplitOutcome::Split(id) => id,
        o => panic!("{o:?}"),
    };
    let win = Rect::new(0, 0, 800, 600);
    assert!(l.move_leaf(b, LeafId(0), Axis::Horizontal, /*before=*/ true));
    let rects = l.cascade(win);
    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0].0, b, "moved leaf is now first (leftmost)");
    assert_eq!(rects[1].0, LeafId(0));
}

#[test]
fn move_leaf_changes_axis_when_dropped_on_top_or_bottom() {
    // Side-by-side 0 | b → drag 0 onto b's BOTTOM edge → b stacked over 0.
    let mut l = Layout::new();
    let b = match l.try_split(LeafId(0), Axis::Horizontal) {
        SplitOutcome::Split(id) => id,
        o => panic!("{o:?}"),
    };
    let win = Rect::new(0, 0, 800, 600);
    assert!(l.move_leaf(LeafId(0), b, Axis::Vertical, /*before=*/ false));
    let rects = l.cascade(win);
    assert_eq!(rects.len(), 2);
    // Now vertically stacked: same x, different y, b above 0.
    let r_b = rects.iter().find(|(id, _)| *id == b).unwrap().1;
    let r_0 = rects.iter().find(|(id, _)| *id == LeafId(0)).unwrap().1;
    assert_eq!(r_b.x, r_0.x, "stacked panes share the x column");
    assert!(r_b.y < r_0.y, "b is above 0 after a bottom drop");
}

#[test]
fn move_leaf_in_a_2x2_grid_keeps_all_panes() {
    let mut l = grid(4);
    let win = Rect::new(0, 0, 1000, 800);
    let ids = l.leaves();
    assert_eq!(ids.len(), 4);
    let (src, dst) = (ids[3], ids[0]);
    assert!(l.move_leaf(src, dst, Axis::Horizontal, false));
    let after: std::collections::BTreeSet<_> = l.leaves().into_iter().collect();
    let expected: std::collections::BTreeSet<_> = ids.into_iter().collect();
    assert_eq!(after, expected, "no pane lost or duplicated by the move");
    // Cascade still tiles cleanly.
    for (_, r) in l.cascade(win) {
        assert!(r.w > 0 && r.h > 0);
    }
}

#[test]
fn move_leaf_noops_on_self_or_missing() {
    let mut l = Layout::new();
    let b = match l.try_split(LeafId(0), Axis::Horizontal) {
        SplitOutcome::Split(id) => id,
        o => panic!("{o:?}"),
    };
    // Self-drop is a no-op.
    assert!(!l.move_leaf(b, b, Axis::Horizontal, false));
    // Missing source / target is a no-op.
    assert!(!l.move_leaf(LeafId(99), b, Axis::Horizontal, false));
    assert!(!l.move_leaf(b, LeafId(99), Axis::Horizontal, false));
    assert_eq!(l.leaf_count(), 2, "no-ops leave the tree unchanged");
}

#[test]
fn move_then_navigate_is_consistent() {
    // After moving a pane, directional navigation reflects the new geometry.
    let mut l = Layout::new();
    let b = match l.try_split(LeafId(0), Axis::Horizontal) {
        SplitOutcome::Split(id) => id,
        o => panic!("{o:?}"),
    };
    let win = Rect::new(0, 0, 800, 600);
    // 0 | b → move 0 to b's right → b | 0.
    assert!(l.move_leaf(LeafId(0), b, Axis::Horizontal, false));
    // From b, Right reaches 0; from 0, Left reaches b.
    assert_eq!(l.neighbor(b, Direction::Right, win), Some(LeafId(0)));
    assert_eq!(l.neighbor(LeafId(0), Direction::Left, win), Some(b));
}
