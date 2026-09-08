//! Virtual arrangement of machines and the edge ("portal") math.
//!
//! The layout is a list of directed links: `A.right -> B` means "when the
//! cursor pushes through A's right edge, it appears at B's left edge".
//! Every link is implicitly bidirectional: `B.left -> A` is derived.
//!
//! Positions along an edge are expressed as a ratio in `0..=1` over the
//! machine's desktop bounding box, so machines with different resolutions
//! map proportionally.

use serde::{Deserialize, Serialize};

use crate::proto::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
}

impl std::fmt::Display for Edge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Edge::Left => "left",
            Edge::Right => "right",
            Edge::Top => "top",
            Edge::Bottom => "bottom",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    pub edge: Edge,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    #[serde(default)]
    pub links: Vec<Link>,
}

impl Layout {
    /// Where does `machine` end up when its cursor pushes through `edge`?
    /// Returns the target machine and the edge of the *target* through which
    /// the cursor enters.
    pub fn target(&self, machine: &str, edge: Edge) -> Option<(&str, Edge)> {
        for l in &self.links {
            if l.from == machine && l.edge == edge {
                return Some((&l.to, edge.opposite()));
            }
            if l.to == machine && l.edge.opposite() == edge {
                return Some((&l.from, l.edge));
            }
        }
        None
    }

    /// All edges of `machine` that lead somewhere.
    pub fn portals(&self, machine: &str) -> Vec<Edge> {
        let mut edges = Vec::new();
        for e in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            if self.target(machine, e).is_some() && !edges.contains(&e) {
                edges.push(e);
            }
        }
        edges
    }
}

/// Ratio (0..=1) of a point along a given edge of `bounds`.
pub fn ratio_on_edge(bounds: Rect, edge: Edge, x: i32, y: i32) -> f32 {
    let r = match edge {
        Edge::Left | Edge::Right => (y - bounds.y) as f32 / bounds.h.max(1) as f32,
        Edge::Top | Edge::Bottom => (x - bounds.x) as f32 / bounds.w.max(1) as f32,
    };
    r.clamp(0.0, 1.0)
}

/// Point just inside `bounds` on `edge` at `ratio`. `inset` pixels keep the
/// cursor off the exact edge so the arrival does not instantly re-trigger the
/// portal in the other direction.
pub fn point_on_edge(bounds: Rect, edge: Edge, ratio: f32, inset: i32) -> (i32, i32) {
    let ratio = ratio.clamp(0.0, 1.0);
    let along_y = bounds.y + (ratio * bounds.h as f32) as i32;
    let along_x = bounds.x + (ratio * bounds.w as f32) as i32;
    let (x, y) = match edge {
        Edge::Left => (bounds.x + inset, along_y),
        Edge::Right => (bounds.right() - 1 - inset, along_y),
        Edge::Top => (along_x, bounds.y + inset),
        Edge::Bottom => (along_x, bounds.bottom() - 1 - inset),
    };
    (
        x.clamp(bounds.x, bounds.right() - 1),
        y.clamp(bounds.y, bounds.bottom() - 1),
    )
}

/// Is (x, y) inside `r`?
pub fn point_in(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && x < r.right() && y >= r.y && y < r.bottom()
}

/// The cursor is inside the shared monitor `b` (which this machine must not
/// show right now). Return the point just past `b` in the direction of travel
/// `(dx, dy)`, so the cursor *skips over* the monitor onto whatever is beyond —
/// as if that screen weren't there. If motion is ambiguous, pop out the nearest
/// edge.
pub fn skip_out(b: Rect, x: i32, y: i32, dx: i32, dy: i32) -> (i32, i32) {
    if dx == 0 && dy == 0 {
        // No direction: leave via the nearest edge.
        let dl = x - b.x;
        let dr = b.right() - x;
        let dt = y - b.y;
        let db = b.bottom() - y;
        let m = dl.min(dr).min(dt).min(db);
        return if m == dl {
            (b.x - 1, y)
        } else if m == dr {
            (b.right(), y)
        } else if m == dt {
            (x, b.y - 1)
        } else {
            (x, b.bottom())
        };
    }
    if dx.abs() >= dy.abs() {
        if dx >= 0 { (b.right(), y) } else { (b.x - 1, y) }
    } else if dy >= 0 {
        (x, b.bottom())
    } else {
        (x, b.y - 1)
    }
}

/// True if (x, y) is touching `edge` of `bounds` (cursor clamped at boundary).
pub fn touches_edge(bounds: Rect, edge: Edge, x: i32, y: i32) -> bool {
    match edge {
        Edge::Left => x <= bounds.x,
        Edge::Right => x >= bounds.right() - 1,
        Edge::Top => y <= bounds.y,
        Edge::Bottom => y >= bounds.bottom() - 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Layout {
        Layout {
            links: vec![Link {
                from: "mac".into(),
                edge: Edge::Right,
                to: "win".into(),
            }],
        }
    }

    #[test]
    fn forward_and_reverse_links() {
        let l = layout();
        assert_eq!(l.target("mac", Edge::Right), Some(("win", Edge::Left)));
        assert_eq!(l.target("win", Edge::Left), Some(("mac", Edge::Right)));
        assert_eq!(l.target("mac", Edge::Left), None);
        assert_eq!(l.target("win", Edge::Right), None);
    }

    #[test]
    fn portals_listed() {
        let l = layout();
        assert_eq!(l.portals("mac"), vec![Edge::Right]);
        assert_eq!(l.portals("win"), vec![Edge::Left]);
    }

    #[test]
    fn edge_ratio_math() {
        let b = Rect { x: 0, y: 0, w: 1000, h: 500 };
        assert_eq!(ratio_on_edge(b, Edge::Right, 999, 250), 0.5);
        let (x, y) = point_on_edge(b, Edge::Left, 0.5, 2);
        assert_eq!((x, y), (2, 250));
        assert!(touches_edge(b, Edge::Right, 999, 100));
        assert!(!touches_edge(b, Edge::Right, 998, 100));
    }

    #[test]
    fn negative_origin_bounds() {
        // Secondary monitor left of primary: Windows-style negative coords.
        let b = Rect { x: -1920, y: 0, w: 3840, h: 1080 };
        assert!(touches_edge(b, Edge::Left, -1920, 500));
        let (x, _) = point_on_edge(b, Edge::Right, 0.0, 1);
        assert_eq!(x, 1918);
    }
}

/// The position each `current` monitor should take to realise `desired`:
/// desired rects are matched to current monitors by size (an ambiguous size
/// falls back to order), then translated so the current PRIMARY (the monitor
/// at 0,0 — Windows pins it there) stays at 0,0. `None` when the sets don't
/// match or nothing would move.
pub fn arranged(current: &[Rect], desired: &[Rect]) -> Option<Vec<Rect>> {
    if current.is_empty() || current.len() != desired.len() {
        return None;
    }
    let mut used = vec![false; desired.len()];
    let mut out = Vec::with_capacity(current.len());
    for c in current {
        let i = (0..desired.len()).find(|&i| !used[i] && desired[i].w == c.w && desired[i].h == c.h)?;
        used[i] = true;
        out.push(Rect { x: desired[i].x, y: desired[i].y, w: c.w, h: c.h });
    }
    let prim = current.iter().position(|c| c.x == 0 && c.y == 0).unwrap_or(0);
    let (dx, dy) = (out[prim].x, out[prim].y);
    for r in &mut out {
        r.x -= dx;
        r.y -= dy;
    }
    if out == current {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod arranged_tests {
    use super::*;
    #[test]
    fn matches_by_size_and_pins_primary() {
        let cur = [Rect { x: 0, y: 0, w: 1920, h: 1080 }, Rect { x: 1920, y: 0, w: 1920, h: 1080 }];
        let want = [Rect { x: 0, y: 0, w: 1920, h: 1080 }, Rect { x: 200, y: -1080, w: 1920, h: 1080 }];
        assert_eq!(arranged(&cur, &want).unwrap()[1], want[1]);
        // desired given with the primary off-origin: re-pinned
        let want2 = [Rect { x: 100, y: 100, w: 1920, h: 1080 }, Rect { x: 300, y: -980, w: 1920, h: 1080 }];
        assert_eq!(arranged(&cur, &want2).unwrap()[1], want[1]);
        assert!(arranged(&cur, &cur).is_none());
        assert!(arranged(&cur, &want[..1]).is_none());
        let other = [Rect { x: 0, y: 0, w: 2560, h: 1440 }, Rect { x: 0, y: -1080, w: 1920, h: 1080 }];
        assert!(arranged(&cur, &other).is_none(), "a size that is not on this desk matches nothing");
    }
}

/// Where a window that is sitting on a monitor this machine is NOT showing
/// (the shared panel handed to the peer) should be moved to, so it is not
/// stranded on an invisible screen. `win` is the window rect, `blocked` the
/// hidden monitor, `target` a monitor that IS showing us. `None` when the
/// window is not on the hidden monitor, or is already fine.
///
/// The window keeps its size and its relative spot on the monitor, then is
/// clamped so it lands fully inside `target` — a window pushed half off the
/// screen (title bar out of reach) would be no better than a hidden one.
pub fn relocate_off(win: Rect, blocked: Rect, target: Rect) -> Option<Rect> {
    // Judge by the window's CENTRE: a window merely overlapping the seam is
    // still usable, and dragging every straddling window would fight the user.
    let (cx, cy) = (win.x + win.w / 2, win.y + win.h / 2);
    if !point_in(blocked, cx, cy) {
        return None;
    }
    let fx = (win.x - blocked.x) as f32 / blocked.w.max(1) as f32;
    let fy = (win.y - blocked.y) as f32 / blocked.h.max(1) as f32;
    let w = win.w.min(target.w);
    let h = win.h.min(target.h);
    let x = (target.x + (fx * target.w as f32) as i32).clamp(target.x, target.right() - w);
    let y = (target.y + (fy * target.h as f32) as i32).clamp(target.y, target.bottom() - h);
    Some(Rect { x, y, w: win.w, h: win.h })
}

#[cfg(test)]
mod relocate_tests {
    use super::*;
    const BLOCKED: Rect = Rect { x: -320, y: 1080, w: 2560, h: 1440 };
    const TARGET: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

    #[test]
    fn moves_a_window_centred_on_the_hidden_monitor() {
        let win = Rect { x: 700, y: 1700, w: 800, h: 600 };
        let r = relocate_off(win, BLOCKED, TARGET).expect("centre is on the hidden monitor");
        assert_eq!((r.w, r.h), (800, 600), "size is preserved");
        assert!(r.x >= TARGET.x && r.x + r.w <= TARGET.right(), "lands fully on the target: {r:?}");
        assert!(r.y >= TARGET.y && r.y + r.h <= TARGET.bottom(), "lands fully on the target: {r:?}");
    }

    #[test]
    fn leaves_windows_that_are_not_on_the_hidden_monitor() {
        let on_target = Rect { x: 100, y: 100, w: 400, h: 300 };
        assert!(relocate_off(on_target, BLOCKED, TARGET).is_none());
    }

    #[test]
    fn a_window_bigger_than_the_target_still_lands_at_its_origin() {
        let huge = Rect { x: 0, y: 1200, w: 2400, h: 1300 };
        let r = relocate_off(huge, BLOCKED, TARGET).unwrap();
        assert_eq!((r.x, r.y), (TARGET.x, TARGET.y), "clamped to the target origin, not pushed off");
    }

    #[test]
    fn keeps_the_relative_spot_so_a_corner_window_stays_a_corner_window() {
        let bottom_right = Rect { x: 1900, y: 2300, w: 300, h: 200 };
        let r = relocate_off(bottom_right, BLOCKED, TARGET).unwrap();
        assert!(r.x > TARGET.x + TARGET.w / 2, "stays on the right half: {r:?}");
        assert!(r.y > TARGET.y + TARGET.h / 2, "stays on the lower half: {r:?}");
    }
}

/// Where the segment `from`→`to` (ending inside `b`) enters the rect, as
/// fractions across it — the entered edge pinned to exactly 0.0 / 1.0 and the
/// crossing point preserved along it. Every side the segment could have
/// crossed is intersected and the first hit along the travel (smallest t)
/// wins, so a diagonal entry near a corner still resolves to the side that was
/// physically hit first. When there is no crossing to measure (`from` already
/// inside, or no motion — e.g. the block appeared under a resting cursor),
/// falls back to pinning the nearest side of the caught position.
pub fn entry_on_rect(b: Rect, from: (i32, i32), to: (i32, i32)) -> (f32, f32) {
    let (px, py) = (from.0 as f32, from.1 as f32);
    let (dx, dy) = (to.0 as f32 - px, to.1 as f32 - py);
    let w = b.w.max(1) as f32;
    let h = b.h.max(1) as f32;
    let (x0, y0) = (b.x as f32, b.y as f32);
    let (x1, y1) = ((b.x + b.w) as f32, (b.y + b.h) as f32);

    let mut best: Option<(f32, (f32, f32))> = None;
    let mut consider = |t: f32, fx: f32, fy: f32| {
        // A candidate is a real entry only if the crossing point sits on the
        // rect's side (small tolerance for float rounding at corners).
        let on_side = (-0.01..=1.01).contains(&fx) && (-0.01..=1.01).contains(&fy);
        if (0.0..=1.0).contains(&t) && on_side && best.map_or(true, |(bt, _)| t < bt) {
            best = Some((t, (fx.clamp(0.0, 1.0), fy.clamp(0.0, 1.0))));
        }
    };
    if dx > 0.0 && px < x0 {
        let t = (x0 - px) / dx;
        consider(t, 0.0, (py + t * dy - y0) / h);
    }
    if dx < 0.0 && px >= x1 {
        let t = (x1 - px) / dx;
        consider(t, 1.0, (py + t * dy - y0) / h);
    }
    if dy > 0.0 && py < y0 {
        let t = (y0 - py) / dy;
        consider(t, (px + t * dx - x0) / w, 0.0);
    }
    if dy < 0.0 && py >= y1 {
        let t = (y1 - py) / dy;
        consider(t, (px + t * dx - x0) / w, 1.0);
    }
    if let Some((_, f)) = best {
        return f;
    }
    let fx = ((to.0 - b.x) as f32 / w).clamp(0.0, 1.0);
    let fy = ((to.1 - b.y) as f32 / h).clamp(0.0, 1.0);
    let (dl, dr, dt, db) = (fx, 1.0 - fx, fy, 1.0 - fy);
    let m = dl.min(dr).min(dt).min(db);
    if m == dl {
        (0.0, fy)
    } else if m == dr {
        (1.0, fy)
    } else if m == dt {
        (fx, 0.0)
    } else {
        (fx, 1.0)
    }
}

#[cfg(test)]
mod entry_tests {
    use super::*;
    // A tall panel that does NOT start at the origin, so an off-by-origin bug
    // cannot hide behind zeros.
    const PANEL: Rect = Rect { x: -320, y: 1080, w: 2560, h: 1440 };

    #[test]
    fn a_fast_move_deep_into_the_panel_still_reports_the_edge_it_crossed() {
        // Came down from the monitor above and overshot far past the seam:
        // the hand-back must use where it ENTERED (the top edge), not how far
        // the pointer happened to travel in one report.
        let (fx, fy) = entry_on_rect(PANEL, (900, 1000), (900, 2400));
        assert_eq!(fy, 0.0, "entered through the TOP edge");
        assert!((fx - (1220.0 / 2560.0)).abs() < 0.01, "kept its place along that edge, got {fx}");
    }

    #[test]
    fn a_slow_step_across_the_seam_agrees_with_the_fast_one() {
        let slow = entry_on_rect(PANEL, (900, 1070), (900, 1090));
        let fast = entry_on_rect(PANEL, (900, 1000), (900, 2400));
        assert_eq!(slow, fast, "the same crossing must not depend on mouse speed");
    }

    #[test]
    fn a_side_entry_pins_that_side() {
        let (fx, fy) = entry_on_rect(PANEL, (-900, 1800), (500, 1800));
        assert_eq!(fx, 0.0, "entered through the LEFT edge");
        assert!((fy - 0.5).abs() < 0.01, "at half height, got {fy}");
    }

    #[test]
    fn no_motion_falls_back_to_the_nearest_side() {
        // The block appeared under a resting cursor: nothing was crossed, so
        // pin the side it is closest to rather than inventing a travel.
        let (_, fy) = entry_on_rect(PANEL, (900, 1120), (900, 1120));
        assert_eq!(fy, 0.0, "nearest side is the top");
    }
}
