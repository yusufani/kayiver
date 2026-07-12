//! Key code translation. The wire format uses USB HID keyboard usage IDs
//! (page 0x07); each platform maps to/from its native virtual key codes.
//!
//! Only entries relevant to the local OS are compiled in.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::OnceLock;

// HID usages the engine itself needs to recognize.
pub const HID_ESC: u16 = 0x29;
pub const HID_CAPSLOCK: u16 = 0x39;
pub const HID_LCTRL: u16 = 0xE0;
pub const HID_LSHIFT: u16 = 0xE1;
pub const HID_LALT: u16 = 0xE2;
pub const HID_LGUI: u16 = 0xE3;
pub const HID_RCTRL: u16 = 0xE4;
pub const HID_RSHIFT: u16 = 0xE5;
pub const HID_RALT: u16 = 0xE6;
pub const HID_RGUI: u16 = 0xE7;

pub fn is_modifier(hid: u16) -> bool {
    (0xE0..=0xE7).contains(&hid) || hid == HID_CAPSLOCK
}

/// (hid, native vk) pairs beyond the alphanumeric block, which is generated.
#[cfg(target_os = "macos")]
const KEYS: &[(u16, u16)] = &[
    // HID -> macOS kVK_* virtual key codes.
    (0x04, 0), (0x05, 11), (0x06, 8), (0x07, 2), (0x08, 14), (0x09, 3),
    (0x0A, 5), (0x0B, 4), (0x0C, 34), (0x0D, 38), (0x0E, 40), (0x0F, 37),
    (0x10, 46), (0x11, 45), (0x12, 31), (0x13, 35), (0x14, 12), (0x15, 15),
    (0x16, 1), (0x17, 17), (0x18, 32), (0x19, 9), (0x1A, 13), (0x1B, 7),
    (0x1C, 16), (0x1D, 6),
    (0x1E, 18), (0x1F, 19), (0x20, 20), (0x21, 21), (0x22, 23), (0x23, 22),
    (0x24, 26), (0x25, 28), (0x26, 25), (0x27, 29),
    (0x28, 36),  // Enter
    (0x29, 53),  // Escape
    (0x2A, 51),  // Backspace
    (0x2B, 48),  // Tab
    (0x2C, 49),  // Space
    (0x2D, 27), (0x2E, 24), (0x2F, 33), (0x30, 30), (0x31, 42),
    (0x33, 41), (0x34, 39), (0x35, 50), (0x36, 43), (0x37, 47), (0x38, 44),
    (0x39, 57),  // CapsLock
    (0x3A, 122), (0x3B, 120), (0x3C, 99), (0x3D, 118), (0x3E, 96), (0x3F, 97),
    (0x40, 98), (0x41, 100), (0x42, 101), (0x43, 109), (0x44, 103), (0x45, 111),
    (0x46, 105), // PrintScreen -> F13 (classic Mac mapping)
    (0x47, 107), // ScrollLock  -> F14
    (0x48, 113), // Pause       -> F15
    (0x49, 114), // Insert      -> Help
    (0x4A, 115), (0x4B, 116), (0x4C, 117), (0x4D, 119), (0x4E, 121),
    (0x4F, 124), (0x50, 123), (0x51, 125), (0x52, 126),
    (0x53, 71),  // NumLock -> KP Clear
    (0x54, 75), (0x55, 67), (0x56, 78), (0x57, 69), (0x58, 76),
    (0x59, 83), (0x5A, 84), (0x5B, 85), (0x5C, 86), (0x5D, 87),
    (0x5E, 88), (0x5F, 89), (0x60, 91), (0x61, 92), (0x62, 82), (0x63, 65),
    (0x68, 105), (0x69, 107), (0x6A, 113), // F13-F15 share keys above
    (0x6B, 106), (0x6C, 64), (0x6D, 79), (0x6E, 80), // F16-F19
    (0xE0, 59), (0xE1, 56), (0xE2, 58), (0xE3, 55),
    (0xE4, 62), (0xE5, 60), (0xE6, 61), (0xE7, 54),
];

#[cfg(target_os = "windows")]
const KEYS: &[(u16, u16)] = &[
    // HID -> Windows VK_* codes. Letters/digits are generated in `maps()`.
    (0x28, 0x0D), // Enter
    (0x29, 0x1B), // Escape
    (0x2A, 0x08), // Backspace
    (0x2B, 0x09), // Tab
    (0x2C, 0x20), // Space
    (0x2D, 0xBD), (0x2E, 0xBB), (0x2F, 0xDB), (0x30, 0xDD), (0x31, 0xDC),
    (0x33, 0xBA), (0x34, 0xDE), (0x35, 0xC0), (0x36, 0xBC), (0x37, 0xBE), (0x38, 0xBF),
    (0x39, 0x14), // CapsLock
    (0x3A, 0x70), (0x3B, 0x71), (0x3C, 0x72), (0x3D, 0x73), (0x3E, 0x74), (0x3F, 0x75),
    (0x40, 0x76), (0x41, 0x77), (0x42, 0x78), (0x43, 0x79), (0x44, 0x7A), (0x45, 0x7B),
    (0x46, 0x2C), // PrintScreen
    (0x47, 0x91), // ScrollLock
    (0x48, 0x13), // Pause
    (0x49, 0x2D), (0x4A, 0x24), (0x4B, 0x21), (0x4C, 0x2E), (0x4D, 0x23), (0x4E, 0x22),
    (0x4F, 0x27), (0x50, 0x25), (0x51, 0x28), (0x52, 0x26),
    (0x53, 0x90), // NumLock
    (0x54, 0x6F), (0x55, 0x6A), (0x56, 0x6D), (0x57, 0x6B),
    (0x58, 0x0D), // KP Enter -> VK_RETURN (extended)
    (0x59, 0x61), (0x5A, 0x62), (0x5B, 0x63), (0x5C, 0x64), (0x5D, 0x65),
    (0x5E, 0x66), (0x5F, 0x67), (0x60, 0x68), (0x61, 0x69), (0x62, 0x60), (0x63, 0x6E),
    (0x68, 0x7C), (0x69, 0x7D), (0x6A, 0x7E), (0x6B, 0x7F), (0x6C, 0x80), (0x6D, 0x81), (0x6E, 0x82),
    (0xE0, 0xA2), (0xE1, 0xA0), (0xE2, 0xA4), (0xE3, 0x5B),
    (0xE4, 0xA3), (0xE5, 0xA1), (0xE6, 0xA5), (0xE7, 0x5C),
];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const KEYS: &[(u16, u16)] = &[];

fn maps() -> &'static (HashMap<u16, u16>, HashMap<u16, u16>) {
    static MAPS: OnceLock<(HashMap<u16, u16>, HashMap<u16, u16>)> = OnceLock::new();
    MAPS.get_or_init(|| {
        let mut fwd = HashMap::new();
        let mut rev = HashMap::new();
        // Windows letter/digit VKs are ASCII, so generate them.
        #[cfg(target_os = "windows")]
        {
            for i in 0..26u16 {
                fwd.insert(0x04 + i, 0x41 + i); // A-Z
                rev.insert(0x41 + i, 0x04 + i);
            }
            for i in 0..9u16 {
                fwd.insert(0x1E + i, 0x31 + i); // 1-9
                rev.insert(0x31 + i, 0x1E + i);
            }
            fwd.insert(0x27, 0x30); // 0
            rev.insert(0x30, 0x27);
        }
        for &(hid, vk) in KEYS {
            fwd.entry(hid).or_insert(vk);
            rev.entry(vk).or_insert(hid); // first entry wins for shared VKs
        }
        (fwd, rev)
    })
}

/// HID usage -> native virtual key (for injection).
pub fn hid_to_native(hid: u16) -> Option<u16> {
    maps().0.get(&hid).copied()
}

/// Native virtual key -> HID usage (for capture).
pub fn native_to_hid(vk: u16) -> Option<u16> {
    maps().1.get(&vk).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn roundtrip_core_keys() {
        // Every key that appears in the forward map must come back to *some*
        // HID code that maps to the same native key (shared VKs collapse).
        for &(hid, _) in KEYS {
            let vk = hid_to_native(hid).unwrap();
            let back = native_to_hid(vk).unwrap();
            assert_eq!(hid_to_native(back), Some(vk), "hid {hid:#x}");
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mac_letter_a() {
        assert_eq!(hid_to_native(0x04), Some(0)); // kVK_ANSI_A
        assert_eq!(native_to_hid(0), Some(0x04));
    }
}
