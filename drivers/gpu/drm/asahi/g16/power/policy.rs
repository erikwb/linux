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

/// Percentage of `window` ns the GPU was busy for.
pub(crate) fn utilization(busy: u64, window: u64) -> u32 {
    if window == 0 {
        100
    } else {
        let busy = busy.min(window);
        // No 128-bit division in the kernel; a window near u64::MAX loses nothing.
        let util = match busy.checked_mul(100) {
            Some(scaled) => scaled / window,
            None => busy / (window / 100),
        };
        util.min(100) as u32
    }
}

/// Utilization averaged over about four windows; a longer gap restarts it.
pub(crate) fn smooth(average: u32, util: u32, window: u64) -> u32 {
    if window >= 4 * WINDOW_NS {
        util
    } else {
        (3 * average.min(100) + util.min(100)) / 4
    }
}

/// The state to request from the `current` published one: up on the last
/// window's utilization `util`, to the top state under heavy load, and down
/// on the smoothed `average`, to the floor when idle.
pub(crate) fn demand(current: u32, util: u32, average: u32) -> u32 {
    let next = if util >= 70 {
        PSTATE_MAX
    } else if util >= 40 {
        current.saturating_add(1)
    } else if average < 10 {
        FLOOR
    } else if average < 25 {
        current.saturating_sub(1)
    } else {
        current
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
    fn utilization_is_a_bounded_share_of_the_window() {
        let w = WINDOW_NS;
        assert_eq!(utilization(0, w), 0);
        assert_eq!(utilization(w / 4, w), 25);
        assert_eq!(utilization(w, w), 100);
        assert_eq!(utilization(2 * w, w), 100);
        assert_eq!(utilization(0, 0), 100);
        assert_eq!(utilization(u64::MAX, u64::MAX), 100);
    }

    #[test]
    fn the_average_decays_and_restarts_after_a_gap() {
        let w = WINDOW_NS;
        assert_eq!(smooth(100, 0, w), 75);
        assert_eq!(smooth(0, 100, w), 25);
        assert_eq!(smooth(100, 0, 4 * w), 0);
        assert_eq!(smooth(u32::MAX, u32::MAX, w), 100);
        let mut average = 100;
        let mut windows = 0;
        while demand(PSTATE_MAX, 0, average) != FLOOR {
            average = smooth(average, 0, w);
            windows += 1;
        }
        assert!(windows <= 10);
    }

    #[test]
    fn demand_rises_fast_and_falls_on_the_average() {
        assert_eq!(demand(FLOOR, 100, 0), PSTATE_MAX);
        assert_eq!(demand(FLOOR, 70, 0), PSTATE_MAX);
        assert_eq!(demand(4, 50, 0), 5);
        assert_eq!(demand(PSTATE_MAX, 50, 0), PSTATE_MAX);
        // A short dip under a busy average holds the state.
        assert_eq!(demand(PSTATE_MAX, 5, 80), PSTATE_MAX);
        assert_eq!(demand(6, 30, 30), 6);
        assert_eq!(demand(6, 20, 20), 5);
        assert_eq!(demand(PSTATE_MAX, 0, 5), FLOOR);
        // A light 60 fps load near 25% busy settles on the floor.
        assert_eq!(demand(FLOOR, 25, 24), FLOOR);
        assert_eq!(demand(1, 30, 30), FLOOR);
        for current in [0, 1, FLOOR, 7, PSTATE_MAX, u32::MAX] {
            for util in [0, 20, 50, 70, 100, u32::MAX] {
                for average in [0, 9, 20, 30, 100, u32::MAX] {
                    let next = demand(current, util, average);
                    assert!((FLOOR..=PSTATE_MAX).contains(&next));
                }
            }
        }
    }
}
