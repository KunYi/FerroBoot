//! Interrupt-free millisecond clock built on a free-running 32-bit cycle
//! counter (for example the Cortex-M DWT `CYCCNT`).
//!
//! The bootloader runs with interrupts disabled and must not depend on a
//! SysTick handler (plan D-08), yet the NOR driver needs a real clock for its
//! busy-wait timeouts. A polled cycle counter gives that without interrupts.
//!
//! The pure arithmetic lives here so it can be tested on the host; the crate
//! that owns the hardware supplies `now` (the raw counter) and the number of
//! counter ticks per millisecond.
//!
//! **Contract:** [`MillisClock::advance`] must be called at least once per
//! counter wrap (about 10.7 s at 400 MHz). Every NOR operation in the
//! bootloader polls far more often than that. If a longer gap happens the
//! missing time is dropped (the clock runs slow); it never panics.

#![no_std]
#![forbid(unsafe_code)]

/// Millisecond clock state derived from a free-running 32-bit counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MillisClock {
    /// Counter value at the previous call.
    last: u32,
    /// Counter ticks carried over that did not yet add up to a millisecond.
    remainder: u32,
    /// Milliseconds since `start`; wraps after about 49.7 days.
    millis: u32,
}

impl MillisClock {
    /// A clock whose time zero is the counter value `now`.
    pub const fn start(now: u32) -> Self {
        Self {
            last: now,
            remainder: 0,
            millis: 0,
        }
    }

    /// Update from the current counter value and return milliseconds since
    /// `start`. `ticks_per_ms` is the counter frequency divided by 1000.
    pub fn advance(&mut self, now: u32, ticks_per_ms: u32) -> u32 {
        let per_ms = if ticks_per_ms == 0 { 1 } else { ticks_per_ms };
        let delta = now.wrapping_sub(self.last);
        self.last = now;
        // Saturate instead of overflowing if the contract above was violated.
        let total = delta.saturating_add(self.remainder);
        let whole = total / per_ms;
        self.remainder = total - whole * per_ms;
        self.millis = self.millis.wrapping_add(whole);
        self.millis
    }

    /// Milliseconds as of the last `advance` (no hardware access).
    pub const fn last_millis(&self) -> u32 {
        self.millis
    }
}

/// Elapsed milliseconds between two readings of a wrapping millisecond
/// counter. Correct across the 49.7-day wrap; use this instead of `now - start`.
pub const fn elapsed_ms(now: u32, start: u32) -> u32 {
    now.wrapping_sub(start)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    const PER_MS: u32 = 400_000; // 400 MHz

    #[test]
    fn starts_at_zero_regardless_of_counter_value() {
        let mut c = MillisClock::start(123_456_789);
        assert_eq!(c.advance(123_456_789, PER_MS), 0);
    }

    #[test]
    fn counts_whole_milliseconds() {
        let mut c = MillisClock::start(0);
        assert_eq!(c.advance(PER_MS, PER_MS), 1);
        assert_eq!(c.advance(10 * PER_MS, PER_MS), 10);
        assert_eq!(c.advance(10 * PER_MS + PER_MS - 1, PER_MS), 10);
        assert_eq!(c.advance(11 * PER_MS, PER_MS), 11);
    }

    #[test]
    fn fractions_accumulate_without_drift() {
        // Poll much faster than 1 ms: every call adds a tiny fraction.
        let mut c = MillisClock::start(0);
        let mut now = 0u32;
        let mut ms = 0;
        for _ in 0..1000 {
            now = now.wrapping_add(PER_MS / 8); // 125 us steps
            ms = c.advance(now, PER_MS);
        }
        assert_eq!(ms, 125); // 1000 * 125 us
    }

    #[test]
    fn survives_counter_wrap() {
        let start = u32::MAX - 100_000;
        let mut c = MillisClock::start(start);
        // 300_000 ticks later the counter has wrapped past zero.
        let now = start.wrapping_add(300_000);
        assert!(now < start);
        assert_eq!(c.advance(now, PER_MS), 0);
        let now = start.wrapping_add(PER_MS + 5);
        assert_eq!(c.advance(now, PER_MS), 1);
    }

    #[test]
    fn matches_ideal_integer_time_over_random_steps() {
        let mut rng = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let start = 0xFFFF_0000u32;
        let mut c = MillisClock::start(start);
        let mut total_ticks: u64 = 0;
        let mut now = start;
        let mut readings = Vec::new();
        for _ in 0..5000 {
            let step = (next() % 3_000_000) as u32; // up to 7.5 ms per poll
            total_ticks += step as u64;
            now = now.wrapping_add(step);
            let ms = c.advance(now, PER_MS);
            assert_eq!(ms as u64, total_ticks / PER_MS as u64);
            readings.push(ms);
        }
        assert!(readings.windows(2).all(|w| w[0] <= w[1]), "monotonic");
    }

    #[test]
    fn millisecond_counter_wraps_without_panic() {
        let mut c = MillisClock {
            last: 0,
            remainder: 0,
            millis: u32::MAX,
        };
        assert_eq!(c.advance(PER_MS, PER_MS), 0);
        assert_eq!(c.advance(2 * PER_MS, PER_MS), 1);
    }

    #[test]
    fn overlong_gap_saturates_instead_of_panicking() {
        let mut c = MillisClock::start(0);
        c.remainder = u32::MAX - 10;
        let ms = c.advance(1000, PER_MS);
        assert!(ms > 0);
    }

    #[test]
    fn zero_ticks_per_ms_does_not_divide_by_zero() {
        let mut c = MillisClock::start(0);
        assert_eq!(c.advance(5, 0), 5);
    }

    #[test]
    fn elapsed_ms_is_wrap_safe() {
        assert_eq!(elapsed_ms(10, 4), 6);
        assert_eq!(elapsed_ms(3, u32::MAX - 2), 6);
        assert_eq!(elapsed_ms(7, 7), 0);
    }
}
