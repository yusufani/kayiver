//! The synchronous input decision point, shared by macOS, Windows and simulation.
use kayiver_core::motion::{Location, Point, Stamp, Topology};
use kayiver_core::proto::{InputEvent, MouseButton};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Frame {
    pub stamp: Stamp,
    pub machine: String,
    pub surface: String,
    pub x: i32,
    pub y: i32,
    pub path: Vec<String>,
    pub wall: bool,
    pub keys: Vec<u16>,
    pub buttons: Vec<MouseButton>,
}
#[derive(Clone, Debug)]
pub enum Control {
    Local,
    Remote(String),
    Driven(String),
    Recovering,
}
#[derive(Clone, Debug)]
pub struct Navigation {
    pub machine: String,
    pub topology: Topology,
    pub location: Option<Location>,
    pub keys: HashSet<u16>,
    pub buttons: HashSet<MouseButton>,
    pub control: Control,
    pub session: u64,
    pub generation: u64,
    pub sequence: u64,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            machine: String::new(),
            topology: Topology::default(),
            location: None,
            control: Control::Local,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            session: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
            generation: 1,
            sequence: 0,
        }
    }
}
impl Navigation {
    pub fn install(&mut self, machine: String, topology: Topology) {
        self.machine = machine;
        if self.topology != topology {
            self.generation += 1;
            if self.location.as_ref().is_some_and(|l| {
                topology.surface(&l.surface).is_none_or(|s| {
                    l.point.x < s.rect.x as f64
                        || l.point.x > s.rect.right() as f64
                        || l.point.y < s.rect.y as f64
                        || l.point.y > s.rect.bottom() as f64
                })
            }) {
                self.location = None;
                self.control = Control::Recovering;
            }
            self.topology = topology;
        }
    }
    pub fn input(&mut self, event: InputEvent) -> (Stamp, Option<String>) {
        match event {
            InputEvent::Key { key, pressed } => {
                if pressed {
                    self.keys.insert(key);
                } else {
                    self.keys.remove(&key);
                }
            }
            InputEvent::MouseButton { button, pressed } => {
                if pressed {
                    self.buttons.insert(button);
                } else {
                    self.buttons.remove(&button);
                }
            }
            _ => {}
        }
        self.sequence += 1;
        let stamp = Stamp {
            session: self.session,
            generation: self.generation,
            sequence: self.sequence,
            revision: self.topology.revision,
        };
        let target = if let Control::Remote(peer) = &self.control {
            Some(peer.clone())
        } else {
            None
        };
        (stamp, target)
    }
    pub fn drive(&mut self, peer: Option<String>) {
        self.control = peer.map(Control::Driven).unwrap_or(Control::Local);
        self.location = None;
        self.generation += 1;
    }
    /// Native position is a reference only in Local mode; Remote mode advances
    /// the logical cursor, never a parked or detached OS cursor.
    pub fn sample(&mut self, native: (i32, i32), dx: i32, dy: i32) -> Option<Frame> {
        if self.machine.is_empty() || matches!(self.control, Control::Driven(_)) {
            return None;
        }
        let prev = Point::new(native.0 as f64 - dx as f64, native.1 as f64 - dy as f64);
        // A native cursor clipped by the OS outer boundary cannot describe the
        // report's starting point. Keep the logical reference for that report.
        let clipped = self
            .location
            .as_ref()
            .and_then(|l| self.topology.surface(&l.surface))
            .is_some_and(|s| {
                (dx < 0 && native.0 <= s.rect.x)
                    || (dx > 0 && native.0 >= s.rect.right() - 1)
                    || (dy < 0 && native.1 <= s.rect.y)
                    || (dy > 0 && native.1 >= s.rect.bottom() - 1)
            });
        if self.location.is_none()
            || (matches!(self.control, Control::Local)
                && !clipped
                && self.location.as_ref().is_some_and(|l| {
                    (l.point.x - prev.x).abs() > 1.01 || (l.point.y - prev.y).abs() > 1.01
                }))
        {
            if let Some(at) = self.topology.locate(&self.machine, prev) {
                self.location = Some(at);
            }
        }
        if self.location.is_none() {
            if !self.topology.surfaces.is_empty() {
                self.control = Control::Recovering;
            }
            return None;
        }
        let at = self.location.as_ref()?;
        let Some(step) = self.topology.advance(at, Point::new(dx as f64, dy as f64)) else {
            self.control = Control::Recovering;
            self.location = None;
            self.generation += 1;
            return None;
        };
        super::motion_trace::record(&self.topology, at, Point::new(dx as f64, dy as f64), &step);
        let surface = self.topology.surface(&step.location.surface)?;
        let was_remote = matches!(self.control, Control::Remote(_));
        self.control = if surface.machine == self.machine {
            Control::Local
        } else {
            Control::Remote(surface.machine.clone())
        };
        self.sequence += 1;
        let mut keys: Vec<_> = self.keys.iter().copied().collect();
        keys.sort_by_key(|key| (!(0xe0..=0xe7).contains(key), *key));
        let frame = Frame {
            stamp: Stamp {
                session: self.session,
                generation: self.generation,
                sequence: self.sequence,
                revision: self.topology.revision,
            },
            machine: surface.machine.clone(),
            surface: surface.id.clone(),
            x: (step.location.point.x.round() as i32)
                .clamp(surface.rect.x, surface.rect.right() - 1),
            y: (step.location.point.y.round() as i32)
                .clamp(surface.rect.y, surface.rect.bottom() - 1),
            path: step.path,
            wall: step.wall,
            keys,
            buttons: self.buttons.iter().copied().collect(),
        };
        self.location = Some(step.location);
        if was_remote
            || step.wall
            || frame.path.len() > 1
            || matches!(self.control, Control::Remote(_))
        {
            Some(frame)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kayiver_core::{motion::Surface, proto::Rect};
    fn nav() -> Navigation {
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
                        x: 0,
                        y: 0,
                        w: 2560,
                        h: 1440,
                    },
                },
            ],
            seams: vec![],
        };
        t.connect(
            "A",
            kayiver_core::layout::Edge::Right,
            0.0,
            1440.0,
            "D",
            kayiver_core::layout::Edge::Left,
            0.0,
            1440.0,
        );
        t.connect(
            "D",
            kayiver_core::layout::Edge::Left,
            0.0,
            1440.0,
            "A",
            kayiver_core::layout::Edge::Right,
            0.0,
            1440.0,
        );
        let mut n = Navigation::default();
        n.install("mac".into(), t);
        n
    }
    #[test]
    fn rapid_roundtrips_never_wait_or_lose_distance() {
        let mut n = nav();
        n.location = Some(Location {
            surface: "A".into(),
            point: Point::new(2550.25, 700.25),
        });
        for _ in 0..1000 {
            let f = n.sample((2559, 700), 100, 0).unwrap();
            assert_eq!(f.machine, "win");
            assert_eq!(f.x, 90);
            let f = n.sample((2559, 700), -100, 0).unwrap();
            assert_eq!(f.machine, "mac");
            assert_eq!(f.x, 2550);
        }
        assert!((n.location.unwrap().point.x - 2550.25).abs() < 0.001);
    }
    #[test]
    fn captured_buttons_and_keys_cross_with_the_cursor() {
        let mut n = nav();
        n.location = Some(Location {
            surface: "A".into(),
            point: Point::new(2550.0, 700.0),
        });
        n.input(InputEvent::Key {
            key: 0xe0,
            pressed: true,
        });
        n.input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        });
        let f = n.sample((2565, 700), 15, 0).unwrap();
        assert_eq!(f.keys, vec![0xe0]);
        assert_eq!(f.buttons, vec![MouseButton::Left]);
        let (s, target) = n.input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        });
        assert_eq!(target.as_deref(), Some("win"));
        assert!(s.sequence > f.stamp.sequence);
    }
    #[test]
    fn physical_input_cannot_steal_a_driven_desk() {
        let mut n = nav();
        n.drive(Some("win".into()));
        assert!(n.sample((2565, 700), 15, 0).is_none());
    }
    #[test]
    fn clipped_native_position_does_not_eat_the_flick_remainder() {
        let mut n = nav();
        n.location = Some(Location {
            surface: "A".into(),
            point: Point::new(2550.25, 700.25),
        });
        let f = n.sample((2559, 700), 100, 0).unwrap();
        assert_eq!(f.machine, "win");
        assert_eq!(f.x, 90);
    }
    #[test]
    fn snapshot_replacement_fences_queued_input() {
        let mut n = nav();
        let old = n.generation;
        let mut t = n.topology.clone();
        t.revision += 1;
        n.install("mac".into(), t);
        assert!(n.generation > old);
    }
}
