//! Fn keys that only the platform input device reports. Fn+R (refresh-rate toggle) arrives as a burst of ~10 key
//! events per press, so presses are debounced. Fn+Q needs no key handling: it changes `platform_profile`, which the
//! daemon already watches.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::sysroot::SysRoot;

pub const FN_KEYS_DEVICE: &str = "Ideapad extra buttons";
/// KEY_REFRESH_RATE_TOGGLE (562), and 188 (F18) while the old hwdb remap is installed.
const FN_R_CODES: [u16; 2] = [562, 188];
const EV_KEY: u16 = 1;
/// `struct input_event` on 64-bit: timeval (16 bytes), type u16, code u16, value i32.
pub const EVENT_SIZE: usize = 24;
pub const FN_R_DEBOUNCE: Duration = Duration::from_millis(1500);

/// `/dev/input/eventN` of the device named `name`, if present.
pub fn find_device(root: &SysRoot, name: &str) -> Option<PathBuf> {
    root.children("/sys/class/input", "event")
        .into_iter()
        .find(|e| root.read(e.join("device/name")).as_deref() == Some(name))
        .and_then(|e| Some(PathBuf::from("/dev/input").join(e.file_name()?)))
}

/// Whether a buffer of raw input events contains an Fn+R key-down.
pub fn has_fn_r_press(data: &[u8]) -> bool {
    data.as_chunks::<EVENT_SIZE>().0.iter().any(|e| {
        let kind = u16::from_ne_bytes([e[16], e[17]]);
        let code = u16::from_ne_bytes([e[18], e[19]]);
        let value = i32::from_ne_bytes([e[20], e[21], e[22], e[23]]);
        kind == EV_KEY && FN_R_CODES.contains(&code) && value == 1
    })
}

/// One action per press: further presses within the window belong to the same burst.
#[derive(Debug, Default)]
pub struct Debounce {
    last: Option<Instant>,
}

impl Debounce {
    /// Whether a press at `now` starts a new action.
    pub fn press(&mut self, now: Instant) -> bool {
        let fresh = self
            .last
            .is_none_or(|t| now.duration_since(t) > FN_R_DEBOUNCE);
        if fresh {
            self.last = Some(now);
        }
        fresh
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u16, code: u16, value: i32) -> Vec<u8> {
        let mut e = vec![0u8; 16];
        e.extend(kind.to_ne_bytes());
        e.extend(code.to_ne_bytes());
        e.extend(value.to_ne_bytes());
        e
    }

    #[test]
    fn a_burst_is_one_press_and_a_later_burst_is_another() {
        // what one Fn+R press looks like on this machine: key-down/up pairs for both codes, plus scan codes (EV_MSC)
        let burst: Vec<u8> = (0..5)
            .flat_map(|_| {
                [
                    event(4, 4, 0x110),
                    event(EV_KEY, 562, 1),
                    event(EV_KEY, 562, 0),
                    event(EV_KEY, 188, 1),
                ]
            })
            .flatten()
            .collect();
        assert!(has_fn_r_press(&burst));
        assert!(!has_fn_r_press(
            &[
                event(4, 4, 0x110),
                event(EV_KEY, 562, 0),
                event(EV_KEY, 30, 1)
            ]
            .concat()
        ));

        let start = Instant::now();
        let mut d = Debounce::default();
        assert!(d.press(start));
        assert!(
            !d.press(start + Duration::from_millis(900)),
            "rest of the burst"
        );
        assert!(
            !d.press(start + Duration::from_millis(1400)),
            "still the same press"
        );
        assert!(d.press(start + Duration::from_millis(1600)), "next press");
    }
}
