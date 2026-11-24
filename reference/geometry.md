# Platform geometry and load cell data

Source: David, 2026-11-24 (confirmed in follow-up the same day). Checked numerically by the agent (numpy) on 2026-11-24; results below.

## Frame and shape
- Units: mm for geometry (convert to m for torques in N·m). Angles are about +z, measured from +x, counter-clockwise seen from +z, in 0°–360°.
- The links lie on the 6 "waist" edges of a **regular octahedron with edge length a = 150 mm**. Its top and bottom faces are equilateral triangles (side 150 mm), centered on and normal to the z axis, rotated 60° relative to each other.
- Exact values (derived; consistent with David's approximate numbers):
  - triangle circumradius R = a/√3 = 86.60254 mm (edge offset from center R/2 = 43.30127 mm)
  - distance between the triangles h = a·√(2/3) = 122.47449 mm
- **Origin (0,0,0)**: center of the top triangle. The top surface of the platform is at z = 0; all platform geometry extends into −z. The bottom triangle is at z = −h.
- Top triangle: one edge on the +y side, parallel to x → vertices at 30°, 150°, 270°.
- Bottom triangle: one edge on the −y side, parallel to x → vertices at 90°, 210°, 330°.
- The physical link pivots are not at the octahedron vertices, but lie on the lines between them (David). That needs no correction: a link is a two-force member, so its force acts along the line through its pivots, and the moment r × F about the origin is the same for any point r on that line (moving r along the line adds a component parallel to F, whose cross product with F is zero). Not modelled: sideways shifts of the lines from slop or flex, and moments carried by joint friction; the empirical calibration partly absorbs these.

## Vertices (mm)
| Name | Angle | x | y | z |
|---|---|---|---|---|
| T30 | 30° | 75 | 43.30127 | 0 |
| T150 | 150° | −75 | 43.30127 | 0 |
| T270 | 270° | 0 | −86.60254 | 0 |
| B90 | 90° | 0 | 86.60254 | −122.47449 |
| B210 | 210° | −75 | −43.30127 | −122.47449 |
| B330 | 330° | 75 | −43.30127 | −122.47449 |

## Links (= load cells), bottom → top
Ordered counter-clockwise seen from +z, starting at B330 → T270. **Link i = `Readings::raw[i]`** (index = 2 × ADC index + channel; confirmed by David).

| Link | Bottom | Top | Midpoint angle |
|---|---|---|---|
| 0 | B330 (75, −43.30127, −122.47449) | T270 (0, −86.60254, 0) | 300° |
| 1 | B330 (75, −43.30127, −122.47449) | T30 (75, 43.30127, 0) | 0° |
| 2 | B90 (0, 86.60254, −122.47449) | T30 (75, 43.30127, 0) | 60° |
| 3 | B90 (0, 86.60254, −122.47449) | T150 (−75, 43.30127, 0) | 120° |
| 4 | B210 (−75, −43.30127, −122.47449) | T150 (−75, 43.30127, 0) | 180° |
| 5 | B210 (−75, −43.30127, −122.47449) | T270 (0, −86.60254, 0) | 240° |

Checks (passed): all 6 links and all 6 triangle edges are 150.000000 mm long; each bottom vertex is 150 mm from exactly two top vertices (its link partners) and 212.132 mm (= 150·√2) from the third; midpoint angles increase in link order (counter-clockwise seen from +z).

## Load cells and ADC
- Rated output S = 1 mV/V, capacity 98.0665 N (10 kg·g), ADC gain 128, external reference tied to the bridge excitation (3.3 V; ratiometric, so the excitation voltage cancels).
- One ADC count = V_ref / (Gain · 2²³) (datasheet: FSR = ±V_ref/Gain over ±2²³ counts). Cell output at force F = S · V_exc · F / capacity. With V_ref = V_exc:
  **k_nominal = capacity / (Gain · 2²³ · S) = 98.0665 / (128 · 8388608 · 0.001) = 9.133155e-5 N/count.**
  Full capacity = 1,073,742 counts (about 12.8 % of the ADC's ±2²³ range).
- **Sign: positive counts = tension in the link, negative = compression** (David). An upward external force on the platform is held down by the links → tension → positive counts; a weight on the platform → mostly compression → negative counts.
- Test weights: two × 1 kg. g = 9.80665 m/s² (standard; local g differs by up to about 0.5 %).

## Wrench matrix and reported wrench
- For link i: unit vector sᵢ = (topᵢ − bottomᵢ)/|…| (bottom → top), attachment point rᵢ = topᵢ (in m). A's column i = [sᵢ; rᵢ × sᵢ].
- A link in tension tᵢ (positive) pulls the platform toward its bottom pivot: force −tᵢ·sᵢ at rᵢ, so all links together exert −A·t on the platform. Static equilibrium with the external wrench w: w − A·t = 0, so **w = A·t = A · diag(k) · (c − z)**.
- **The firmware reports the external wrench applied to the platform** (David), about the origin: a weight gives Fz < 0, upward thrust gives Fz > 0. **Default C = +A · diag(k_nominal).**

```
A (rows Fx Fy Fz Mx My Mz [N, N·m]; columns = links 0..5):
[[-0.5       0.        0.5      -0.5       0.        0.5     ]
 [-0.288675  0.57735  -0.288675 -0.288675  0.57735  -0.288675]
 [ 0.816497  0.816497  0.816497  0.816497  0.816497  0.816497]
 [-0.070711  0.035355  0.035355  0.035355  0.035355 -0.070711]
 [ 0.       -0.061237 -0.061237  0.061237  0.061237  0.      ]
 [-0.043301  0.043301 -0.043301  0.043301 -0.043301  0.043301]]
```
- det A = −0.0023865; singular values 2, 1.0019, 1.0019, 0.10607, 0.10587, 0.10587. Condition number 18.9 in raw N / N·m units, **2.51** with torques scaled by R (mixed units make the raw number misleading). Far from singular: every force/torque direction is measured well.

## Calibration-weight mounts
A 1 kg weight applies a downward (−z) force centered at one of 6 points: M0 at (0, −50 mm) and the rest at 60° steps (radius 50 mm). One weight per mount → at most 6 single + 15 pairs = 21 load cases. For vertical loads only x, y matter, not the height of the weight.

| Mount | Angle | x (mm) | y (mm) |
|---|---|---|---|
| M0 | 270° | 0 | −50 |
| M1 | 330° | 43.30127 | −25 |
| M2 | 30° | 43.30127 | 25 |
| M3 | 90° | 0 | 50 |
| M4 | 150° | −43.30127 | 25 |
| M5 | 210° | −43.30127 | −25 |

Expected link tensions for 1 kg, in N (positive = tension), columns = links 0..5 (for checking the calibration wizard):
```
center: -2.0018 -2.0018 -2.0018 -2.0018 -2.0018 -2.0018
M0:     -4.3132 -2.0018  0.3097  0.3097 -2.0018 -4.3132
M1:     -4.3132 -4.3132 -2.0018  0.3097  0.3097 -2.0018
M2:     -2.0018 -4.3132 -4.3132 -2.0018  0.3097  0.3097
M3:      0.3097 -2.0018 -4.3132 -4.3132 -2.0018  0.3097
M4:      0.3097  0.3097 -2.0018 -4.3132 -4.3132 -2.0018
M5:     -2.0018  0.3097  0.3097 -2.0018 -4.3132 -4.3132
```
(Each mount's row is the previous one shifted by one link: the 60° symmetry.)

Per-cell fit over all 21 cases: every link is excited equally (Σ t² = 371.3 N², max |t| = 8.63 N ≈ 94,452 counts, 2 cases each with |t| < 0.5 N), so all six kᵢ are equally well determined. The cases are almost all compression (largest tension 0.62 N), so the fit says little about the tension side; strain-gauge load cells are normally close to symmetric, but that is an assumption.

Not specified: mass of any weight-mounting hardware (doesn't matter if the stand is zeroed with it fitted).
