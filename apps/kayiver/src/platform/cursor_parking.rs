//! Native cursor containment, independent of logical navigation coordinates.
#[derive(Default)]
pub(super) struct CursorParking { anchor: Option<(i32,i32)> }
impl CursorParking {
    pub const fn new() -> Self {Self {anchor:None}}
    pub fn set_remote(&mut self, remote: bool, native: (i32,i32)) {
        if remote {self.anchor.get_or_insert(native);} else {self.anchor=None;}
    }
    pub fn anchor(&self) -> Option<(i32,i32)> {self.anchor}
    pub fn correction(&self, native: (i32,i32)) -> Option<(i32,i32)> {
        self.anchor.filter(|anchor| *anchor!=native)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_drift_never_changes_the_anchor() {
        let mut p=CursorParking::default();p.set_remote(true,(3000,0));
        for n in 1..1000 {p.set_remote(true,(3000+n,n));assert_eq!(p.correction((3000+n,n)),Some((3000,0)));}
        assert_eq!(p.correction((3000,0)),None);
    }
    #[test]
    fn return_and_local_empty_edges_are_never_corrected() {
        let mut p=CursorParking::default();p.set_remote(true,(3000,0));p.set_remote(false,(3400,140));
        for native in [(3400,140),(3400,1439),(5119,1439),(2560,0)] {assert_eq!(p.correction(native),None);}
        p.set_remote(true,(3800,0));assert_eq!(p.correction((3800,4)),Some((3800,0)));
    }
}
