//! Quartz flags describe state, not a toggle. Duplicate flagsChanged events,
//! startup with a held key, or a missed release must not invert the held set.
use std::collections::HashSet;
use kayiver_core::proto::InputEvent;

// Family masks are CGEventFlags; side masks are NX_DEVICE*KEYMASK from the
// macOS SDK's IOKit/hidsystem/IOLLEvent.h. Each pair is left, right HID.
const FAMILIES: [(u64,[(u16,u64);2]);4] = [
    (0x0002_0000,[(0xe1,0x2),(0xe5,0x4)]),
    (0x0004_0000,[(0xe0,0x1),(0xe4,0x2000)]),
    (0x0008_0000,[(0xe2,0x20),(0xe6,0x40)]),
    (0x0010_0000,[(0xe3,0x8),(0xe7,0x10)]),
];

pub(super) fn changes(flags:u64,held:&HashSet<u16>)->Vec<InputEvent> {
    let mut releases=Vec::new();
    let mut presses=Vec::new();
    for (family,sides) in FAMILIES {
        let on=flags&family!=0;
        let sided=flags&(sides[0].1|sides[1].1)!=0;
        let known=sides.iter().any(|(key,_)|held.contains(key));
        for (index,(key,mask)) in sides.into_iter().enumerate() {
            // Synthetic producers may omit side bits. Keep a known held side
            // or use the left side for a newly observed aggregate modifier.
            let pressed=on && if sided {flags&mask!=0}else if known {held.contains(&key)}else{index==0};
            if pressed!=held.contains(&key) {
                let event=InputEvent::Key {key,pressed};
                if pressed {presses.push(event);}else{releases.push(event);}
            }
        }
    }
    releases.extend(presses);
    releases
}

#[cfg(test)]
mod tests {
    use super::*;
    fn apply(held:&mut HashSet<u16>,flags:u64)->Vec<InputEvent> {
        let events=changes(flags,held);
        for event in &events {
            if let InputEvent::Key {key,pressed}=event {if *pressed {held.insert(*key);}else{held.remove(key);}}
        }
        events
    }
    #[test]
    fn duplicate_flags_and_release_without_press_do_not_invert_shift() {
        let mut held=HashSet::new();
        assert!(apply(&mut held,0).is_empty());
        assert_eq!(apply(&mut held,0x20002),vec![InputEvent::Key {key:0xe1,pressed:true}]);
        assert!(apply(&mut held,0x20002).is_empty());
        assert_eq!(apply(&mut held,0),vec![InputEvent::Key {key:0xe1,pressed:false}]);
        assert!(apply(&mut held,0).is_empty());
    }
    #[test]
    fn wheel_snapshot_releases_a_modifier_whose_keyup_was_missed() {
        for (family,sides) in FAMILIES {
            for (key,mask) in sides {
                let mut held=HashSet::new();apply(&mut held,family|mask);
                assert_eq!(held,HashSet::from([key]));
                assert_eq!(apply(&mut held,0),vec![InputEvent::Key {key,pressed:false}]);
            }
        }
    }
    #[test]
    fn releasing_one_side_preserves_the_other_and_unrelated_keys() {
        let mut held=HashSet::from([0x04]);
        apply(&mut held,0x20006);
        assert_eq!(apply(&mut held,0x20004),vec![InputEvent::Key {key:0xe1,pressed:false}]);
        assert!(held.contains(&0xe5));assert!(held.contains(&0x04));
        apply(&mut held,0);assert_eq!(held,HashSet::from([0x04]));
    }
    #[test]
    fn aggregate_only_flags_preserve_a_known_right_modifier_and_release_it() {
        let mut held=HashSet::from([0xe6]);
        assert!(apply(&mut held,0x80000).is_empty());
        assert_eq!(apply(&mut held,0),vec![InputEvent::Key {key:0xe6,pressed:false}]);
        apply(&mut held,0x80000);assert_eq!(held,HashSet::from([0xe2]));
    }
}
