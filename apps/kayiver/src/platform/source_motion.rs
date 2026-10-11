//! Keep native coordinates and OS event displacement as separate contracts.
//! A programmatic warp must never contribute movement to the transport.
use kayiver_core::motion::Point;

pub(super) fn physical_motion(delta:Point, unaccelerated:Point) -> bool {
    delta.x!=0.0 || delta.y!=0.0 || unaccelerated.x!=0.0 || unaccelerated.y!=0.0
}

pub(super) fn screen_delta(native:Point, reference:Option<Point>, event_delta:Point, remote:bool) -> Point {
    if remote {
        // The event already carries the source OS's quantized accelerated
        // displacement. Its rounding phase is NOT the absolute cursor's
        // fractional coordinate. Combining the two can reverse real motion:
        // native y 832.125 -> 831.8671875, dy 0 becomes +0.7421875.
        // Parking and queued pre-warp coordinates have no authority here.
        return event_delta;
    }
    let Some(previous)=reference else {return event_delta;};
    let mut delta=Point::new(native.x-previous.x,native.y-previous.y);
    // At an OS-clipped local edge the report still carries outward intent.
    if delta.x==0.0 && event_delta.x!=0.0 {delta.x=event_delta.x;}
    if delta.y==0.0 && event_delta.y!=0.0 {delta.y=event_delta.y;}
    delta
}

/// One native return relocation, fenced from reports already queued in Quartz.
/// Unlike a debounce, this never drops or delays physical movement.
#[derive(Clone,Copy)]
pub(super) struct ReturnWarp { origin:Point, target:Point, destination:Point, cutoff:u64 }
impl ReturnWarp {
    pub fn new(origin:Point,destination:Point,cutoff:u64)->Self {
        Self {origin,target:destination,destination,cutoff}
    }
    pub fn consume(&mut self,timestamp:u64,native:Point,report:Point)->(Point,Point,bool) {
        if timestamp<=self.cutoff {
            self.origin=native;
            self.destination=Point::new(self.destination.x+report.x,self.destination.y+report.y);
            return (self.destination,report,false);
        }
        let delta=Point::new(report.x-(self.target.x-self.origin.x),report.y-(self.target.y-self.origin.y));
        // Absolute native position is authoritative once the relocation has
        // reached Quartz. Fractional relocation must not enter app deltas.
        (native,delta,true)
    }
}

/// Position-only native hooks must explicitly rebase after a programmatic
/// move. A parked remote source uses the parking origin for every report.
#[cfg(any(target_os="windows",test))]
#[derive(Default)]
pub(super) struct PositionReference { previous: Option<(i32,i32)> }
#[cfg(any(target_os="windows",test))]
impl PositionReference {
    pub fn rebase(&mut self, native:(i32,i32)) {self.previous=Some(native);}
    pub fn consume(&mut self, native:(i32,i32), parked:Option<(i32,i32)>) -> (i32,i32) {
        let origin=parked.or(self.previous).unwrap_or(native);
        self.previous=Some(native);
        (native.0-origin.0,native.1-origin.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn return_relocation_is_removed_once_without_losing_accelerated_motion() {
        let mut warp=ReturnWarp::new(Point::new(3840.0,720.0),Point::new(3712.0,529.0),100);
        let (_,queued,settled)=warp.consume(99,Point::new(3842.0,723.0),Point::new(2.0,3.0));
        assert_eq!(queued,Point::new(2.0,3.0));assert!(!settled);
        let (native,delta,settled)=warp.consume(101,Point::new(3717.0,535.0),Point::new(-125.0,-188.0));
        assert_eq!(delta,Point::new(5.0,6.0));assert!(settled);assert_eq!(native,Point::new(3717.0,535.0));
    }
    #[test]
    fn programmatic_receiver_positions_do_not_become_source_motion() {
        let mut reference=PositionReference::default();reference.rebase((100,100));
        assert_eq!(reference.consume((102,99),None),(2,-1));
        // Peer control, negative-origin desktop and hidden-panel parking.
        for destination in [(900,-500),(-1500,270),(3840,720)] {
            reference.rebase(destination);
            assert_eq!(reference.consume((destination.0+2,destination.1-1),None),(2,-1));
            assert_eq!(reference.consume(destination,None),(-2,1));
        }
    }
    #[test]
    fn parked_reports_preserve_immediate_direction_changes() {
        let mut reference=PositionReference::default();reference.rebase((5,5));
        for delta in [(100,-20),(-100,20),(1,0),(-1,0)] {
            assert_eq!(reference.consume((3840+delta.0,720+delta.1),Some((3840,720))),delta);
        }
        reference.rebase((2700,140));
        assert_eq!(reference.consume((2701,140),None),(1,0));
    }
    #[test]
    fn programmatic_position_changes_are_not_physical_motion() {
        assert!(!physical_motion(Point::default(),Point::default()));
        assert!(physical_motion(Point::default(),Point::new(0.25,0.0)));
        assert!(physical_motion(Point::new(10.0,0.0),Point::default()));
    }
    #[test]
    fn remote_reports_are_independent_of_native_position_and_fraction() {
        let reports=[Point::new(0.0,0.0),Point::new(0.0,-1.0),Point::new(12.0,20.0),Point::new(-100.0,70.0)];
        for report in reports {
            for native in [Point::new(3515.40234375,831.8671875),Point::new(3840.0,720.0),Point::new(-1512.9,0.1)] {
                for previous in [None,Some(Point::new(3515.40234375,832.125)),Some(Point::new(3840.5,720.75))] {
                    assert_eq!(screen_delta(native,previous,report,true),report);
                }
            }
        }
    }
    #[test]
    fn physical_fraction_crossing_does_not_create_reversed_remote_motion() {
        // Consecutive physical reports from the read-only HID recording.
        // The old floor-phase formula generated +0.7421875 for this upward
        // movement despite the OS reporting zero integer displacement.
        let previous=Point::new(3515.40234375,832.125);
        let native=Point::new(3515.40234375,831.8671875);
        assert_eq!(screen_delta(native,Some(previous),Point::default(),false),Point::new(0.0,-0.2578125));
        assert_eq!(screen_delta(native,Some(previous),Point::default(),true),Point::default());
        // Quantization is supplied by subsequent source reports. Do not add
        // another fractional accumulator with an unrelated phase.
        assert_eq!(screen_delta(Point::new(3515.40234375,831.609375),Some(native),Point::new(0.0,-1.0),true),Point::new(0.0,-1.0));
    }
    #[test]
    fn remote_sequence_consumes_each_os_delta_once_without_phase_noise() {
        let reports=[Point::new(10.0,-4.0),Point::default(),Point::new(-10.0,4.0)];
        let mut total=Point::default();
        for (index,report) in reports.into_iter().enumerate() {
            let actual=screen_delta(Point::new(3840.1+index as f64*0.3,720.9-index as f64*0.2),Some(Point::new(100.7,400.2)),report,true);
            total.x+=actual.x;total.y+=actual.y;
        }
        assert_eq!(total,Point::default());
    }
    #[test]
    fn local_clipping_retains_outward_intent() {
        let edge=Point::new(2559.0,700.0);
        assert_eq!(screen_delta(edge,Some(edge),Point::new(10.0,0.0),false),Point::new(10.0,0.0));
    }
    #[test]
    fn real_hardware_queue_before_return_warp_does_not_become_an_815_pixel_move() {
        let mut warp=ReturnWarp::new(Point::new(3913.609375,0.0),Point::new(3098.609375,19.0),2269930767500000);
        let (_,delta,settled)=warp.consume(2269930765800250,Point::new(3913.609375,0.0),Point::new(-7.0,6.0));
        assert!(!settled);assert_eq!(delta,Point::new(-7.0,6.0));
        let (_,delta,settled)=warp.consume(2269930767226458,Point::new(3907.85546875,6.7109375),Point::new(-6.0,7.0));
        assert!(!settled);assert_eq!(delta,Point::new(-6.0,7.0));
        let (_,delta,settled)=warp.consume(2269930767886416,Point::new(3092.24609375,25.7109375),Point::new(-815.0,19.0));
        assert!(settled);assert_eq!(delta,Point::new(-5.75390625,6.7109375));
    }
    #[test]
    fn return_warp_preserves_fast_reversal_and_fractional_origin() {
        for motion in [Point::new(100.0,-70.0),Point::new(-100.0,70.0),Point::default()] {
            let origin=Point::new(3367.25,14.5);let destination=Point::new(2700.5,529.25);
            let mut warp=ReturnWarp::new(origin,destination,100);
            let report=Point::new(destination.x-origin.x+motion.x,destination.y-origin.y+motion.y);
            let (_,delta,settled)=warp.consume(101,Point::new(destination.x+motion.x,destination.y+motion.y),report);
            assert!(settled);assert_eq!(delta,motion);
        }
    }

}
