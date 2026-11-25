//! Applying a calibration (`w = C · (c − z)`) and fitting one from known loads.
//!
//! `wrench_from_counts` runs in the firmware for every set of readings (in
//! `f32`: the ESP32-C6 has no floating-point unit, and software `f32` is much
//! cheaper than `f64`). `fit_cell_scales` runs in the UI (calibration wizard).

use crate::{
    geometry::{A, Vec6, solve},
    interface::{CHANNELS, CalibrationMatrix, WRENCH_LEN, ZeroCounts},
};

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

/// One load case of the empirical calibration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoadCase {
    /// Known wrench applied to the platform (N, N·m about the origin), e.g. from
    /// `geometry::weight_wrench`.
    pub expected: Vec6,
    /// Measured counts minus the counts with nothing on the platform.
    pub zeroed_counts: [f64; CHANNELS],
}

/// Result of `fit_cell_scales`.
#[derive(Clone, Debug, PartialEq)]
pub struct CellFit {
    /// Fitted scale per load cell, N/count (positive = tension).
    pub scales: [f64; CHANNELS],
    /// RMS over all cases of (fitted link force − expected link force), N.
    pub rms_residual_n: [f64; CHANNELS],
    /// The calibration matrix C = A · diag(scales).
    pub matrix: CalibrationMatrix,
}

/// Why `fit_cell_scales` failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitError {
    NoCases,
    /// A cell saw (almost) no change in counts in any case: its scale can't be
    /// determined (e.g. not connected, or no case loads it).
    CellNotExcited(usize),
    /// A cell's fitted scale is zero, negative or not finite: its counts move
    /// opposite to the expected force (wiring or sign problem) or are invalid.
    BadScale(usize),
}

/// Fits one scale per load cell from known loads, keeping the geometry A fixed
/// (TODO section 2): for each cell i, the expected link forces fᵢ = (A⁻¹·w)ᵢ
/// of all cases are fitted as fᵢ ≈ kᵢ·dᵢ (d = zeroed counts) by least squares
/// through the origin: kᵢ = Σ fᵢdᵢ / Σ dᵢ². Then C = A·diag(k).
///
/// Vertical loads (weights) only excite Fz, Mx, My, so the six per-cell scales
/// are all that can be determined from them; a full 36-entry C would need
/// horizontal loads (TODO 2.6).
pub fn fit_cell_scales(cases: &[LoadCase]) -> Result<CellFit, FitError> {
    if cases.is_empty() {
        return Err(FitError::NoCases);
    }
    // Expected link forces of a case (solved again where needed: at most a few
    // hundred 6×6 solves, and no allocation, so this also works without `alloc`).
    let link_forces = |case: &LoadCase| solve(&A, &case.expected).unwrap_or([f64::NAN; 6]);

    let mut scales = [0.0; CHANNELS];
    let mut rms_residual_n = [0.0; CHANNELS];
    for cell in 0..CHANNELS {
        let (mut fd, mut dd) = (0.0, 0.0);
        for case in cases {
            let d = case.zeroed_counts[cell];
            fd += link_forces(case)[cell] * d;
            dd += d * d;
        }
        // Less than about one count of change in total: nothing to fit.
        if dd < 1.0 {
            return Err(FitError::CellNotExcited(cell));
        }
        let k = fd / dd;
        if !(k.is_finite() && k > 0.0) {
            return Err(FitError::BadScale(cell));
        }
        scales[cell] = k;
        let squares: f64 = cases
            .iter()
            .map(|case| {
                let residual = k * case.zeroed_counts[cell] - link_forces(case)[cell];
                residual * residual
            })
            .sum();
        rms_residual_n[cell] = sqrt(squares / cases.len() as f64);
    }

    let mut matrix = [[0.0_f32; CHANNELS]; WRENCH_LEN];
    for (row, out) in matrix.iter_mut().enumerate() {
        for (col, value) in out.iter_mut().enumerate() {
            *value = (A[row][col] * scales[col]) as f32;
        }
    }
    Ok(CellFit {
        scales,
        rms_residual_n,
        matrix,
    })
}

/// Square root without `std` (`f64::sqrt` is `std`-only).
fn sqrt(x: f64) -> f64 {
    crate::averaging::sqrt(x)
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

    /// The wizard's load cases: each mount alone, then every pair.
    fn wizard_cases(scales: &[f64; CHANNELS], noise_counts: f64) -> Vec<LoadCase> {
        let mut sets: Vec<Vec<usize>> = (0..6).map(|m| vec![m]).collect();
        for a in 0..6 {
            for b in a + 1..6 {
                sets.push(vec![a, b]);
            }
        }
        let mut wobble = 0.0_f64;
        sets.iter()
            .map(|mounts| {
                let mut expected = [0.0; 6];
                for &m in mounts {
                    let w = weight_wrench(MOUNTS[m], TEST_WEIGHT_KG);
                    expected.iter_mut().zip(w).for_each(|(e, v)| *e += v);
                }
                let forces = solve(&A, &expected).unwrap();
                let zeroed_counts = core::array::from_fn(|i| {
                    // Deterministic "noise" in ±noise_counts.
                    wobble = (wobble + 0.618_034) % 1.0;
                    forces[i] / scales[i] + noise_counts * (2.0 * wobble - 1.0)
                });
                LoadCase {
                    expected,
                    zeroed_counts,
                }
            })
            .collect()
    }

    /// Scales that differ per cell by up to ±5 % from nominal.
    fn true_scales() -> [f64; CHANNELS] {
        core::array::from_fn(|i| K_NOMINAL_N_PER_COUNT * (0.95 + 0.02 * i as f64))
    }

    #[test]
    fn fit_recovers_scales() {
        let truth = true_scales();
        let fit = fit_cell_scales(&wizard_cases(&truth, 0.0)).unwrap();
        for i in 0..CHANNELS {
            assert!((fit.scales[i] / truth[i] - 1.0).abs() < 1e-12, "cell {i}");
            assert!(fit.rms_residual_n[i] < 1e-9);
            for row in 0..WRENCH_LEN {
                let expected = A[row][i] * truth[i];
                assert!((f64::from(fit.matrix[row][i]) - expected).abs() <= expected.abs() * 1e-6);
            }
        }
        // Singles alone determine the scales too.
        let singles = &wizard_cases(&truth, 0.0)[..6];
        let fit = fit_cell_scales(singles).unwrap();
        assert!((0..CHANNELS).all(|i| (fit.scales[i] / truth[i] - 1.0).abs() < 1e-12));
    }

    /// With ±30 counts of noise (about 1/6 of one reading's noise; a 1 s average
    /// is quieter still): scales within 0.3 %, residuals a few mN.
    #[test]
    fn fit_with_noise() {
        let truth = true_scales();
        let fit = fit_cell_scales(&wizard_cases(&truth, 30.0)).unwrap();
        for i in 0..CHANNELS {
            assert!((fit.scales[i] / truth[i] - 1.0).abs() < 3e-3, "cell {i}");
            assert!(fit.rms_residual_n[i] > 0.0 && fit.rms_residual_n[i] < 0.005);
        }
    }

    #[test]
    fn fit_errors() {
        assert_eq!(fit_cell_scales(&[]), Err(FitError::NoCases));
        let truth = true_scales();
        let mut flipped = wizard_cases(&truth, 0.0);
        flipped.iter_mut().for_each(|c| c.zeroed_counts[2] *= -1.0);
        assert_eq!(fit_cell_scales(&flipped), Err(FitError::BadScale(2)));
        let mut dead = wizard_cases(&truth, 0.0);
        dead.iter_mut().for_each(|c| c.zeroed_counts[4] = 0.0);
        assert_eq!(fit_cell_scales(&dead), Err(FitError::CellNotExcited(4)));
    }

    #[test]
    fn finite() {
        assert!(all_finite(&[1.0, -2.0, 0.0]));
        assert!(!all_finite(&[1.0, f32::NAN]));
        assert!(!all_finite(&[f32::INFINITY]));
    }
}
