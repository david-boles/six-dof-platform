//! Calibration wizard (TODO 3.5): fits one scale per load cell from known
//! weights on the platform's mounts (`calibration::fit_cell_scales`).
//!
//! Steps: settings → capture with the platform empty (the wizard's own zero; the
//! device's zero offsets aren't used) → for each load case, place the weight(s)
//! shown in the 3D view and capture → capture empty again (drift check) →
//! results, apply to the device and/or save in the browser.
//!
//! Every capture is the device's 1 s `average` of raw counts, so the result
//! doesn't depend on the calibration or zero the device currently uses.

use leptos::{prelude::*, task::spawn_local};
use six_dof_platform_common::{
    calibration::{CellFit, FitError, LoadCase, fit_cell_scales},
    geometry::{
        K_NOMINAL_N_PER_COUNT, MOUNTS, STANDARD_GRAVITY, TEST_WEIGHT_KG, center_of_mass,
        weight_wrench,
    },
    interface::{AverageCounts, CHANNELS},
};

use crate::{
    DeviceCalibration, request_average, serial::Client, set_matrix, storage,
    storage::SavedCalibration,
};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Setup,
    EmptyBefore,
    Case(usize),
    EmptyAfter,
    Results,
}

/// Everything captured in one run.
#[derive(Clone, Debug, Default, PartialEq)]
struct Run {
    mass_kg: f64,
    /// Mounts loaded in each case (one or two weights).
    cases: Vec<Vec<usize>>,
    empty_before: Option<AverageCounts>,
    captures: Vec<AverageCounts>,
    empty_after: Option<AverageCounts>,
}

impl Run {
    fn new(mass_kg: f64, pairs: bool) -> Self {
        let mut cases: Vec<Vec<usize>> = (0..MOUNTS.len()).map(|m| vec![m]).collect();
        if pairs {
            for a in 0..MOUNTS.len() {
                for b in a + 1..MOUNTS.len() {
                    cases.push(vec![a, b]);
                }
            }
        }
        Self {
            mass_kg,
            cases,
            ..Default::default()
        }
    }

    /// Known wrench of a case (the weights on their mounts).
    fn expected(&self, mounts: &[usize]) -> [f64; 6] {
        let mut total = [0.0; 6];
        for &m in mounts {
            let w = weight_wrench(MOUNTS[m], self.mass_kg);
            total.iter_mut().zip(w).for_each(|(t, v)| *t += v);
        }
        total
    }

    fn load_cases(&self) -> Option<Vec<LoadCase>> {
        let empty = self.empty_before.as_ref()?;
        Some(
            self.cases
                .iter()
                .zip(&self.captures)
                .map(|(mounts, capture)| LoadCase {
                    expected: self.expected(mounts),
                    zeroed_counts: std::array::from_fn(|i| {
                        f64::from(capture.mean[i]) - f64::from(empty.mean[i])
                    }),
                })
                .collect(),
        )
    }
}

/// Mount names as printed on the page: M0..M5.
fn mount_list(mounts: &[usize]) -> String {
    let names: Vec<String> = mounts.iter().map(|m| format!("M{m}")).collect();
    names.join(" and ")
}

#[component]
pub fn Wizard(
    /// The current client, if connected.
    client: impl Fn() -> Option<Client> + Copy + Send + Sync + 'static,
    /// Mounts the 3D view shows weights on.
    weights: RwSignal<Vec<usize>>,
    device_calibration: RwSignal<Option<DeviceCalibration>>,
    saved: RwSignal<Option<SavedCalibration>>,
) -> impl IntoView {
    let step = RwSignal::new(Step::Setup);
    let run = RwSignal::new(Run::default());
    let capturing = RwSignal::new(false);
    // Outcome of the last capture or action (Ok = info, Err = error).
    let message = RwSignal::new(None::<Result<String, String>>);
    let mass_g = RwSignal::new(TEST_WEIGHT_KG * 1000.0);
    let pairs = RwSignal::new(false);

    // Keep the 3D view's weights in step with the wizard.
    Effect::new(move || {
        let shown = match step.get() {
            Step::Case(i) => run.with(|r| r.cases.get(i).cloned().unwrap_or_default()),
            _ => Vec::new(),
        };
        weights.set(shown);
    });

    let on_start = move |_| {
        let mass_kg = mass_g.get_untracked() / 1000.0;
        if !(mass_kg.is_finite() && mass_kg > 0.0) {
            message.set(Some(Err("enter the mass of the weight".into())));
            return;
        }
        run.set(Run::new(mass_kg, pairs.get_untracked()));
        message.set(None);
        step.set(Step::EmptyBefore);
    };

    let on_capture = move |_| {
        let Some(client) = client() else {
            message.set(Some(Err("not connected".into())));
            return;
        };
        capturing.set(true);
        spawn_local(async move {
            let result = request_average(&client).await;
            capturing.set(false);
            let average = match result {
                Ok(a) => a,
                Err(e) => {
                    message.set(Some(Err(format!("capture failed: {e}; try again"))));
                    return;
                }
            };
            let noise = average.std_dev.iter().fold(0.0_f32, |a, &b| a.max(b));
            message.set(Some(Ok(format!(
                "captured ({} readings, noise up to {noise:.0} counts RMS).",
                average.n
            ))));
            let next = match step.get_untracked() {
                Step::EmptyBefore => {
                    run.update(|r| r.empty_before = Some(average));
                    Step::Case(0)
                }
                Step::Case(i) => {
                    run.update(|r| r.captures.push(average));
                    if i + 1 < run.with_untracked(|r| r.cases.len()) {
                        Step::Case(i + 1)
                    } else {
                        Step::EmptyAfter
                    }
                }
                Step::EmptyAfter => {
                    run.update(|r| r.empty_after = Some(average));
                    Step::Results
                }
                other => other,
            };
            step.set(next);
        });
    };

    // One step back (redo the previous capture).
    let on_back = move |_| {
        message.set(None);
        let previous = match step.get_untracked() {
            Step::EmptyBefore => Step::Setup,
            Step::Case(0) => {
                run.update(|r| r.empty_before = None);
                Step::EmptyBefore
            }
            Step::Case(i) => {
                run.update(|r| {
                    r.captures.pop();
                });
                Step::Case(i - 1)
            }
            Step::EmptyAfter => {
                run.update(|r| {
                    r.captures.pop();
                });
                Step::Case(run.with_untracked(|r| r.cases.len()) - 1)
            }
            Step::Results => {
                run.update(|r| r.empty_after = None);
                Step::EmptyAfter
            }
            Step::Setup => Step::Setup,
        };
        step.set(previous);
    };
    let on_cancel = move |_| {
        message.set(None);
        step.set(Step::Setup);
    };

    // The fit, once all captures are in.
    let fit = Memo::new(move |_| {
        if step.get() != Step::Results {
            return None;
        }
        Some(run.with(|r| r.load_cases().map(|cases| fit_cell_scales(&cases)))).flatten()
    });

    let on_apply = move |_| {
        let (Some(Ok(fit)), Some(client)) = (fit.get_untracked(), client()) else {
            return;
        };
        let matrix = fit.matrix;
        spawn_local(async move {
            match set_matrix(&client, matrix).await {
                Ok(()) => {
                    device_calibration.update(|d| {
                        if let Some(d) = d {
                            d.matrix = matrix;
                        }
                    });
                    // The zero offsets stay valid: they're raw counts, subtracted
                    // before C is applied (w = C · (c − z)).
                    message.set(Some(Ok("applied to the device (until it's reset).".into())));
                }
                Err(e) => message.set(Some(Err(format!("applying failed: {e}")))),
            }
        });
    };
    let on_save = move |_| {
        let Some(Ok(fit)) = fit.get_untracked() else {
            return;
        };
        let note = run.with_untracked(|r| {
            format!(
                "calibration wizard: {} cases, {:.1} g per weight",
                r.cases.len(),
                r.mass_kg * 1000.0
            )
        });
        let new = SavedCalibration::new(fit.matrix, note);
        message.set(Some(match storage::store_calibration(&new) {
            Ok(()) => {
                saved.set(Some(new));
                Ok("saved in this browser.".into())
            }
            Err(e) => Err(e),
        }));
    };

    let connected = move || device_calibration.with(Option::is_some);

    view! {
        <h3>"Calibration wizard"</h3>
        <p class="hint">
            "Fits one scale per load cell from known weights on the mounts M0\u{2013}M5 (M0 at \
             (0, \u{2212}50) mm, then counter-clockwise seen from above, every 60\u{b0}), keeping the \
             CAD geometry. Each step is a 1 s average; keep the stand still while capturing."
        </p>
        {move || match step.get() {
            Step::Setup => {
                view! {
                    <div class="row">
                        <label class="inline">
                            "Mass of each weight (g)"
                            <input
                                type="number"
                                step="any"
                                prop:value=move || mass_g.get().to_string()
                                on:change=move |ev| {
                                    if let Ok(v) = event_target_value(&ev).trim().parse::<f64>() {
                                        mass_g.set(v);
                                    }
                                }
                            />
                        </label>
                        <label class="inline">
                            <input
                                type="checkbox"
                                prop:checked=move || pairs.get()
                                on:change=move |ev| pairs.set(event_target_checked(&ev))
                            />
                            "also pairs of weights (21 cases instead of 6; needs two weights)"
                        </label>
                    </div>
                    <div class="row">
                        <button class="primary" on:click=on_start disabled=move || !connected()>
                            "Start"
                        </button>
                    </div>
                }
                    .into_any()
            }
            Step::Results => view! { <Results run=run fit=fit /> }.into_any(),
            current => {
                let instruction = match current {
                    Step::EmptyBefore => {
                        "Remove everything from the platform, then capture.".to_string()
                    }
                    Step::Case(i) => {
                        let (mounts, total) = run
                            .with_untracked(|r| (r.cases.get(i).cloned().unwrap_or_default(), r.cases.len()));
                        let what = if mounts.len() == 1 { "a weight" } else { "the weights" };
                        format!(
                            "Case {} of {total}: place {what} on {} (see the 3D view), \
                             nothing else on the platform, then capture.",
                            i + 1,
                            mount_list(&mounts),
                        )
                    }
                    _ => "Remove the weights, then capture (checks drift).".to_string(),
                };
                view! {
                    <p class="notice">{instruction}</p>
                    <div class="row">
                        <button
                            class="primary"
                            on:click=on_capture
                            disabled=move || capturing.get() || !connected()
                        >
                            {move || if capturing.get() { "Capturing…" } else { "Capture" }}
                        </button>
                        <button on:click=on_back disabled=move || capturing.get()>
                            "Back"
                        </button>
                        <button on:click=on_cancel disabled=move || capturing.get()>
                            "Cancel"
                        </button>
                    </div>
                }
                    .into_any()
            }
        }}
        <Show when=move || step.get() == Step::Results && matches!(fit.get(), Some(Ok(_)))>
            <div class="row">
                <button class="primary" on:click=on_apply disabled=move || !connected()>
                    "Apply to the device"
                </button>
                <button on:click=on_save>"Save in this browser"</button>
                <button on:click=on_back>"Back"</button>
                <button on:click=on_cancel>"Start over"</button>
            </div>
        </Show>
        // Back is also needed when the fit failed (to redo captures).
        <Show when=move || step.get() == Step::Results && matches!(fit.get(), Some(Err(_)))>
            <div class="row">
                <button on:click=on_back>"Back"</button>
                <button on:click=on_cancel>"Start over"</button>
            </div>
        </Show>
        <p class=move || match message.get() {
            Some(Err(_)) => "message error",
            Some(Ok(_)) => "message ok",
            None => "message",
        }>
            {move || match message.get() {
                None => String::new(),
                Some(Ok(m)) => m,
                Some(Err(e)) => format!("Error: {e}"),
            }}
        </p>
    }
}

/// Fit results: per cell and per case.
#[component]
fn Results(run: RwSignal<Run>, fit: Memo<Option<Result<CellFit, FitError>>>) -> impl IntoView {
    move || {
        let fit = match fit.get() {
            Some(Ok(fit)) => fit,
            Some(Err(e)) => {
                let text = match e {
                    FitError::NoCases => "no load cases captured".to_string(),
                    FitError::CellNotExcited(i) => {
                        format!("cell {i} didn't change in any case (connected?)")
                    }
                    FitError::BadScale(i) => format!(
                        "cell {i}'s counts move opposite to the expected force (wiring, sign, \
                         or a weight on the wrong mount?)"
                    ),
                };
                return view! {
                    <p class="message error">
                        {format!("The fit failed: {text}. Use Back to redo captures.")}
                    </p>
                }
                .into_any();
            }
            None => return ().into_any(),
        };
        let r = run.get();
        let drift = |i: usize| -> Option<f64> {
            Some(
                f64::from(r.empty_after.as_ref()?.mean[i])
                    - f64::from(r.empty_before.as_ref()?.mean[i]),
            )
        };
        let cells = (0..CHANNELS)
            .map(|i| {
                let drift_counts = drift(i).unwrap_or(f64::NAN);
                view! {
                    <tr>
                        <td>{format!("Cell {i}")}</td>
                        <td class="num">{format!("{:.4e}", fit.scales[i])}</td>
                        <td class="num">{format!("{:.4}", fit.scales[i] / K_NOMINAL_N_PER_COUNT)}</td>
                        <td class="num">{format!("{:.1}", fit.rms_residual_n[i] * 1e3)}</td>
                        <td class="num">
                            {format!("{drift_counts:.0} ({:.1} mN)", drift_counts * fit.scales[i] * 1e3)}
                        </td>
                    </tr>
                }
            })
            .collect_view();

        // Each case measured with the new calibration: mass and position.
        let cases = r.load_cases().unwrap_or_default();
        let case_rows = r
            .cases
            .iter()
            .zip(&cases)
            .map(|(mounts, case)| {
                let measured: [f32; 6] = std::array::from_fn(|row| {
                    (0..CHANNELS)
                        .map(|i| f64::from(fit.matrix[row][i]) * case.zeroed_counts[i])
                        .sum::<f64>() as f32
                });
                let expected = r.expected(mounts).map(|v| v as f32);
                let (m, e) = (
                    center_of_mass(&measured, 0.1),
                    center_of_mass(&expected, 0.1),
                );
                let text = match (m, e) {
                    (Some(m), Some(e)) => (
                        format!("{:.1}", m.mass_kg * 1000.0),
                        format!("{:+.1}", (m.mass_kg - e.mass_kg) * 1000.0),
                        format!(
                            "{:.1}",
                            ((m.position_mm[0] - e.position_mm[0]).powi(2)
                                + (m.position_mm[1] - e.position_mm[1]).powi(2))
                            .sqrt()
                        ),
                    ),
                    _ => ("-".into(), "-".into(), "-".into()),
                };
                view! {
                    <tr>
                        <td>{mount_list(mounts)}</td>
                        <td class="num">{text.0}</td>
                        <td class="num">{text.1}</td>
                        <td class="num">{text.2}</td>
                    </tr>
                }
            })
            .collect_view();

        view! {
            <p class="hint">{format!(
                "Fitted from {} cases with {:.1} g per weight (g = {STANDARD_GRAVITY} m/s\u{b2}).",
                r.cases.len(),
                r.mass_kg * 1000.0
            )}</p>
            <div class="scroll-x">
            <table class="data">
                <tr>
                    <th></th>
                    <th>"scale (N/count)"</th>
                    <th>"vs. nominal"</th>
                    <th>"RMS error (mN)"</th>
                    <th>"zero drift (counts)"</th>
                </tr>
                {cells}
            </table>
            </div>
            <p class="hint">"Each case measured with the new calibration:"</p>
            <div class="scroll-x">
            <table class="data">
                <tr>
                    <th>"weights on"</th>
                    <th>"mass (g)"</th>
                    <th>"error (g)"</th>
                    <th>"position error (mm)"</th>
                </tr>
                {case_rows}
            </table>
            </div>
        }
        .into_any()
    }
}
