//! Platform geometry, load cell data, the ideal wrench matrix `A` and the default
//! calibration matrix `C`.
//!
//! Source, derivation and numerical checks: `reference/geometry.md`. Summary:
//! - Frame: origin at the center of the top triangle (top surface of the
//!   platform, z = 0; the stand extends into −z). Lengths in mm, angles about +z
//!   from +x.
//! - The 6 links are the "waist" edges of a regular octahedron (edge 150 mm)
//!   whose top and bottom faces are horizontal triangles. Link `i` is load cell
//!   `i`, i.e. `Readings::raw[i]`.
//! - Positive counts = tension in the link.
//! - Wrenches are `[Fx, Fy, Fz, Mx, My, Mz]` in N and N·m: the **external**
//!   wrench applied to the platform, torques about the origin.
//!
//! Everything here is `const` (evaluated by the compiler), so the firmware gets
//! the default calibration as a table in flash.

use crate::interface::{CHANNELS, CalibrationMatrix, WRENCH_LEN};

/// 3D vector (mm or m, see the item's documentation).
pub type Vec3 = [f64; 3];
/// Wrench `[Fx, Fy, Fz, Mx, My, Mz]` (N, N·m) or one value per link.
pub type Vec6 = [f64; 6];
/// 6×6 matrix, row-major: `m[row][column]`.
pub type Mat6 = [[f64; 6]; 6];

// `sqrt` isn't available in `const` expressions and `core::f64::consts::SQRT_3`
// is unstable, so these are literals (checked by the `constants` test).
/// √3
const SQRT_3: f64 = 1.732_050_807_568_877_2;
/// √(2/3)
const SQRT_2_3: f64 = 0.816_496_580_927_726;

// ---------------------------------------------------------------------------
// Octahedron and links
// ---------------------------------------------------------------------------

/// Edge length of the octahedron = length of every link (pivot line) and every
/// side of the top and bottom triangles, in mm.
pub const EDGE_MM: f64 = 150.0;
/// Distance from the z axis to each triangle vertex (circumradius), in mm.
pub const CIRCUMRADIUS_MM: f64 = EDGE_MM / SQRT_3;
/// Distance between the top (z = 0) and bottom triangles, in mm.
pub const HEIGHT_MM: f64 = EDGE_MM * SQRT_2_3;

// A vertex at angle θ is (CIRCUMRADIUS·cos θ, CIRCUMRADIUS·sin θ). For the
// angles used here, CIRCUMRADIUS·cos 30° = CIRCUMRADIUS·√3/2 = EDGE/2 and
// CIRCUMRADIUS·sin 30° = CIRCUMRADIUS/2.
const HALF_EDGE_MM: f64 = EDGE_MM / 2.0;
const HALF_CIRCUMRADIUS_MM: f64 = CIRCUMRADIUS_MM / 2.0;

/// Top triangle vertex at 30° (mm).
pub const T30: Vec3 = [HALF_EDGE_MM, HALF_CIRCUMRADIUS_MM, 0.0];
/// Top triangle vertex at 150° (mm).
pub const T150: Vec3 = [-HALF_EDGE_MM, HALF_CIRCUMRADIUS_MM, 0.0];
/// Top triangle vertex at 270° (mm).
pub const T270: Vec3 = [0.0, -CIRCUMRADIUS_MM, 0.0];
/// Bottom triangle vertex at 90° (mm).
pub const B90: Vec3 = [0.0, CIRCUMRADIUS_MM, -HEIGHT_MM];
/// Bottom triangle vertex at 210° (mm).
pub const B210: Vec3 = [-HALF_EDGE_MM, -HALF_CIRCUMRADIUS_MM, -HEIGHT_MM];
/// Bottom triangle vertex at 330° (mm).
pub const B330: Vec3 = [HALF_EDGE_MM, -HALF_CIRCUMRADIUS_MM, -HEIGHT_MM];

/// One link: the line between its pivots (the physical pivots lie on this
/// line, which is all that matters for a two-force member).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    /// Pivot on the base (mm).
    pub bottom: Vec3,
    /// Pivot on the platform (mm); the link's force acts on the platform here.
    pub top: Vec3,
}

/// The links in load cell order: counter-clockwise seen from +z, starting with
/// B330 → T270. Index = `Readings::raw` index.
pub const LINKS: [Link; CHANNELS] = [
    Link {
        bottom: B330,
        top: T270,
    },
    Link {
        bottom: B330,
        top: T30,
    },
    Link {
        bottom: B90,
        top: T30,
    },
    Link {
        bottom: B90,
        top: T150,
    },
    Link {
        bottom: B210,
        top: T150,
    },
    Link {
        bottom: B210,
        top: T270,
    },
];

// ---------------------------------------------------------------------------
// Calibration weights
// ---------------------------------------------------------------------------

/// Distance of the calibration-weight mounts from the z axis, in mm.
pub const MOUNT_RADIUS_MM: f64 = 50.0;

// Mount coordinates: MOUNT_RADIUS·cos 30° and MOUNT_RADIUS·sin 30°.
const MOUNT_RADIUS_COS_30_MM: f64 = MOUNT_RADIUS_MM * SQRT_3 / 2.0;
const MOUNT_RADIUS_SIN_30_MM: f64 = MOUNT_RADIUS_MM / 2.0;

/// (x, y) in mm of the 6 calibration-weight mounts M0..M5: M0 at 270°
/// (0, −50 mm), then counter-clockwise in 60° steps. A weight applies a
/// downward force there; for vertical forces the height doesn't matter.
pub const MOUNTS: [[f64; 2]; 6] = [
    [0.0, -MOUNT_RADIUS_MM],
    [MOUNT_RADIUS_COS_30_MM, -MOUNT_RADIUS_SIN_30_MM],
    [MOUNT_RADIUS_COS_30_MM, MOUNT_RADIUS_SIN_30_MM],
    [0.0, MOUNT_RADIUS_MM],
    [-MOUNT_RADIUS_COS_30_MM, MOUNT_RADIUS_SIN_30_MM],
    [-MOUNT_RADIUS_COS_30_MM, -MOUNT_RADIUS_SIN_30_MM],
];

/// Mass of each calibration weight, in kg (there are two).
pub const TEST_WEIGHT_KG: f64 = 1.0;
/// Standard gravity, m/s². Local g differs by up to about 0.5 %.
pub const STANDARD_GRAVITY: f64 = 9.806_65;

// ---------------------------------------------------------------------------
// Load cells and ADC
// ---------------------------------------------------------------------------

/// Load cell capacity in N (10 kg·g).
pub const CELL_CAPACITY_N: f64 = 98.0665;
/// Load cell rated output at capacity, in V per V of excitation (1 mV/V).
pub const CELL_RATED_OUTPUT_V_PER_V: f64 = 1e-3;
/// ADC programmable gain. KEEP IN SYNC with `Gain::G128` in
/// `firmware/src/input_adcs.rs`.
pub const ADC_GAIN: f64 = 128.0;
/// ADC counts for a differential input of +V_ref / gain (24-bit two's complement:
/// ±2²³ counts span ±V_ref / gain).
pub const ADC_FULL_SCALE_COUNTS: f64 = 8_388_608.0;

/// Nominal load cell scale in N per ADC count (positive = tension), from the
/// datasheet values above. The ADC reference is the bridge excitation
/// (ratiometric), so the excitation voltage cancels:
/// counts = F / capacity · S · V_exc / (V_ref / gain) · 2²³ with V_ref = V_exc.
pub const K_NOMINAL_N_PER_COUNT: f64 =
    CELL_CAPACITY_N / (ADC_GAIN * ADC_FULL_SCALE_COUNTS * CELL_RATED_OUTPUT_V_PER_V);

// ---------------------------------------------------------------------------
// Wrench matrix and default calibration
// ---------------------------------------------------------------------------

/// Ideal wrench matrix: `w = A · t` gives the external wrench on the platform
/// (N, N·m about the origin) that is balanced by the link tensions `t` (N,
/// positive = tension).
///
/// Column `i` is `[sᵢ; rᵢ × sᵢ]` with `sᵢ` the unit vector of link `i` from
/// bottom to top and `rᵢ` its top pivot in m. Why `+`: a link in tension `tᵢ`
/// pulls the platform toward its bottom pivot (force `−tᵢ·sᵢ` at `rᵢ`), so the
/// links exert `−A·t` and equilibrium needs the external wrench `w = A·t`.
pub const A: Mat6 = wrench_matrix(&LINKS);

/// Default calibration matrix (used by the firmware until the UI sets one):
/// `w = C · (c − z)` with `c` the raw counts and `z` the zero offsets.
/// `C = A · diag(K_NOMINAL_N_PER_COUNT)`; rows 0–2 in N/count, rows 3–5 in
/// N·m/count. `f32` because that's what the firmware computes with.
pub const DEFAULT_C: CalibrationMatrix = default_calibration();

const fn wrench_matrix(links: &[Link; CHANNELS]) -> Mat6 {
    let mut a = [[0.0; 6]; 6];
    let mut i = 0;
    while i < CHANNELS {
        // Every link is exactly one octahedron edge long (checked by the
        // `octahedron` test), so no `sqrt` (not available in `const`) is needed.
        let s = scale(sub(links[i].top, links[i].bottom), 1.0 / EDGE_MM);
        let m = cross(scale(links[i].top, 1e-3), s);
        let mut row = 0;
        while row < 3 {
            a[row][i] = s[row];
            a[row + 3][i] = m[row];
            row += 1;
        }
        i += 1;
    }
    a
}

const fn default_calibration() -> CalibrationMatrix {
    let mut c = [[0.0; CHANNELS]; WRENCH_LEN];
    let mut row = 0;
    while row < WRENCH_LEN {
        let mut col = 0;
        while col < CHANNELS {
            c[row][col] = (A[row][col] * K_NOMINAL_N_PER_COUNT) as f32;
            col += 1;
        }
        row += 1;
    }
    c
}

/// Wrench (N, N·m about the origin) of `force` (N) applied at `point_mm`.
pub const fn point_load_wrench(point_mm: Vec3, force: Vec3) -> Vec6 {
    let m = cross(scale(point_mm, 1e-3), force);
    [force[0], force[1], force[2], m[0], m[1], m[2]]
}

/// Wrench (N, N·m about the origin) of a weight of `mass_kg` resting on a mount
/// at `mount_mm` (x, y): a downward force of `mass_kg · STANDARD_GRAVITY`.
pub const fn weight_wrench(mount_mm: [f64; 2], mass_kg: f64) -> Vec6 {
    point_load_wrench(
        [mount_mm[0], mount_mm[1], 0.0],
        [0.0, 0.0, -mass_kg * STANDARD_GRAVITY],
    )
}

// ---------------------------------------------------------------------------
// Small linear algebra helpers
// ---------------------------------------------------------------------------

/// `m · v`.
pub const fn mat_vec(m: &Mat6, v: &Vec6) -> Vec6 {
    let mut out = [0.0; 6];
    let mut row = 0;
    while row < 6 {
        let mut col = 0;
        while col < 6 {
            out[row] += m[row][col] * v[col];
            col += 1;
        }
        row += 1;
    }
    out
}

/// Solves `m · x = b` by Gaussian elimination with partial pivoting. Returns
/// `None` if `m` is singular (a pivot is smaller than 1e-12 × the largest
/// entry of `m`). Use it as `solve(&A, &w)` to get the link tensions for a
/// wrench (calibration).
///
/// How it works: for each column, swap the row with the largest remaining
/// entry in that column to the diagonal (pivoting; avoids dividing by small
/// numbers, which would amplify rounding errors), then subtract multiples of
/// it from the rows below so the column becomes zero under the diagonal. The
/// result is upper triangular and is solved from the last row upwards.
pub fn solve(m: &Mat6, b: &Vec6) -> Option<Vec6> {
    let mut m = *m;
    let mut b = *b;
    let largest = m
        .iter()
        .flatten()
        .fold(0.0_f64, |acc, x| if x.abs() > acc { x.abs() } else { acc });
    let tolerance = 1e-12 * largest;

    for col in 0..6 {
        let pivot = (col..6)
            .max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))
            .unwrap_or(col);
        if m[pivot][col].abs() <= tolerance {
            return None;
        }
        m.swap(col, pivot);
        b.swap(col, pivot);
        for row in col + 1..6 {
            let factor = m[row][col] / m[col][col];
            for k in col..6 {
                m[row][k] -= factor * m[col][k];
            }
            b[row] -= factor * b[col];
        }
    }

    let mut x = [0.0; 6];
    for row in (0..6).rev() {
        let mut sum = b[row];
        for k in row + 1..6 {
            sum -= m[row][k] * x[k];
        }
        x[row] = sum / m[row][row];
    }
    Some(x)
}

const fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

const fn scale(a: Vec3, f: f64) -> Vec3 {
    [a[0] * f, a[1] * f, a[2] * f]
}

const fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Values checked against `reference/geometry.md` (computed independently with
/// numpy). Tolerances match the number of digits printed there.
#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: Vec3, b: Vec3) -> f64 {
        let d = sub(a, b);
        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
    }

    fn assert_close(actual: &[f64], expected: &[f64], tol: f64, what: &str) {
        assert_eq!(actual.len(), expected.len(), "{what}: length");
        for (i, (a, e)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (a - e).abs() <= tol,
                "{what}[{i}]: {a} vs. expected {e} (tolerance {tol})"
            );
        }
    }

    #[test]
    fn constants() {
        assert!((SQRT_3 - 3.0_f64.sqrt()).abs() < 1e-15);
        assert!((SQRT_2_3 - (2.0_f64 / 3.0).sqrt()).abs() < 1e-15);
        assert!((CIRCUMRADIUS_MM - 86.60254).abs() < 1e-5);
        assert!((HEIGHT_MM - 122.47449).abs() < 1e-5);
    }

    #[test]
    fn octahedron() {
        // All links and all triangle sides are one edge long (`wrench_matrix`
        // relies on the link lengths).
        for (i, link) in LINKS.iter().enumerate() {
            let len = dist(link.top, link.bottom);
            assert!((len - EDGE_MM).abs() < 1e-9, "link {i} length {len}");
        }
        for tri in [[T30, T150, T270], [B90, B210, B330]] {
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                assert!((dist(tri[a], tri[b]) - EDGE_MM).abs() < 1e-9);
            }
        }
        // Link order: midpoints at 300°, 0°, 60°, ..., i.e. counter-clockwise.
        for (i, link) in LINKS.iter().enumerate() {
            let mid = scale(add(link.top, link.bottom), 0.5);
            let angle = mid[1].atan2(mid[0]).to_degrees().rem_euclid(360.0);
            let expected = (300.0 + 60.0 * i as f64) % 360.0;
            assert!((angle - expected).abs() < 1e-9, "link {i} at {angle}°");
        }
        // Link 0 as stated by David.
        assert_close(
            &LINKS[0].bottom,
            &[75.0, -43.30127, -122.47449],
            1e-5,
            "link 0 bottom",
        );
        assert_close(&LINKS[0].top, &[0.0, -86.60254, 0.0], 1e-5, "link 0 top");
    }

    fn add(a: Vec3, b: Vec3) -> Vec3 {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    #[test]
    fn wrench_matrix_matches_reference() {
        let expected: Mat6 = [
            [-0.5, 0.0, 0.5, -0.5, 0.0, 0.5],
            [-0.288675, 0.57735, -0.288675, -0.288675, 0.57735, -0.288675],
            [0.816497, 0.816497, 0.816497, 0.816497, 0.816497, 0.816497],
            [-0.070711, 0.035355, 0.035355, 0.035355, 0.035355, -0.070711],
            [0.0, -0.061237, -0.061237, 0.061237, 0.061237, 0.0],
            [
                -0.043301, 0.043301, -0.043301, 0.043301, -0.043301, 0.043301,
            ],
        ];
        for row in 0..6 {
            assert_close(&A[row], &expected[row], 1e-6, &format!("A row {row}"));
        }
    }

    #[test]
    fn nominal_scale_and_default_c() {
        assert!((K_NOMINAL_N_PER_COUNT / 9.133155e-5 - 1.0).abs() < 1e-6);
        for row in 0..6 {
            for col in 0..CHANNELS {
                let expected = A[row][col] * K_NOMINAL_N_PER_COUNT;
                let actual = DEFAULT_C[row][col] as f64;
                // f32 has a 24-bit mantissa: relative error < 2⁻²⁴ ≈ 6e-8.
                assert!((actual - expected).abs() <= expected.abs() * 1e-7);
            }
        }
    }

    #[test]
    fn mounts() {
        let expected = [
            [0.0, -50.0],
            [43.30127, -25.0],
            [43.30127, 25.0],
            [0.0, 50.0],
            [-43.30127, 25.0],
            [-43.30127, -25.0],
        ];
        for (i, m) in MOUNTS.iter().enumerate() {
            assert_close(m, &expected[i], 1e-5, &format!("mount {i}"));
            assert!((m[0].hypot(m[1]) - MOUNT_RADIUS_MM).abs() < 1e-9);
        }
    }

    /// Link tensions for 1 kg at the center and on each mount (the table in
    /// `reference/geometry.md`).
    #[test]
    fn weight_tensions() {
        let center = solve(&A, &weight_wrench([0.0, 0.0], TEST_WEIGHT_KG)).unwrap();
        assert_close(&center, &[-2.0018; 6], 1e-4, "center");

        let expected = [
            [-4.3132, -2.0018, 0.3097, 0.3097, -2.0018, -4.3132],
            [-4.3132, -4.3132, -2.0018, 0.3097, 0.3097, -2.0018],
            [-2.0018, -4.3132, -4.3132, -2.0018, 0.3097, 0.3097],
            [0.3097, -2.0018, -4.3132, -4.3132, -2.0018, 0.3097],
            [0.3097, 0.3097, -2.0018, -4.3132, -4.3132, -2.0018],
            [-2.0018, 0.3097, 0.3097, -2.0018, -4.3132, -4.3132],
        ];
        for (i, mount) in MOUNTS.iter().enumerate() {
            let t = solve(&A, &weight_wrench(*mount, TEST_WEIGHT_KG)).unwrap();
            assert_close(&t, &expected[i], 1e-4, &format!("mount {i}"));
        }
    }

    #[test]
    fn solve_round_trip_and_singular() {
        let b = [1.0, -2.0, 3.0, 0.1, -0.2, 0.3];
        let x = solve(&A, &b).unwrap();
        assert_close(&mat_vec(&A, &x), &b, 1e-12, "A·x");

        let mut singular = A;
        singular[5] = singular[4];
        assert_eq!(solve(&singular, &b), None);
    }
}
