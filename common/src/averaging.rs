//! Averaging raw counts (the `average` endpoint): mean and standard deviation
//! per channel.
//!
//! Accumulates exact integer sums of the *shifted* counts `d = c − c₀` (`c₀` =
//! first reading) and converts to floating point once at the end:
//!
//! ```text
//! mean = c₀ + Σd / n
//! variance = (Σd² − (Σd)² / n) / (n − 1)      (sample variance)
//! ```
//!
//! Why shift: computed from the raw counts (`Σc²` of values around 10⁵ to 10⁶),
//! the variance would be a small difference of two huge, nearly equal numbers,
//! which loses most of the precision ("catastrophic cancellation"). Shifted, the
//! sums stay small. Why integers: every step is exact; floating point (in
//! software on the ESP32-C6) is only needed once per average.
//!
//! Range: counts are 24-bit, so |d| < 2²⁴, d² < 2⁴⁸; `i64` holds the sums for
//! more than 2¹⁵ readings (`push` saturates the count beyond `MAX_READINGS`).

use crate::interface::{AverageCounts, CHANNELS};

/// Most readings one accumulator takes (2¹⁵ = 131 s at 250 Hz); `i64` sums of
/// d² < 2⁴⁸ can't overflow below 2¹⁵ readings.
pub const MAX_READINGS: u32 = 1 << 15;

#[derive(Clone, Copy, Debug, Default)]
pub struct CountsAccumulator {
    /// First reading, the shift `c₀`; `None` until the first `push`.
    first: Option<[i32; CHANNELS]>,
    n: u32,
    sum: [i64; CHANNELS],
    sum_sq: [i64; CHANNELS],
}

impl CountsAccumulator {
    pub const fn new() -> Self {
        Self {
            first: None,
            n: 0,
            sum: [0; CHANNELS],
            sum_sq: [0; CHANNELS],
        }
    }

    /// Number of readings so far.
    pub fn n(&self) -> u32 {
        self.n
    }

    /// Adds one set of readings. Ignored beyond `MAX_READINGS`.
    pub fn push(&mut self, raw: &[i32; CHANNELS]) {
        if self.n >= MAX_READINGS {
            return;
        }
        let first = *self.first.get_or_insert(*raw);
        for i in 0..CHANNELS {
            let d = i64::from(raw[i]) - i64::from(first[i]);
            self.sum[i] += d;
            self.sum_sq[i] += d * d;
        }
        self.n += 1;
    }

    /// Mean and sample standard deviation per channel. With fewer than 2
    /// readings the standard deviation is 0 (undefined), with none everything is 0.
    pub fn finish(&self) -> AverageCounts {
        let mut mean = [0.0; CHANNELS];
        let mut std_dev = [0.0; CHANNELS];
        if let Some(first) = self.first {
            let n = f64::from(self.n);
            for i in 0..CHANNELS {
                let sum = self.sum[i] as f64;
                mean[i] = (f64::from(first[i]) + sum / n) as f32;
                if self.n >= 2 {
                    // Σd² − (Σd)²/n, in f64 from exact integers. Can come out
                    // very slightly negative from rounding when all values are
                    // equal; clamp before the square root.
                    let squares = self.sum_sq[i] as f64 - sum * sum / n;
                    std_dev[i] = sqrt((squares / (n - 1.0)).max(0.0)) as f32;
                }
            }
        }
        AverageCounts {
            mean,
            std_dev,
            n: self.n,
        }
    }
}

/// Square root by Newton's method: `f64::sqrt` is only in `std`, and this
/// crate is `no_std` (e.g. for the firmware). Runs once per channel per average.
fn sqrt(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    // Start above the root (x ≥ 1: x itself; x < 1: 1), then each step
    // (y + x/y)/2 moves down towards it, roughly doubling the correct digits
    // once close. Stop when it no longer decreases.
    let mut y = if x > 1.0 { x } else { 1.0 };
    loop {
        let next = 0.5 * (y + x / y);
        if next >= y {
            return y;
        }
        y = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accumulate(readings: &[[i32; CHANNELS]]) -> AverageCounts {
        let mut acc = CountsAccumulator::new();
        for r in readings {
            acc.push(r);
        }
        acc.finish()
    }

    /// Reference values computed directly in f64 (two-pass: mean first).
    fn reference(readings: &[[i32; CHANNELS]], ch: usize) -> (f64, f64) {
        let n = readings.len() as f64;
        let mean = readings.iter().map(|r| f64::from(r[ch])).sum::<f64>() / n;
        let var = readings
            .iter()
            .map(|r| (f64::from(r[ch]) - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        (mean, var.sqrt())
    }

    #[test]
    fn mean_and_std_dev() {
        // Large offsets (as in real readings) plus a few counts of variation,
        // and extremes of the 24-bit range on channel 5.
        let readings: Vec<[i32; CHANNELS]> = (0..250)
            .map(|k: i32| {
                let wobble = (k * 7919) % 13 - 6;
                [
                    207_159 + wobble,
                    -21_918 + 2 * wobble,
                    0,
                    1_000_000 - wobble,
                    -8_388_608 + (k % 2),
                    if k % 2 == 0 { -8_388_608 } else { 8_388_607 },
                ]
            })
            .collect();
        let avg = accumulate(&readings);
        assert_eq!(avg.n, 250);
        for ch in 0..CHANNELS {
            let (mean, std_dev) = reference(&readings, ch);
            // f32 output: relative precision 2⁻²⁴ ≈ 6e-8.
            let mean_tol = mean.abs() * 1e-7 + 1e-6;
            assert!(
                (f64::from(avg.mean[ch]) - mean).abs() <= mean_tol,
                "mean {ch}: {} vs {mean}",
                avg.mean[ch]
            );
            assert!(
                (f64::from(avg.std_dev[ch]) - std_dev).abs() <= std_dev * 1e-6 + 1e-6,
                "std dev {ch}: {} vs {std_dev}",
                avg.std_dev[ch]
            );
        }
        assert_eq!(avg.std_dev[2], 0.0);
    }

    #[test]
    fn few_readings() {
        assert_eq!(accumulate(&[]).n, 0);
        assert_eq!(accumulate(&[]).mean, [0.0; CHANNELS]);
        let one = accumulate(&[[5; CHANNELS]]);
        assert_eq!(
            (one.n, one.mean, one.std_dev),
            (1, [5.0; CHANNELS], [0.0; CHANNELS])
        );
    }

    #[test]
    fn newton_sqrt() {
        for x in [0.0, 1e-12, 0.25, 1.0, 2.0, 9.0, 12_345.678, 4e14] {
            let expected = f64::sqrt(x);
            assert!((sqrt(x) - expected).abs() <= expected * 1e-15, "sqrt({x})");
        }
    }
}
