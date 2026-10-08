//! Deterministic cursor transport. Geometry, not OS cursor polling, owns crossings.
use crate::{layout::Edge, proto::Rect};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Surface {
    pub id: String,
    pub machine: String,
    pub rect: Rect,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Seam {
    pub from: String,
    pub edge: Edge,
    pub start: f64,
    pub end: f64,
    pub to: String,
    pub entry: Edge,
    pub target_start: f64,
    pub target_end: f64,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    pub revision: u64,
    pub surfaces: Vec<Surface>,
    pub seams: Vec<Seam>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Location {
    pub surface: String,
    pub point: Point,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub location: Location,
    pub path: Vec<String>,
    pub wall: bool,
}

impl Topology {
    pub fn surface(&self, id: &str) -> Option<&Surface> {
        self.surfaces.iter().find(|s| s.id == id)
    }
    pub fn locate(&self, machine: &str, p: Point) -> Option<Location> {
        let mut candidates = self
            .surfaces
            .iter()
            .filter(|s| s.machine == machine && contains(s.rect, p));
        let first = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        Some(Location {
            surface: first.id.clone(),
            point: p,
        })
    }
    /// Build exact shared edge segments. Ambiguous overlaps are walls.
    pub fn connect(
        &mut self,
        from: &str,
        edge: Edge,
        start: f64,
        end: f64,
        to: &str,
        entry: Edge,
        target_start: f64,
        target_end: f64,
    ) {
        if from == to
            || entry != edge.opposite()
            || !start.is_finite()
            || !end.is_finite()
            || !target_start.is_finite()
            || !target_end.is_finite()
            || start >= end
            || target_start >= target_end
            || self.surface(from).is_none()
            || self.surface(to).is_none()
        {
            return;
        }
        self.seams.push(Seam {
            from: from.into(),
            edge,
            start,
            end,
            to: to.into(),
            entry,
            target_start,
            target_end,
        });
    }
    pub fn connect_neighbours(&mut self, a: &str, b: &str) {
        let (Some(ar), Some(br)) = (
            self.surface(a).map(|s| s.rect),
            self.surface(b).map(|s| s.rect),
        ) else {
            return;
        };
        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let (touch, lo, hi) = match edge {
                Edge::Left => (
                    ar.x == br.right(),
                    ar.y.max(br.y),
                    ar.bottom().min(br.bottom()),
                ),
                Edge::Right => (
                    ar.right() == br.x,
                    ar.y.max(br.y),
                    ar.bottom().min(br.bottom()),
                ),
                Edge::Top => (
                    ar.y == br.bottom(),
                    ar.x.max(br.x),
                    ar.right().min(br.right()),
                ),
                Edge::Bottom => (
                    ar.bottom() == br.y,
                    ar.x.max(br.x),
                    ar.right().min(br.right()),
                ),
            };
            if touch && lo < hi {
                self.connect(
                    a,
                    edge,
                    lo as f64,
                    hi as f64,
                    b,
                    edge.opposite(),
                    lo as f64,
                    hi as f64,
                );
            }
        }
    }
    /// Each native report stays ordered. Never sum direction reversals before this call.
    pub fn advance(&self, at: &Location, delta: Point) -> Option<Step> {
        if !at.point.x.is_finite()
            || !at.point.y.is_finite()
            || !delta.x.is_finite()
            || !delta.y.is_finite()
        {
            return None;
        }
        let mut at = at.clone();
        let mut d = delta;
        let mut path = vec![at.surface.clone()];
        let mut wall = false;
        for _ in 0..256 {
            let rect = self.surface(&at.surface)?.rect;
            if rect.w <= 0
                || rect.h <= 0
                || rect.x.checked_add(rect.w).is_none()
                || rect.y.checked_add(rect.h).is_none()
            {
                return None;
            }
            at.point = interior(rect, at.point);
            let mut t = 1.0;
            let mut hit = None;
            for (edge, distance, velocity) in [
                (Edge::Left, rect.x as f64 - at.point.x, d.x),
                (Edge::Right, rect.right() as f64 - at.point.x, d.x),
                (Edge::Top, rect.y as f64 - at.point.y, d.y),
                (Edge::Bottom, rect.bottom() as f64 - at.point.y, d.y),
            ] {
                let outward = match edge {
                    Edge::Left | Edge::Top => velocity < 0.0,
                    _ => velocity > 0.0,
                };
                if outward {
                    let candidate = distance / velocity;
                    if candidate >= 0.0 && candidate <= t {
                        t = candidate;
                        hit = Some(edge);
                    }
                }
            }
            at.point.x += d.x * t;
            at.point.y += d.y * t;
            let Some(edge) = hit else {
                return Some(Step {
                    location: at,
                    path,
                    wall,
                });
            };
            d.x *= 1.0 - t;
            d.y *= 1.0 - t;
            let along = match edge {
                Edge::Left | Edge::Right => at.point.y,
                _ => at.point.x,
            };
            let candidates: Vec<_> = self
                .seams
                .iter()
                .filter(|s| {
                    s.from == at.surface && s.edge == edge && along >= s.start && along < s.end
                })
                .collect();
            if candidates.len() == 1 {
                let s = candidates[0];
                let target = self.surface(&s.to)?.rect;
                let ratio = (along - s.start) / (s.end - s.start);
                let a = s.target_start + ratio * (s.target_end - s.target_start);
                let scale = (s.target_end - s.target_start) / (s.end - s.start);
                // Tangential and normal components use the same seam scale, preserving angle.
                d.x *= scale;
                d.y *= scale;
                at = Location {
                    surface: s.to.clone(),
                    point: match s.entry {
                        Edge::Left => Point::new(target.x as f64, a),
                        Edge::Right => Point::new(target.right() as f64, a),
                        Edge::Top => Point::new(a, target.y as f64),
                        Edge::Bottom => Point::new(a, target.bottom() as f64),
                    },
                };
                path.push(at.surface.clone());
            } else {
                wall = true;
                match edge {
                    Edge::Left | Edge::Right => d.x = 0.0,
                    _ => d.y = 0.0,
                }
                at.point = interior(rect, at.point);
            }
            if d.x == 0.0 && d.y == 0.0 {
                at.point = interior(self.surface(&at.surface)?.rect, at.point);
                return Some(Step {
                    location: at,
                    path,
                    wall,
                });
            }
        }
        None // invalid cyclic topology: caller must restore local control
    }
}
fn contains(r: Rect, p: Point) -> bool {
    p.x >= r.x as f64 && p.x < r.right() as f64 && p.y >= r.y as f64 && p.y < r.bottom() as f64
}
fn interior(r: Rect, p: Point) -> Point {
    Point::new(
        p.x.clamp(r.x as f64, r.right() as f64),
        p.y.clamp(r.y as f64, r.bottom() as f64),
    )
}

/// Receiver cursor frames are ordered within a connection/control generation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub session: u64,
    pub generation: u64,
    pub sequence: u64,
    pub revision: u64,
}
#[derive(Clone, Debug, Default)]
pub struct Receiver {
    pub last: Option<Stamp>,
}
impl Receiver {
    pub fn accept(&mut self, s: Stamp, session: u64, revision: u64) -> bool {
        if s.session != session || s.revision != revision {
            return false;
        }
        if let Some(last) = self.last {
            if last.session == s.session
                && (s.generation < last.generation
                    || (s.generation == last.generation && s.sequence <= last.sequence))
            {
                return false;
            }
        }
        self.last = Some(s);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn desk() -> Topology {
        let mut t = Topology {
            revision: 1,
            surfaces: vec![
                Surface {
                    id: "A".into(),
                    machine: "mac".into(),
                    rect: Rect {
                        x: 0,
                        y: 0,
                        w: 2560,
                        h: 1440,
                    },
                },
                Surface {
                    id: "D".into(),
                    machine: "win".into(),
                    rect: Rect {
                        x: 2560,
                        y: 0,
                        w: 2560,
                        h: 1440,
                    },
                },
            ],
            seams: vec![],
        };
        t.connect_neighbours("A", "D");
        t.connect_neighbours("D", "A");
        t
    }
    #[test]
    fn flick_and_immediate_reverse_preserve_distance() {
        let t = desk();
        let a = t.locate("mac", Point::new(2550.0, 700.0)).unwrap();
        let d = t.advance(&a, Point::new(100.0, 10.0)).unwrap();
        assert_eq!(d.path, ["A", "D"]);
        assert!((d.location.point.x - 2650.0).abs() < 1e-5);
        let a = t.advance(&d.location, Point::new(-100.0, -10.0)).unwrap();
        assert_eq!(a.path, ["D", "A"]);
        assert!((a.location.point.x - 2550.0).abs() < 1e-5);
    }
    #[test]
    fn walls_do_not_accumulate_debt() {
        let t = desk();
        let a = t.locate("mac", Point::new(1.0, 700.0)).unwrap();
        let a = t.advance(&a, Point::new(-10000.0, 30.0)).unwrap();
        assert!(a.wall);
        let a = t.advance(&a.location, Point::new(1.0, 0.0)).unwrap();
        assert!((a.location.point.x - 1.0).abs() < 1e-5);
    }
    #[test]
    fn stale_and_duplicate_frames_are_rejected() {
        let mut r = Receiver::default();
        let s = Stamp {
            session: 7,
            generation: 2,
            sequence: 10,
            revision: 1,
        };
        assert!(r.accept(s, 7, 1));
        assert!(!r.accept(s, 7, 1));
        assert!(!r.accept(
            Stamp {
                generation: 1,
                sequence: 20,
                ..s
            },
            7,
            1
        ));
        assert!(!r.accept(Stamp { session: 8, ..s }, 7, 1));
        assert!(!r.accept(Stamp { revision: 2, ..s }, 7, 1));
    }
    #[test]
    fn generated_paths_roundtrip_without_losing_fractional_motion() {
        let t = desk();
        let mut seed = 77u64;
        for _ in 0..10000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let dx = (seed % 4000) as f64 / 10.0;
            let a = t.locate("mac", Point::new(2500.125, 700.375)).unwrap();
            let b = t.advance(&a, Point::new(dx, 0.0)).unwrap();
            let a2 = t.advance(&b.location, Point::new(-dx, 0.0)).unwrap();
            assert_eq!(a2.location.surface, "A");
            assert!((a2.location.point.x - a.point.x).abs() < 1e-4);
        }
    }
    #[test]
    fn overlapping_seams_are_walls() {
        let mut t = desk();
        t.connect_neighbours("A", "D");
        let a = t.locate("mac", Point::new(2550.0, 700.0)).unwrap();
        let b = t.advance(&a, Point::new(100.0, 0.0)).unwrap();
        assert!(b.wall);
        assert_eq!(b.location.surface, "A");
    }
}

/// A desk snapshot includes stable monitor IDs and native desktop coordinates.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Desk {
    pub machine: String,
    pub monitors: Vec<(String, Rect)>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SharedPanel {
    pub a: String,
    pub b: String,
    pub owner: String,
}
impl Topology {
    fn free_segments(&self, machine: &str, edge: Edge) -> Vec<(String, f64, f64)> {
        let mut result = Vec::new();
        for surface in self.surfaces.iter().filter(|s| s.machine == machine) {
            let (lo, hi) = match edge {
                Edge::Left | Edge::Right => (surface.rect.y as f64, surface.rect.bottom() as f64),
                _ => (surface.rect.x as f64, surface.rect.right() as f64),
            };
            let mut free = vec![(lo, hi)];
            for occupied in self
                .seams
                .iter()
                .filter(|s| s.from == surface.id && s.edge == edge)
            {
                free = free
                    .into_iter()
                    .flat_map(|(a, b)| {
                        if occupied.end <= a || occupied.start >= b {
                            vec![(a, b)]
                        } else {
                            [(a, occupied.start.min(b)), (occupied.end.max(a), b)]
                                .into_iter()
                                .filter(|(a, b)| a < b)
                                .collect()
                        }
                    })
                    .collect();
            }
            result.extend(free.into_iter().map(|(a, b)| (surface.id.clone(), a, b)));
        }
        result
    }
    /// Compile shared-panel aliases into one set of visible surfaces. Both drivers
    /// use this same immutable snapshot; labels and enumeration order play no role.
    pub fn compile(
        revision: u64,
        desks: &[Desk],
        panel: Option<&SharedPanel>,
        links: &[(String, Edge, String)],
    ) -> Self {
        let mut all: Vec<Surface> = desks
            .iter()
            .flat_map(|d| {
                d.monitors.iter().map(|(id, rect)| Surface {
                    id: format!("{}:{id}", d.machine),
                    machine: d.machine.clone(),
                    rect: *rect,
                })
            })
            .collect();
        let ids: Vec<_> = all.iter().map(|s| s.id.clone()).collect();
        all.retain(|s| {
            s.rect.w > 0
                && s.rect.h > 0
                && s.rect.x.checked_add(s.rect.w).is_some()
                && s.rect.y.checked_add(s.rect.h).is_some()
                && ids.iter().filter(|id| **id == s.id).count() == 1
        });
        all.sort_by(|a, b| a.id.cmp(&b.id));
        let aliases = panel.and_then(|p| {
            Some((
                all.iter().find(|s| s.id == p.a)?,
                all.iter().find(|s| s.id == p.b)?,
                &p.owner,
            ))
        });
        let hidden = aliases.map(|(a, b, owner)| {
            if a.machine == *owner {
                b.id.as_str()
            } else {
                a.id.as_str()
            }
        });
        let mut t = Topology {
            revision,
            surfaces: all
                .iter()
                .filter(|s| Some(s.id.as_str()) != hidden)
                .cloned()
                .collect(),
            seams: vec![],
        };
        let ids: Vec<_> = t
            .surfaces
            .iter()
            .map(|s| (s.id.clone(), s.machine.clone()))
            .collect();
        for (a, am) in &ids {
            for (b, bm) in &ids {
                if a != b && am == bm {
                    t.connect_neighbours(a, b);
                }
            }
        }
        if let Some((a, b, owner)) = aliases {
            let (visible, blocked) = if a.machine == *owner { (a, b) } else { (b, a) };
            for n in all
                .iter()
                .filter(|s| s.machine == blocked.machine && s.id != blocked.id)
            {
                let mut probe = Topology {
                    revision,
                    surfaces: vec![n.clone(), blocked.clone()],
                    seams: vec![],
                };
                probe.connect_neighbours(&n.id, &blocked.id);
                for seam in probe.seams {
                    let (source_origin, source_size, target_origin, target_size) = match seam.edge {
                        Edge::Left | Edge::Right => (
                            blocked.rect.y,
                            blocked.rect.h,
                            visible.rect.y,
                            visible.rect.h,
                        ),
                        _ => (
                            blocked.rect.x,
                            blocked.rect.w,
                            visible.rect.x,
                            visible.rect.w,
                        ),
                    };
                    let map = |v: f64| {
                        target_origin as f64
                            + (v - source_origin as f64) * target_size as f64
                                / source_size.max(1) as f64
                    };
                    let start = map(seam.start);
                    let end = map(seam.end);
                    t.connect(
                        &n.id,
                        seam.edge,
                        seam.start,
                        seam.end,
                        &visible.id,
                        seam.entry,
                        start,
                        end,
                    );
                    t.connect(
                        &visible.id,
                        seam.entry,
                        start,
                        end,
                        &n.id,
                        seam.edge,
                        seam.start,
                        seam.end,
                    );
                }
            }
        }
        // Legacy machine links between shared desktops are superseded by the
        // physical panel graph. Other explicit links pair only unoccupied,
        // unambiguous real edge segments, including non-rectangular notches.
        for (from, edge, to) in links {
            let ambiguous = |machine: &String, edge: Edge, dest: &String| {
                links.iter().any(|(a, e, b)| {
                    (a == machine && *e == edge && b != dest)
                        || (b == machine && e.opposite() == edge && a != dest)
                })
            };
            if ambiguous(from, *edge, to) || ambiguous(to, edge.opposite(), from) {
                continue;
            }
            if aliases.is_some_and(|(a, b, _)| {
                (a.machine == *from && b.machine == *to) || (b.machine == *from && a.machine == *to)
            }) {
                continue;
            }
            let source = t.free_segments(from, *edge);
            let target = t.free_segments(to, edge.opposite());
            if source.is_empty() || target.is_empty() {
                continue;
            }
            let span = |parts: &[(String, f64, f64)]| {
                (
                    parts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min),
                    parts.iter().map(|p| p.2).fold(f64::NEG_INFINITY, f64::max),
                )
            };
            let (sl, sh) = span(&source);
            let (tl, th) = span(&target);
            let mut cuts = vec![0.0, 1.0];
            for (_, a, b) in &source {
                cuts.extend([(a - sl) / (sh - sl), (b - sl) / (sh - sl)]);
            }
            for (_, a, b) in &target {
                cuts.extend([(a - tl) / (th - tl), (b - tl) / (th - tl)]);
            }
            cuts.sort_by(f64::total_cmp);
            cuts.dedup();
            for pair in cuts.windows(2) {
                let (a, b) = (pair[0], pair[1]);
                if a >= b {
                    continue;
                }
                let middle = (a + b) / 2.0;
                let sm = sl + middle * (sh - sl);
                let tm = tl + middle * (th - tl);
                let src: Vec<_> = source.iter().filter(|p| sm >= p.1 && sm < p.2).collect();
                let dst: Vec<_> = target.iter().filter(|p| tm >= p.1 && tm < p.2).collect();
                if src.len() != 1 || dst.len() != 1 {
                    continue;
                }
                let (sa, sb) = (sl + a * (sh - sl), sl + b * (sh - sl));
                let (ta, tb) = (tl + a * (th - tl), tl + b * (th - tl));
                t.connect(&src[0].0, *edge, sa, sb, &dst[0].0, edge.opposite(), ta, tb);
                t.connect(&dst[0].0, edge.opposite(), ta, tb, &src[0].0, *edge, sa, sb);
            }
        }
        t
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    fn surface(id: &str, machine: &str, x: i32, y: i32, w: i32, h: i32) -> Surface {
        Surface {
            id: id.into(),
            machine: machine.into(),
            rect: Rect { x, y, w, h },
        }
    }
    #[test]
    fn randomized_topologies_preserve_path_and_distance() {
        let mut rng = 481u64;
        for _ in 0..2000 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            let widths = [
                100 + (rng % 800) as i32,
                100 + ((rng >> 12) % 800) as i32,
                100 + ((rng >> 24) % 800) as i32,
            ];
            let y = -(((rng >> 36) % 900) as i32);
            let x = -(((rng >> 46) % 900) as i32);
            let mut t = Topology {
                revision: 1,
                surfaces: vec![
                    surface("A", "one", x, y, widths[0], 700),
                    surface("D", "two", x + widths[0], y, widths[1], 700),
                    surface("E", "two", x + widths[0] + widths[1], y, widths[2], 700),
                ],
                seams: vec![],
            };
            for (a, b) in [("A", "D"), ("D", "A"), ("D", "E"), ("E", "D")] {
                t.connect_neighbours(a, b);
            }
            let origin = t
                .locate("one", Point::new(x as f64 + 0.25, y as f64 + 350.375))
                .unwrap();
            let dx = (widths[0] + widths[1] + widths[2] / 2) as f64;
            let there = t.advance(&origin, Point::new(dx, 20.0)).unwrap();
            assert_eq!(there.path, ["A", "D", "E"]);
            assert!(!there.wall);
            let back = t.advance(&there.location, Point::new(-dx, -20.0)).unwrap();
            assert_eq!(back.path, ["E", "D", "A"]);
            assert!((back.location.point.x - origin.point.x).abs() < 1e-4);
            assert!((back.location.point.y - origin.point.y).abs() < 1e-4);
        }
    }
    #[test]
    fn gaps_and_corner_motion_stay_on_real_surfaces() {
        let mut t = Topology {
            revision: 1,
            surfaces: vec![
                surface("A", "one", 0, 0, 100, 100),
                surface("D", "two", 100, 40, 100, 60),
            ],
            seams: vec![],
        };
        t.connect_neighbours("A", "D");
        t.connect_neighbours("D", "A");
        let at = t.locate("one", Point::new(90.0, 10.0)).unwrap();
        let wall = t.advance(&at, Point::new(40.0, 5.0)).unwrap();
        assert!(wall.wall);
        assert_eq!(wall.location.surface, "A");
        assert!((wall.location.point.y - 15.0).abs() < 1e-5);
        let corner = t.advance(&wall.location, Point::new(300.0, 300.0)).unwrap();
        let r = t.surface(&corner.location.surface).unwrap().rect;
        assert!(
            corner.location.point.x >= r.x as f64
                && corner.location.point.x <= r.right() as f64
                && corner.location.point.y >= r.y as f64
                && corner.location.point.y <= r.bottom() as f64
        );
        assert!(t.locate("one", Point::new(110.0, 20.0)).is_none());
    }
    #[test]
    fn monitor_order_never_changes_compiled_topology() {
        let monitors = vec![
            (
                "stable-a".into(),
                Rect {
                    x: 0,
                    y: 0,
                    w: 100,
                    h: 100,
                },
            ),
            (
                "stable-b".into(),
                Rect {
                    x: 100,
                    y: 0,
                    w: 100,
                    h: 100,
                },
            ),
        ];
        let mut reversed = monitors.clone();
        reversed.reverse();
        let a = Topology::compile(
            1,
            &[Desk {
                machine: "m".into(),
                monitors,
            }],
            None,
            &[],
        );
        let b = Topology::compile(
            1,
            &[Desk {
                machine: "m".into(),
                monitors: reversed,
            }],
            None,
            &[],
        );
        let at = Location {
            surface: "m:stable-a".into(),
            point: Point::new(90.0, 50.0),
        };
        assert_eq!(
            a.advance(&at, Point::new(20.0, 0.0)),
            b.advance(&at, Point::new(20.0, 0.0))
        );
    }
    #[test]
    fn malformed_reports_and_old_epochs_fail_closed() {
        let t = Topology {
            revision: 1,
            surfaces: vec![surface("A", "m", 0, 0, 100, 100)],
            seams: vec![],
        };
        assert!(t
            .advance(
                &Location {
                    surface: "A".into(),
                    point: Point::new(1.0, 1.0)
                },
                Point::new(f64::NAN, 0.0)
            )
            .is_none());
        let mut r = Receiver::default();
        let latest = Stamp {
            session: 3,
            generation: 8,
            sequence: 1,
            revision: 10,
        };
        assert!(r.accept(latest, 3, 10));
        for sequence in [2, 99, 10000] {
            assert!(!r.accept(
                Stamp {
                    generation: 7,
                    sequence,
                    ..latest
                },
                3,
                10
            ));
        }
        assert!(!r.accept(
            Stamp {
                session: 2,
                generation: 99,
                sequence: 10000,
                ..latest
            },
            3,
            10
        ));
    }
}

/// Self-contained native motion sample; timestamps do not affect replay.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplaySample {
    pub topology: Topology,
    pub start: Location,
    pub delta: Point,
    pub expected: Step,
}
impl ReplaySample {
    pub fn matches_replay(&self) -> bool {
        self.topology
            .advance(&self.start, self.delta)
            .is_some_and(|actual| {
                actual.path == self.expected.path
                    && actual.wall == self.expected.wall
                    && actual.location.surface == self.expected.location.surface
                    && (actual.location.point.x - self.expected.location.point.x).abs() < 1e-6
                    && (actual.location.point.y - self.expected.location.point.y).abs() < 1e-6
            })
    }
}

#[cfg(test)]
mod ambiguous_link_contract {
    use super::*;
    #[test]
    fn conflicting_links_are_disabled_in_both_directions() {
        let desks: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|name| Desk {
                machine: name.into(),
                monitors: vec![(
                    "panel".into(),
                    Rect {
                        x: 0,
                        y: 0,
                        w: 100,
                        h: 100,
                    },
                )],
            })
            .collect();
        let links = vec![
            ("a".into(), Edge::Right, "b".into()),
            ("a".into(), Edge::Right, "c".into()),
        ];
        let t = Topology::compile(1, &desks, None, &links);
        assert!(t.seams.is_empty());
        for machine in ["a", "b", "c"] {
            let at = t.locate(machine, Point::new(90.0, 50.0)).unwrap();
            assert!(t.advance(&at, Point::new(100.0, 0.0)).unwrap().wall);
        }
    }
}

#[cfg(test)]
mod explicit_segment_contract {
    use super::*;
    #[test]
    fn l_shaped_notch_has_reciprocal_links_without_an_empty_desktop_portal() {
        let t = Topology::compile(
            1,
            &[
                Desk {
                    machine: "m".into(),
                    monitors: vec![
                        (
                            "a".into(),
                            Rect {
                                x: 0,
                                y: 0,
                                w: 100,
                                h: 100,
                            },
                        ),
                        (
                            "b".into(),
                            Rect {
                                x: 100,
                                y: 50,
                                w: 100,
                                h: 50,
                            },
                        ),
                    ],
                },
                Desk {
                    machine: "n".into(),
                    monitors: vec![(
                        "d".into(),
                        Rect {
                            x: 0,
                            y: 0,
                            w: 100,
                            h: 100,
                        },
                    )],
                },
            ],
            None,
            &[("m".into(), Edge::Right, "n".into())],
        );
        for (id, x, y) in [("m:a", 90.0, 25.0), ("m:b", 190.0, 75.0)] {
            let at = Location {
                surface: id.into(),
                point: Point::new(x, y),
            };
            let across = t.advance(&at, Point::new(20.0, 0.0)).unwrap();
            assert_eq!(across.location.surface, "n:d");
            let back = t.advance(&across.location, Point::new(-20.0, 0.0)).unwrap();
            assert_eq!(back.location.surface, id);
            assert!((back.location.point.x - x).abs() < 1e-6);
            assert!((back.location.point.y - y).abs() < 1e-6);
        }
    }
    #[test]
    fn overlapping_free_edges_are_ambiguous_instead_of_choosing_the_first_monitor() {
        let t = Topology::compile(
            1,
            &[
                Desk {
                    machine: "m".into(),
                    monitors: vec![
                        (
                            "a".into(),
                            Rect {
                                x: 0,
                                y: 0,
                                w: 100,
                                h: 100,
                            },
                        ),
                        (
                            "b".into(),
                            Rect {
                                x: 300,
                                y: 0,
                                w: 100,
                                h: 100,
                            },
                        ),
                    ],
                },
                Desk {
                    machine: "n".into(),
                    monitors: vec![(
                        "d".into(),
                        Rect {
                            x: 0,
                            y: 0,
                            w: 100,
                            h: 100,
                        },
                    )],
                },
            ],
            None,
            &[("m".into(), Edge::Right, "n".into())],
        );
        assert!(t.seams.is_empty());
    }
}
