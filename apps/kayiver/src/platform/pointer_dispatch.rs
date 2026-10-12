//! Native pointer notification is needed after typing, entry, desktop changes
//! and idle. Continuous absolute placement does not need a second SendInput
//! round-trip. This policy never waits, drops positions or changes geometry.
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct PointerDispatch { last_move:Option<Instant>, pending:bool }
impl PointerDispatch {
    pub fn invalidate(&mut self) {self.pending=true;}
    pub fn needs_native(&self,now:Instant)->bool {
        self.pending || self.last_move.is_none_or(|t|now.saturating_duration_since(t)>=Duration::from_millis(100))
    }
    pub fn applied(&mut self,now:Instant,native_sent:bool) {
        self.last_move=Some(now);
        if native_sent {self.pending=false;}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn continuous_motion_never_waits_and_entry_and_idle_notify_windows() {
        let start=Instant::now();let mut p=PointerDispatch::default();
        assert!(p.needs_native(start));p.applied(start,true);
        for n in 1..1000 {
            let now=start+Duration::from_millis(n);
            assert!(!p.needs_native(now));p.applied(now,false);
        }
        assert!(p.needs_native(start+Duration::from_millis(1100)));
    }
    #[test]
    fn typing_desktop_changes_and_failed_native_events_require_notification() {
        let now=Instant::now();let mut p=PointerDispatch::default();p.applied(now,true);
        for _ in 0..100 {
            p.invalidate();assert!(p.needs_native(now));
            p.applied(now,false);assert!(p.needs_native(now));
            p.applied(now,true);assert!(!p.needs_native(now));
        }
    }
}
