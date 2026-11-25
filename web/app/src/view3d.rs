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

use std::{cell::RefCell, rc::Rc, sync::Arc};

use leptos::{html, prelude::*};
use six_dof_platform_common::geometry::{A, solve, wrench_about};
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

/// Point the camera looks at and orbits around (m): the origin (center of the
/// top triangle, top surface of the platform; David, 2026-11-24).
const CAMERA_TARGET: [f32; 3] = [0.0, 0.0, 0.0];

/// Texts shown below the view.
pub const LEGEND: &str = "Orange arrow: force on the platform, pointing at the reference \
     point (10 mm per N). Purple double arrow: torque about the reference point (200 mm per N·m, \
     right-hand rule). Load cells: blue = compression, red = tension (full color at 10 N). \
     Arrows are drawn in front of the model. Right-drag to rotate, scroll to zoom.";

/// The 3D view. `wrench` returns the latest wrench about the origin (`None`
/// when not connected); `reference_point` is in mm.
#[component]
pub fn View3d(
    wrench: impl Fn() -> Option<[f32; 6]> + 'static,
    reference_point: RwSignal<[f64; 3]>,
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
                    frame(scene.clone(), wrench.clone(), reference_point);
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
    reference_point: RwSignal<[f64; 3]>,
) {
    if let Some(s) = scene.borrow_mut().as_mut() {
        s.render(wrench(), reference_point.get_untracked());
    }
    request_animation_frame(move || frame(scene, wrench, reference_point));
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
    parts: Vec<Gm<Mesh, PhysicalMaterial>>,
    /// Index into `parts` and original color of the load cell of each link.
    cells: [Option<(usize, Srgba)>; 6],
    force: Arrow,
    torque: Arrow,
    ambient: AmbientLight,
    key_light: DirectionalLight,
    fill_light: DirectionalLight,
}

impl Scene {
    fn new(canvas: &HtmlCanvasElement) -> Result<Self, String> {
        let parts = load_model()?;

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
            let mesh = CpuMesh {
                positions: Positions::F32(part.positions.iter().map(|&p| p.into()).collect()),
                normals: Some(part.normals.iter().map(|&n| n.into()).collect()),
                indices: Indices::U32(part.indices.clone()),
                ..Default::default()
            };
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

        let target = Vec3::from(CAMERA_TARGET);
        // Start looking from the front-right, slightly from above, far enough
        // that the base (down to z = −0.174 m) is in view.
        let (azimuth, elevation, distance) = (-60.0_f32.to_radians(), 25.0_f32.to_radians(), 0.8);
        let position = target
            + distance
                * vec3(
                    elevation.cos() * azimuth.cos(),
                    elevation.cos() * azimuth.sin(),
                    elevation.sin(),
                );
        let camera = Camera::new_perspective(
            Viewport::new_at_origo(1, 1),
            position,
            target,
            vec3(0.0, 0.0, 1.0),
            degrees(35.0),
            0.01,
            10.0,
        );

        Ok(Self {
            canvas: canvas.clone(),
            force: Arrow::new(&context, FORCE_COLOR, 1),
            torque: Arrow::new(&context, TORQUE_COLOR, 2),
            ambient: AmbientLight::new(&context, 0.35, Srgba::WHITE),
            key_light: DirectionalLight::new(&context, 2.0, Srgba::WHITE, vec3(-0.4, 0.6, -1.0)),
            fill_light: DirectionalLight::new(&context, 0.6, Srgba::WHITE, vec3(0.6, -0.3, 0.4)),
            context,
            camera,
            parts: gms,
            cells,
        })
    }

    fn orbit(&mut self, dx: f32, dy: f32) {
        let target = Vec3::from(CAMERA_TARGET);
        // Dragging right turns the stand to the right (David).
        self.camera
            .rotate_around_with_fixed_up(target, 0.01 * dx, 0.01 * dy);
    }

    fn zoom(&mut self, delta: f32) {
        let target = Vec3::from(CAMERA_TARGET);
        let distance = (self.camera.position() - target).magnitude();
        let amount = distance * (1.0 - (-delta * 0.001).exp());
        self.camera.zoom_towards(target, amount, 0.1, 3.0);
    }

    fn render(&mut self, wrench: Option<[f32; 6]>, reference_mm: [f64; 3]) {
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

        self.update_load(wrench, reference_mm);

        // Two passes: the model, then (after clearing only the depth buffer) the
        // arrows, so the model never hides them (David). The arrows still hide
        // each other correctly.
        let lights: [&dyn three_d::Light; 3] = [&self.ambient, &self.key_light, &self.fill_light];
        RenderTarget::screen(&self.context, width, height)
            .clear(ClearState::color_and_depth(1.0, 1.0, 1.0, 1.0, 1.0))
            .render(&self.camera, &self.parts, &lights)
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

/// Decodes the model embedded by `xtask` (`window.SIX_DOF_MODEL`).
fn load_model() -> Result<Vec<Part>, String> {
    let window = web_sys::window().ok_or("no window")?;
    let data = js_sys::Reflect::get(&window, &"SIX_DOF_MODEL".into())
        .ok()
        .filter(|v| !v.is_undefined())
        .ok_or("no model embedded (build with `cargo xtask`)")?;
    let bytes = js_sys::Uint8Array::new(&data).to_vec();
    model_format::decode(&bytes)
}
