// SPDX-License-Identifier: GPL-2.0-only OR MIT

//! Composition and sampling cadence for the retained firmware ceiling, and the
//! utilization governor below it.
//!
//! Temperature decoding and regulation belong to the PMP provider and the
//! shared C cooling policy. This module defines no temperature thresholds.

pub(crate) const PSTATE_MAX: u32 = 9;
pub(crate) const COOLING_MAX: u32 = 8;
pub(crate) const SAMPLE_NS: u64 = 5_000_000;
/// Utilization window of the governor.
pub(crate) const WINDOW_NS: u64 = 20_000_000;
/// Lowest governed state. On the J713 S1-S3 draw under 0.1 W for a desktop
/// load and S4 several times that; S3 runs at 2.4 times the clock of S1.
pub(crate) const FLOOR: u32 = 3;

/// Completion polling must not turn into a PMP MMIO read on every pass.
pub(crate) fn read_due(last: Option<u64>, now: u64) -> bool {
    last.is_none_or(|last| now.wrapping_sub(last) >= SAMPLE_NS)
}

/// Zero is not an unlimited selector in the firmware. Every published state
/// is 1..=9, bounded by the initialized Main table and the user ceiling.
pub(crate) fn limit(configured: u32, ceiling: u32, user: u32) -> u32 {
    ceiling.min(PSTATE_MAX).min(configured).min(user).max(1)
}

/// The state to request after a window in which the GPU was busy for `busy`
/// of `window` ns, from the `current` published one: the top state under heavy
/// load, a step up or down around a 30-60% band, and the floor when idle.
pub(crate) fn demand(current: u32, busy: u64, window: u64) -> u32 {
    let util = if window == 0 {
        100
    } else {
        busy.min(window) * 100 / window
    };
    let next = match util {
        85.. => PSTATE_MAX,
        60..=84 => current.saturating_add(1),
        30..=59 => current,
        10..=29 => current.saturating_sub(1),
        _ => FLOOR,
    };
    next.clamp(FLOOR, PSTATE_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_sampling_is_independent_of_completion_polling() {
        assert!(read_due(None, 0));
        assert!(!read_due(Some(0), 0));
        assert!(!read_due(Some(100_000_000), 104_999_999));
        assert!(read_due(Some(100_000_000), 105_000_000));
        assert!(read_due(Some(u64::MAX - SAMPLE_NS + 1), 0));
    }

    #[test]
    fn initialized_selectors_and_user_caps_bound_every_request() {
        assert_eq!(COOLING_MAX, PSTATE_MAX - 1);
        for configured in [0, 1, 7, 9, 10, u32::MAX] {
            for ceiling in [0, 1, 5, 9, 10, u32::MAX] {
                for user in [0, 1, 3, 9, 10, u32::MAX] {
                    let value = limit(configured, ceiling, user);
                    assert!((1..=PSTATE_MAX).contains(&value));
                    assert!(value <= configured.max(1));
                    assert!(value <= ceiling.max(1));
                    assert!(value <= user.max(1));
                }
            }
        }
    }

    #[test]
    fn demand_follows_utilization_within_the_governed_range() {
        let w = WINDOW_NS;
        assert_eq!(demand(FLOOR, w, w), PSTATE_MAX);
        assert_eq!(demand(FLOOR, 2 * w, w), PSTATE_MAX);
        assert_eq!(demand(4, w * 7 / 10, w), 5);
        assert_eq!(demand(PSTATE_MAX, w * 7 / 10, w), PSTATE_MAX);
        assert_eq!(demand(6, w / 2, w), 6);
        assert_eq!(demand(6, w / 5, w), 5);
        assert_eq!(demand(PSTATE_MAX, 0, w), FLOOR);
        assert_eq!(demand(PSTATE_MAX, 0, 100 * w), FLOOR);
        assert_eq!(demand(1, w / 2, w), FLOOR);
        assert_eq!(demand(FLOOR, w, 0), PSTATE_MAX);
        for current in [0, 1, FLOOR, 7, PSTATE_MAX, u32::MAX] {
            for busy in [0, w / 5, w / 2, w * 7 / 10, w] {
                assert!((FLOOR..=PSTATE_MAX).contains(&demand(current, busy, w)));
            }
        }
    }
}
