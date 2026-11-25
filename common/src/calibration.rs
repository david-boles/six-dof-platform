//! Applying a calibration: `w = C · (c − z)`.
//!
//! Runs in the firmware for every set of readings (in `f32`: the ESP32-C6 has
//! no floating-point unit, and software `f32` is much cheaper than `f64`).

use crate::interface::{CHANNELS, CalibrationMatrix, WRENCH_LEN, ZeroCounts};

/// The wrench `[Fx, Fy, Fz, Mx, My, Mz]` (N, N·m) for raw counts `raw`:
/// `matrix · (raw − zero)`.
///
/// Precision: `raw` is a 24-bit count, which `f32` (24-bit mantissa) holds
/// exactly; `zero` is held to within 2⁻⁶ ≈ 0.016 counts up to ±262,144 counts
/// (and 0.5 counts at the ADC's full ±2²³).
pub fn wrench_from_counts(
    matrix: &CalibrationMatrix,
    zero: &ZeroCounts,
    raw: &[i32; CHANNELS],
) -> [f32; WRENCH_LEN] {
    let mut zeroed = [0.0_f32; CHANNELS];
    for ((zeroed, &raw), &zero) in zeroed.iter_mut().zip(raw).zip(zero) {
        *zeroed = raw as f32 - zero;
    }
    let mut wrench = [0.0_f32; WRENCH_LEN];
    for (component, row) in wrench.iter_mut().zip(matrix) {
        *component = row.iter().zip(&zeroed).map(|(c, z)| c * z).sum();
    }
    wrench
}

/// True if every value is finite (no NaN or infinity).
pub fn all_finite<'a>(values: impl IntoIterator<Item = &'a f32>) -> bool {
    values.into_iter().all(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{
        A, DEFAULT_C, K_NOMINAL_N_PER_COUNT, MOUNTS, TEST_WEIGHT_KG, solve, weight_wrench,
    };

    /// Counts the cells would show (with the nominal scale) for a wrench.
    fn counts_for(wrench: &[f64; 6]) -> [i32; CHANNELS] {
        let tensions = solve(&A, wrench).unwrap();
        tensions.map(|t| (t / K_NOMINAL_N_PER_COUNT).round() as i32)
    }

    fn assert_close(actual: &[f32], expected: &[f64], tol: f64) {
        for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (*a as f64 - e).abs() <= tol,
                "component {i}: {a} vs. expected {e}"
            );
        }
    }

    /// Whole sign chain: a weight → negative counts (compression) → the default
    /// calibration reports Fz = −m·g and the weight's torques.
    #[test]
    fn default_calibration_round_trip() {
        let zero = [0.0; CHANNELS];
        for point in [[0.0, 0.0], MOUNTS[0], MOUNTS[2]] {
            let expected = weight_wrench(point, TEST_WEIGHT_KG);
            let raw = counts_for(&expected);
            assert!(raw.iter().any(|&c| c < 0));
            let w = wrench_from_counts(&DEFAULT_C, &zero, &raw);
            // Rounding counts to integers: < 1e-4 N / 1e-5 N·m here.
            assert_close(&w[..3], &expected[..3], 1e-3);
            assert_close(&w[3..], &expected[3..], 1e-4);
        }
    }

    /// Zero offsets are subtracted before applying C (including fractions).
    #[test]
    fn zero_offset() {
        let offset: ZeroCounts = [207_159.25, -1000.5, 0.0, 12.0, 5.0e5, -3.0e5];
        let expected = weight_wrench(MOUNTS[3], TEST_WEIGHT_KG);
        let loaded = counts_for(&expected);
        let raw: [i32; CHANNELS] = core::array::from_fn(|i| loaded[i] + offset[i].round() as i32);
        let zero: ZeroCounts = core::array::from_fn(|i| offset[i].round());
        let w = wrench_from_counts(&DEFAULT_C, &zero, &raw);
        assert_close(&w[..3], &expected[..3], 1e-3);
        assert_close(&w[3..], &expected[3..], 1e-4);
    }

    #[test]
    fn finite() {
        assert!(all_finite(&[1.0, -2.0, 0.0]));
        assert!(!all_finite(&[1.0, f32::NAN]));
        assert!(!all_finite(&[f32::INFINITY]));
    }
}
