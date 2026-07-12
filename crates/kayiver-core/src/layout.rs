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
