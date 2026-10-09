//! Source motion uses accelerated event displacement and retains subpixels.
use kayiver_core::motion::Point;

pub(super) fn physical_motion(delta:Point, unaccelerated:Point) -> bool {
    delta.x!=0.0 || delta.y!=0.0 || unaccelerated.x!=0.0 || unaccelerated.y!=0.0
}

pub(super) fn screen_delta(native:Point, reference:Option<Point>, event_delta:Point, remote:bool) -> Point {
    let Some(previous)=reference else {return event_delta;};
    if remote {
        // A warp changes absolute coordinates, but not the accelerated delta
        // on already queued reports. Only fractional phase is read from native
        // coordinates; the integer part comes from the source OS's event.
        let phase=|v:f64|v-v.floor();
        return Point::new(event_delta.x+phase(native.x)-phase(previous.x),event_delta.y+phase(native.y)-phase(previous.y));
    }
    let mut delta=Point::new(native.x-previous.x,native.y-previous.y);
    // At an OS-clipped local edge the report still carries outward intent.
    if delta.x==0.0 && event_delta.x!=0.0 {delta.x=event_delta.x;}
    if delta.y==0.0 && event_delta.y!=0.0 {delta.y=event_delta.y;}
    delta
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
    fn accelerated_displacement_and_fractional_phase_survive_parking() {
        let start=Point::new(3450.25,120.25);
        // The same accelerated report lands either near the real cursor or
        // near the native park. Parking must not change its consumed distance.
        let delta=Point::new(100.0,11.0);
        assert_eq!(screen_delta(Point::new(3550.75,131.5),Some(start),delta,false),Point::new(100.5,11.25));
        assert_eq!(screen_delta(Point::new(3940.75,731.5),Some(start),delta,true),Point::new(100.5,11.25));
    }
    #[test]
    fn queued_prewarp_reports_never_include_the_warp_distance() {
        let previous=Point::new(3450.25,0.75);
        let queued=Point::new(3452.5,0.5);
        assert_eq!(screen_delta(queued,Some(previous),Point::new(2.0,-1.0),true),Point::new(2.25,-1.25));
        let parked_report=Point::new(3843.75,720.25);
        assert_eq!(screen_delta(parked_report,Some(queued),Point::new(3.0,0.0),true),Point::new(3.25,-0.25));
    }
    #[test]
    fn fractional_phase_is_not_repeated_on_each_remote_report() {
        let start=Point::new(3840.0,720.0);let p=Point::new(3840.25,720.0);
        assert_eq!(screen_delta(p,Some(start),Point::default(),true).x,0.25);
        assert_eq!(screen_delta(p,Some(p),Point::default(),true).x,0.0);
        assert_eq!(screen_delta(p,Some(p),Point::new(1.0,0.0),true).x,1.0);
    }
    #[test]
    fn local_clipping_retains_outward_intent() {
        let edge=Point::new(2559.0,700.0);
        assert_eq!(screen_delta(edge,Some(edge),Point::new(10.0,0.0),false),Point::new(10.0,0.0));
    }
}
