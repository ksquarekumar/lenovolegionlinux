//! Fn keys that only the platform input device reports. One Fn+R tap (refresh-rate toggle) arrives as a burst of 2–6
//! key events within ~0.3 s (captured 2026-10-12), so a new tap starts only after a short quiet time.
//! Fn+Q needs no key handling: it changes `platform_profile`, which the daemon already watches.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::sysroot::SysRoot;

pub const FN_KEYS_DEVICE: &str = "Ideapad extra buttons";
/// `KEY_REFRESH_RATE_TOGGLE`.
const FN_R_CODE: u16 = 562;
const EV_KEY: u16 = 1;
/// `struct input_event` on 64-bit: timeval (16 bytes), type u16, code u16, value i32.
pub const EVENT_SIZE: usize = 24;
/// Quiet time that ends a tap: longer than the gaps inside one burst (at most 0.12 s measured), shorter than the gaps
/// between fast deliberate taps (at least 0.66 s measured).
pub const FN_R_QUIET: Duration = Duration::from_millis(400);

/// `/dev/input/eventN` of the device named `name`, if present.
#[must_use]
pub fn find_device(root: &SysRoot, name: &str) -> Option<PathBuf> {
    root.children("/sys/class/input", "event")
        .into_iter()
        .find(|e| root.read(e.join("device/name")).as_deref() == Some(name))
        .and_then(|e| Some(PathBuf::from("/dev/input").join(e.file_name()?)))
}

/// Whether a buffer of raw input events contains an Fn+R key-down.
#[must_use]
pub fn has_fn_r_press(data: &[u8]) -> bool {
    data.as_chunks::<EVENT_SIZE>().0.iter().any(|e| {
        let kind = u16::from_ne_bytes([e[16], e[17]]);
        let code = u16::from_ne_bytes([e[18], e[19]]);
        let value = i32::from_ne_bytes([e[20], e[21], e[22], e[23]]);
        kind == EV_KEY && code == FN_R_CODE && value == 1
    })
}

/// One action per tap: Fn+R events start a new tap only after the key has been quiet for [`FN_R_QUIET`].
#[derive(Debug, Default)]
pub struct Debounce {
    last_event: Option<Instant>,
}

impl Debounce {
    /// Whether Fn+R events seen at `now` start a new tap.
    pub fn press(&mut self, now: Instant) -> bool {
        let fresh = self
            .last_event
            .is_none_or(|t| now.duration_since(t) > FN_R_QUIET);
        self.last_event = Some(now);
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
    fn each_tap_is_one_action_and_fast_taps_each_count() {
        // one key event as captured on this machine (code 188 then, through the old hwdb remap; 562 is the raw
        // code): scan code, key down, sync, key up, sync
        let captured = [
            event(4, 4, 272),
            event(EV_KEY, 562, 1),
            event(0, 0, 0),
            event(EV_KEY, 562, 0),
            event(0, 0, 0),
        ]
        .concat();
        assert!(has_fn_r_press(&captured));
        assert!(
            !has_fn_r_press(
                &[
                    event(4, 4, 272),
                    event(EV_KEY, 562, 0),
                    event(EV_KEY, 30, 1)
                ]
                .concat()
            ),
            "key-ups, scan codes and other keys are not Fn+R presses"
        );

        // key-down times (s) captured on this machine: one tap; then five taps in a row
        let one_tap = [0.0, 0.121];
        let five_taps = [
            0.0, 0.085, 0.166, 0.206, 0.256, 1.080, 1.110, 1.201, 1.281, 1.301, 1.382, 2.056,
            2.116, 2.156, 2.267, 2.318, 2.357, 3.019, 3.101, 9.947, 10.038, 10.128,
        ];
        let actions = |times: &[f64]| -> Vec<f64> {
            let start = Instant::now();
            let mut d = Debounce::default();
            times
                .iter()
                .copied()
                .filter(|t| d.press(start + Duration::from_secs_f64(*t)))
                .collect()
        };
        assert_eq!(actions(&one_tap), [0.0], "one tap is one action");
        assert_eq!(
            actions(&five_taps),
            [0.0, 1.080, 2.056, 3.019, 9.947],
            "fast taps each count"
        );
    }
}
