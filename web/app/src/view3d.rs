//! 3D view: the stand model with the measured wrench as arrows and the load
//! cells colored by their link force.
//!
//! - Model: `window.SIX_DOF_MODEL`, embedded by `xtask` (`model_format.rs`).
//!   Coordinates in m, stand frame (origin = center of the top triangle, z up).
//! - Rendering: three-d's renderer on a WebGL2 context created from our own
//!   `<canvas>`, redrawn every animation frame (`requestAnimationFrame`). The
//!   wrench is read from the non-reactive accumulator each frame, so 250 Hz data
//!   never goes through Leptos signals.
//! - Arrows: the force ending at the reference point, and the torque about it
//!   (starting there) as a double-headed arrow (right-hand rule: thumb along the arrow, fingers curl in
//!   the direction of rotation).
//! - Load cells: link force f = A⁻¹ · w (`geometry::A`, wrench about the origin);
//!   blue = compression, red = tension.
//! - Calibration weights (`window.SIX_DOF_WEIGHT_MODEL`): shown on the mounts
//!   the wizard asks for. When the requested mounts change, the shown weights
//!   fade out, then the new ones fade in while dropping into their mounts.
//! - Center-of-mass mode: the arrows are drawn at the computed center of mass
//!   (`geometry::center_of_mass`) instead of the reference point.

use std::{cell::RefCell, rc::Rc, sync::Arc};

use leptos::{html, prelude::*};
use six_dof_platform_common::geometry::{A, MOUNTS, center_of_mass, solve, wrench_about};
use three_d::{
    AmbientLight, Camera, ClearState, Context, CpuMaterial, CpuMesh, DirectionalLight, Gm, Indices,
    InnerSpace, Mat4, Mesh, PhysicalMaterial, Positions, Quat, RenderTarget, Srgba, Vec3, Viewport,
    degrees, vec3,
};
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, WebGl2RenderingContext};

use crate::model_format::{self, Part};

/// Force arrow length per newton (m/N): 1 kg (9.81 N) → 98 mm.
const FORCE_SCALE_M_PER_N: f32 = 0.01;
/// Torque arrow length per newton-metre (m/(N·m)): 1 kg at 50 mm → 98 mm.
const TORQUE_SCALE_M_PER_NM: f32 = 0.2;
/// Link force (N) at which the load cell color is fully blue or red.
const LINK_FORCE_COLOR_RANGE_N: f64 = 10.0;
/// Arrows shorter than this aren't drawn (m); below the noise of single readings.
const MIN_ARROW_M: f32 = 0.002;

const FORCE_COLOR: Srgba = Srgba::new_opaque(230, 126, 34);
const TORQUE_COLOR: Srgba = Srgba::new_opaque(142, 68, 173);
const TENSION_COLOR: Srgba = Srgba::new_opaque(220, 40, 40);
const COMPRESSION_COLOR: Srgba = Srgba::new_opaque(40, 90, 220);

/// The weight drops this far into its mount while fading in (m).
const WEIGHT_DROP_M: f32 = 0.03;
const WEIGHT_FADE_IN_MS: f64 = 700.0;
const WEIGHT_FADE_OUT_MS: f64 = 400.0;
/// Below this weight (N) there is no center of mass to show (noise: ~0.04 N).
pub const MIN_COM_WEIGHT_N: f64 = 0.2;

/// Point the camera looks at and orbits around (m): the origin (center of the
/// top triangle, top surface of the platform; David, 2026-11-24).
const CAMERA_TARGET: [f32; 3] = [0.0, 0.0, 0.0];

/// Texts shown below the view.
pub const LEGEND: &str = "Orange arrow: force on the platform, pointing at the reference \
     point (10 mm per N). Purple double arrow: torque about the reference point (200 mm per N·m, \
     right-hand rule). Load cells: blue = compression, red = tension (full color at 10 N). \
     Arrows are drawn in front of the model. Right-drag to rotate, scroll to zoom.";

/// What the frame loop reads each frame (signals are read untracked).
#[derive(Clone, Copy)]
pub struct ViewInputs {
    /// Reference point for the arrows (mm).
    pub reference_point: RwSignal<[f64; 3]>,
    /// Mounts (`geometry::MOUNTS` indices) to show calibration weights on; at
    /// most two.
    pub weights: RwSignal<Vec<usize>>,
    /// Draw the arrows at the center of mass instead of the reference point.
    pub center_of_mass_mode: RwSignal<bool>,
}

/// The 3D view. `wrench` returns the wrench about the origin to show (`None`
/// when not connected).
#[component]
pub fn View3d(
    wrench: impl Fn() -> Option<[f32; 6]> + 'static,
    inputs: ViewInputs,
) -> impl IntoView {
    let canvas_ref = NodeRef::<html::Canvas>::new();
    let error = RwSignal::new(None::<String>);
    let scene: Rc<RefCell<Option<Scene>>> = Rc::new(RefCell::new(None));
    let wrench: Rc<dyn Fn() -> Option<[f32; 6]>> = Rc::new(wrench);

    // Once the canvas exists: set up WebGL and start the frame loop.
    {
        let scene = scene.clone();
        Effect::new(move || {
            let Some(canvas) = canvas_ref.get() else {
                return;
            };
            if scene.borrow().is_some() {
                return;
            }
            match Scene::new(&canvas) {
                Ok(s) => {
                    *scene.borrow_mut() = Some(s);
                    frame(scene.clone(), wrench.clone(), inputs);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    }

    let on_pointer_move = {
        let scene = scene.clone();
        move |ev: leptos::ev::PointerEvent| {
            // Right button held (`buttons` bit 1): orbit (David).
            if ev.buttons() & 2 != 0
                && let Some(s) = scene.borrow_mut().as_mut()
            {
                s.orbit(ev.movement_x() as f32, ev.movement_y() as f32);
            }
        }
    };
    let on_wheel = {
        let scene = scene.clone();
        move |ev: leptos::ev::WheelEvent| {
            ev.prevent_default(); // don't scroll the page
            if let Some(s) = scene.borrow_mut().as_mut() {
                s.zoom(ev.delta_y() as f32);
            }
        }
    };

    view! {
        <canvas
            node_ref=canvas_ref
            style="width: 100%; max-width: 900px; height: 500px; display: block; \
                   box-sizing: border-box; border: 1px solid #ccc; touch-action: none"
            on:pointermove=on_pointer_move
            on:wheel=on_wheel
            // No browser context menu on right click (right-drag orbits).
            on:contextmenu=|ev| ev.prevent_default()
        ></canvas>
        <p>
            {move || error.get().map(|e| format!("3D view unavailable: {e}")).unwrap_or_default()}
        </p>
        <p style="max-width: 900px">{LEGEND}</p>
    }
}

/// Draws one frame and schedules the next.
fn frame(
    scene: Rc<RefCell<Option<Scene>>>,
    wrench: Rc<dyn Fn() -> Option<[f32; 6]>>,
    inputs: ViewInputs,
) {
    if let Some(s) = scene.borrow_mut().as_mut() {
        let now_ms = js_sys::Date::now();
        inputs
            .weights
            .with_untracked(|w| s.update_weights(w, now_ms));
        s.render(
            wrench(),
            inputs.reference_point.get_untracked(),
            inputs.center_of_mass_mode.get_untracked(),
        );
    }
    request_animation_frame(move || frame(scene, wrench, inputs));
}

/// Most the camera can look up or down (degrees from horizontal). At ±90° the
/// view direction would be parallel to "up" (+z), which leaves the camera's
/// sideways direction undefined.
const MAX_ELEVATION_DEG: f32 = 89.0;

/// Camera position around `CAMERA_TARGET` in spherical coordinates.
///
/// Why not three-d's `rotate_around_with_fixed_up`: it tilts about a
/// horizontal axis it recomputes from (view direction × up) on every move.
/// Near straight up/down that cross product is tiny and its direction swings
/// with small changes, and it's taken before the move's sideways part is
/// applied, so looking steeply up or down made the sideways angle unstable.
/// Keeping the two angles directly avoids both.
#[derive(Clone, Copy, Debug)]
struct Orbit {
    /// Angle around the z axis, from +x, counter-clockwise seen from above (rad).
    azimuth: f32,
    /// Angle above the horizontal (rad), within ±`MAX_ELEVATION_DEG`.
    elevation: f32,
    distance_m: f32,
}

impl Orbit {
    fn position(&self) -> Vec3 {
        Vec3::from(CAMERA_TARGET)
            + self.distance_m
                * vec3(
                    self.elevation.cos() * self.azimuth.cos(),
                    self.elevation.cos() * self.azimuth.sin(),
                    self.elevation.sin(),
                )
    }
}

/// Animation state of the calibration weights.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WeightPhase {
    Hidden,
    /// Fading in and dropping, since the given time (ms).
    In(f64),
    Shown,
    /// Fading out, since the given time (ms).
    Out(f64),
}

/// An arrow made of a cylinder (tail, stretched to the length) and one or two
/// cones (heads, fixed size), so long arrows don't get long heads.
struct Arrow {
    tail: Gm<Mesh, PhysicalMaterial>,
    heads: Vec<Gm<Mesh, PhysicalMaterial>>,
    visible: bool,
}

impl Arrow {
    const HEAD_LENGTH: f32 = 0.012;
    const HEAD_RADIUS: f32 = 0.006;
    const TAIL_RADIUS: f32 = 0.0025;

    fn new(context: &Context, color: Srgba, heads: usize) -> Self {
        let material = || {
            PhysicalMaterial::new_opaque(
                context,
                &CpuMaterial {
                    albedo: color,
                    roughness: 0.5,
                    ..Default::default()
                },
            )
        };
        Self {
            tail: Gm::new(Mesh::new(context, &CpuMesh::cylinder(16)), material()),
            heads: (0..heads)
                .map(|_| Gm::new(Mesh::new(context, &CpuMesh::cone(16)), material()))
                .collect(),
            visible: false,
        }
    }

    /// Places the arrow from `origin` along `vector` (both in m).
    fn set(&mut self, origin: Vec3, vector: Vec3) {
        let length = vector.magnitude();
        self.visible = length >= MIN_ARROW_M;
        if !self.visible {
            return;
        }
        // Rotation taking the meshes' axis (+x) to the arrow's direction.
        let rotation = Mat4::from(Quat::from_arc(
            vec3(1.0, 0.0, 0.0),
            vector / length,
            Some(vec3(0.0, 0.0, 1.0)),
        ));
        let place = Mat4::from_translation(origin) * rotation;
        // All heads must fit: shrink them for short arrows.
        let head_total = Self::HEAD_LENGTH * self.heads.len() as f32;
        let shrink = (length / head_total).min(1.0);
        let head_length = Self::HEAD_LENGTH * shrink;
        let tail_length = (length - head_length * self.heads.len() as f32).max(0.0);
        self.tail.set_transformation(
            place
                * Mat4::from_nonuniform_scale(
                    tail_length.max(1e-6),
                    Self::TAIL_RADIUS,
                    Self::TAIL_RADIUS,
                ),
        );
        for (i, head) in self.heads.iter_mut().enumerate() {
            let start = tail_length + head_length * i as f32;
            head.set_transformation(
                place
                    * Mat4::from_translation(vec3(start, 0.0, 0.0))
                    * Mat4::from_nonuniform_scale(
                        head_length,
                        Self::HEAD_RADIUS * shrink,
                        Self::HEAD_RADIUS * shrink,
                    ),
            );
        }
    }

    fn objects(&self) -> impl Iterator<Item = &Gm<Mesh, PhysicalMaterial>> {
        std::iter::once(&self.tail)
            .chain(&self.heads)
            .filter(|_| self.visible)
    }
}

struct Scene {
    canvas: HtmlCanvasElement,
    context: Context,
    camera: Camera,
    orbit: Orbit,
    parts: Vec<Gm<Mesh, PhysicalMaterial>>,
    /// Index into `parts` and original color of the load cell of each link.
    cells: [Option<(usize, Srgba)>; 6],
    force: Arrow,
    torque: Arrow,
    /// Two instances of the weight model (each a list of parts); empty if the
    /// model isn't embedded.
    weights: [Vec<Gm<Mesh, PhysicalMaterial>>; 2],
    /// Mounts the weights are currently shown on, and their animation.
    weights_shown: Vec<usize>,
    weight_phase: WeightPhase,
    ambient: AmbientLight,
    key_light: DirectionalLight,
    fill_light: DirectionalLight,
}

impl Scene {
    fn new(canvas: &HtmlCanvasElement) -> Result<Self, String> {
        let parts = load_model("SIX_DOF_MODEL")?;
        // Optional: without it, the wizard just doesn't show weights.
        let weight_parts = load_model("SIX_DOF_WEIGHT_MODEL").unwrap_or_default();

        // WebGL2 context with antialiasing, wrapped for three-d.
        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&options, &"antialias".into(), &true.into());
        let gl: WebGl2RenderingContext = canvas
            .get_context_with_context_options("webgl2", &options)
            .ok()
            .flatten()
            .ok_or("this browser doesn't provide WebGL2")?
            .unchecked_into();
        #[allow(
            clippy::arc_with_non_send_sync,
            reason = "three-d's API takes an Arc; the browser runs everything on one thread"
        )]
        let context = Context::from_gl_context(Arc::new(glow::Context::from_webgl2_context(gl)))
            .map_err(|e| format!("WebGL setup failed: {e}"))?;

        let mut cells = [None; 6];
        let mut gms = Vec::with_capacity(parts.len());
        for part in &parts {
            let color = Srgba::from(part.color);
            if let Some(link) = part
                .name
                .strip_prefix("LC")
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|&n| n < 6)
            {
                cells[link] = Some((gms.len(), color));
            }
            let mesh = cpu_mesh(part);
            let material = CpuMaterial {
                albedo: color,
                roughness: 0.8,
                metallic: 0.0,
                ..Default::default()
            };
            gms.push(Gm::new(
                Mesh::new(&context, &mesh),
                PhysicalMaterial::new_opaque(&context, &material),
            ));
        }

        // Start looking from the front-right, slightly from above, far enough
        // that the base (down to z = −0.174 m) is in view.
        let orbit = Orbit {
            azimuth: -60.0_f32.to_radians(),
            elevation: 25.0_f32.to_radians(),
            distance_m: 0.8,
        };
        let camera = Camera::new_perspective(
            Viewport::new_at_origo(1, 1),
            orbit.position(),
            Vec3::from(CAMERA_TARGET),
            vec3(0.0, 0.0, 1.0),
            degrees(35.0),
            0.01,
            10.0,
        );

        let weight_instance = || {
            weight_parts
                .iter()
                .map(|part| {
                    Gm::new(
                        Mesh::new(&context, &cpu_mesh(part)),
                        PhysicalMaterial::new_transparent(
                            &context,
                            &CpuMaterial {
                                albedo: Srgba::from(part.color),
                                roughness: 0.4,
                                metallic: 0.0,
                                ..Default::default()
                            },
                        ),
                    )
                })
                .collect::<Vec<_>>()
        };

        Ok(Self {
            canvas: canvas.clone(),
            weights: [weight_instance(), weight_instance()],
            weights_shown: Vec::new(),
            weight_phase: WeightPhase::Hidden,
            force: Arrow::new(&context, FORCE_COLOR, 1),
            torque: Arrow::new(&context, TORQUE_COLOR, 2),
            ambient: AmbientLight::new(&context, 0.35, Srgba::WHITE),
            key_light: DirectionalLight::new(&context, 2.0, Srgba::WHITE, vec3(-0.4, 0.6, -1.0)),
            fill_light: DirectionalLight::new(&context, 0.6, Srgba::WHITE, vec3(0.6, -0.3, 0.4)),
            context,
            camera,
            orbit,
            parts: gms,
            cells,
        })
    }

    /// Drag by (dx, dy) pixels. Dragging right turns the stand to the right,
    /// dragging down looks from higher up (David's choice of directions).
    fn orbit(&mut self, dx: f32, dy: f32) {
        self.orbit.azimuth -= 0.01 * dx;
        self.orbit.elevation = (self.orbit.elevation + 0.01 * dy).clamp(
            -MAX_ELEVATION_DEG.to_radians(),
            MAX_ELEVATION_DEG.to_radians(),
        );
        self.apply_orbit();
    }

    /// Wheel by `delta`; scrolling up (negative delta) zooms in (David).
    fn zoom(&mut self, delta: f32) {
        self.orbit.distance_m = (self.orbit.distance_m * (0.001 * delta).exp()).clamp(0.1, 3.0);
        self.apply_orbit();
    }

    fn apply_orbit(&mut self) {
        self.camera.set_view(
            self.orbit.position(),
            Vec3::from(CAMERA_TARGET),
            vec3(0.0, 0.0, 1.0),
        );
    }

    /// Advances the weights' animation towards showing them on `target` mounts.
    fn update_weights(&mut self, target: &[usize], now_ms: f64) {
        if let WeightPhase::Out(since) = self.weight_phase
            && now_ms - since >= WEIGHT_FADE_OUT_MS
        {
            self.weights_shown.clear();
            self.weight_phase = WeightPhase::Hidden;
        }
        if self.weights_shown != target {
            match self.weight_phase {
                // Nothing shown: bring in the new ones.
                WeightPhase::Hidden if !target.is_empty() => {
                    self.weights_shown = target.iter().copied().take(2).collect();
                    self.weight_phase = WeightPhase::In(now_ms);
                }
                // Something else shown: take it away first.
                WeightPhase::In(_) | WeightPhase::Shown => {
                    self.weight_phase = WeightPhase::Out(now_ms);
                }
                WeightPhase::Hidden | WeightPhase::Out(_) => {}
            }
        }
        if let WeightPhase::In(since) = self.weight_phase
            && now_ms - since >= WEIGHT_FADE_IN_MS
        {
            self.weight_phase = WeightPhase::Shown;
        }

        // Opacity and height above the final position.
        let (alpha, lift) = match self.weight_phase {
            WeightPhase::Hidden => (0.0, 0.0),
            WeightPhase::In(since) => {
                let t = ((now_ms - since) / WEIGHT_FADE_IN_MS).clamp(0.0, 1.0) as f32;
                // Ease out: fast at first, settling gently into the mount.
                let eased = 1.0 - (1.0 - t).powi(3);
                (t, WEIGHT_DROP_M * (1.0 - eased))
            }
            WeightPhase::Shown => (1.0, 0.0),
            WeightPhase::Out(since) => {
                let t = ((now_ms - since) / WEIGHT_FADE_OUT_MS).clamp(0.0, 1.0) as f32;
                (1.0 - t, 0.0)
            }
        };
        for (instance, &mount) in self.weights.iter_mut().zip(&self.weights_shown) {
            let [x, y] = MOUNTS[mount].map(|v| (v * 1e-3) as f32);
            let place = Mat4::from_translation(vec3(x, y, lift));
            for part in instance {
                part.set_transformation(place);
                part.material.albedo.a = (alpha * 255.0).round() as u8;
            }
        }
    }

    fn weight_objects(&self) -> impl Iterator<Item = &Gm<Mesh, PhysicalMaterial>> {
        let visible = if self.weight_phase == WeightPhase::Hidden {
            0
        } else {
            self.weights_shown.len()
        };
        self.weights[..visible].iter().flatten()
    }

    fn render(&mut self, wrench: Option<[f32; 6]>, reference_mm: [f64; 3], com_mode: bool) {
        // Match the drawing buffer to the displayed size (in device pixels).
        let ratio = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
        let width = (f64::from(self.canvas.client_width()) * ratio)
            .round()
            .max(1.0) as u32;
        let height = (f64::from(self.canvas.client_height()) * ratio)
            .round()
            .max(1.0) as u32;
        if self.canvas.width() != width || self.canvas.height() != height {
            self.canvas.set_width(width);
            self.canvas.set_height(height);
        }
        self.camera
            .set_viewport(Viewport::new_at_origo(width, height));

        // Center-of-mass mode: anchor the arrows there (if something rests on
        // the platform).
        let anchor_mm = match wrench.and_then(|w| center_of_mass(&w, MIN_COM_WEIGHT_N)) {
            Some(com) if com_mode => [com.position_mm[0], com.position_mm[1], 0.0],
            _ => reference_mm,
        };
        self.update_load(wrench, anchor_mm);

        // Two passes: the model, then (after clearing only the depth buffer) the
        // arrows, so the model never hides them (David). The arrows still hide
        // each other correctly.
        let lights: [&dyn three_d::Light; 3] = [&self.ambient, &self.key_light, &self.fill_light];
        RenderTarget::screen(&self.context, width, height)
            .clear(ClearState::color_and_depth(1.0, 1.0, 1.0, 1.0, 1.0))
            .render(
                &self.camera,
                self.parts.iter().chain(self.weight_objects()),
                &lights,
            )
            .clear(ClearState::depth(1.0))
            .render(
                &self.camera,
                self.force.objects().chain(self.torque.objects()),
                &lights,
            );
    }

    /// Updates arrows and load cell colors from the wrench about the origin.
    fn update_load(&mut self, wrench: Option<[f32; 6]>, reference_mm: [f64; 3]) {
        let Some(w) = wrench else {
            self.force.visible = false;
            self.torque.visible = false;
            for (index, color) in self.cells.iter().flatten() {
                self.parts[*index].material.albedo = *color;
            }
            return;
        };

        let about = wrench_about(&w, reference_mm);
        let origin = Vec3::from(reference_mm.map(|v| (v * 1e-3) as f32));
        // The force arrow ends at the reference point (pushing on it), so a
        // weight's arrow sits above the platform instead of inside it.
        let force = vec3(about[0], about[1], about[2]) * FORCE_SCALE_M_PER_N;
        self.force.set(origin - force, force);
        self.torque.set(
            origin,
            vec3(about[3], about[4], about[5]) * TORQUE_SCALE_M_PER_NM,
        );

        // Link forces from the wrench about the origin: w = A · f.
        let link_forces = solve(&A, &w.map(f64::from));
        for (link, cell) in self.cells.iter().enumerate() {
            let Some((index, base)) = cell else { continue };
            let color = match link_forces {
                Some(f) => {
                    let t = (f[link] / LINK_FORCE_COLOR_RANGE_N).clamp(-1.0, 1.0);
                    let toward = if t >= 0.0 {
                        TENSION_COLOR
                    } else {
                        COMPRESSION_COLOR
                    };
                    mix(*base, toward, t.abs() as f32)
                }
                None => *base,
            };
            self.parts[*index].material.albedo = color;
        }
    }
}

/// Linear blend of two colors (in sRGB; fine for a display scale).
fn mix(a: Srgba, b: Srgba, t: f32) -> Srgba {
    let blend = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Srgba::new(blend(a.r, b.r), blend(a.g, b.g), blend(a.b, b.b), a.a)
}

fn cpu_mesh(part: &Part) -> CpuMesh {
    CpuMesh {
        positions: Positions::F32(part.positions.iter().map(|&p| p.into()).collect()),
        normals: Some(part.normals.iter().map(|&n| n.into()).collect()),
        indices: Indices::U32(part.indices.clone()),
        ..Default::default()
    }
}

/// Decodes a model embedded by `xtask` (`window.<variable>`).
fn load_model(variable: &str) -> Result<Vec<Part>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let data = js_sys::Reflect::get(&window, &variable.into())
        .ok()
        .filter(|v| !v.is_undefined())
        .ok_or("no model embedded (build with `cargo xtask`)")?;
    let bytes = js_sys::Uint8Array::new(&data).to_vec();
    model_format::decode(&bytes)
}
