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

/// Normalize a report captured before the return warp took effect. Native
/// coordinates may still belong to the parking center; its movement does not.
pub(super) fn after_local_warp(native:Point, previous:Option<Point>, event_delta:Point, destination:Point) -> (Point,Point,bool) {
    let delta=screen_delta(native,previous,event_delta,true);
    let expected=Point::new(destination.x+delta.x,destination.y+delta.y);
    // An integer delta's quantization is at most one pixel per component.
    let settled=(native.x-expected.x).abs()<=1.0 && (native.y-expected.y).abs()<=1.0;
    if settled {(native,screen_delta(native,Some(destination),event_delta,false),true)}
    else {(expected,delta,false)}
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn queued_remote_reports_preserve_distance_after_return_without_teleporting() {
        let mut destination=Point::new(2700.25,140.25);
        let mut previous=Point::new(3840.25,920.25);
        for n in 1..100 {
            let old_basis=Point::new(3840.25+n as f64,920.25);
            let (point,delta,settled)=after_local_warp(old_basis,Some(previous),Point::new(1.0,0.0),destination);
            assert!(!settled);assert_eq!(delta,Point::new(1.0,0.0));
            assert_eq!(point.x,2700.25+n as f64);
            destination=point;previous=old_basis;
        }
        let native=Point::new(destination.x+1.0,destination.y);
        let (point,delta,settled)=after_local_warp(native,Some(previous),Point::new(1.0,0.0),destination);
        assert!(settled);assert_eq!(point,native);assert_eq!(delta,Point::new(1.0,0.0));
    }

}
