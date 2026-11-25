//! `cargo xtask [--dev]`: build the web app into one self-contained HTML file.
//!
//! Steps:
//! 1. `cargo build -p six-dof-platform-web --target wasm32-unknown-unknown [--release]`.
//! 2. Run wasm-bindgen (as a library, so no separately installed CLI is needed) to
//!    generate the JS glue that loads the wasm and connects it to browser APIs.
//! 3. Convert the CAD models (`assets/platform.gltf`, `assets/calibration_weight.gltf`)
//!    into the compact format the 3D view reads (`model.rs`).
//! 4. Inline the JS, the base64-encoded wasm and the model into a single HTML page:
//!    `dist/six-dof.html`. It can be opened straight from disk (`file://`), which
//!    Chromium treats as a secure context, so Web Serial is available.
//!
//! The usual xtask pattern: build logic written as a normal Rust program in the
//! workspace, run through a cargo alias, instead of a shell script or extra tools.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use base64::Engine;
use wasm_bindgen_cli_support::Bindgen;

mod model;

const APP_PACKAGE: &str = "six-dof-platform-web";
/// The UI's stylesheet (relative to the web workspace).
const STYLESHEET: &str = "app/style.css";
/// CAD exports for the 3D view (relative to the web workspace), and the global
/// JavaScript variable each is embedded as.
const MODELS: [(&str, &str); 2] = [
    ("assets/platform.gltf", "SIX_DOF_MODEL"),
    ("assets/calibration_weight.gltf", "SIX_DOF_WEIGHT_MODEL"),
];
const WASM_TARGET: &str = "wasm32-unknown-unknown";
const OUTPUT: &str = "dist/six-dof.html";

fn main() -> Result<(), Box<dyn Error>> {
    let mut release = true;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--dev" => release = false,
            "-h" | "--help" => {
                println!("usage: cargo xtask [--dev]\n\nBuilds {OUTPUT} (release unless --dev).");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {other} (try --help)").into()),
        }
    }

    // The web workspace root (this crate lives in web/xtask/).
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("no workspace dir")?;

    // 1. Build the app. Run from the workspace dir so web/.cargo/config.toml applies
    //    (it sets `--cfg=web_sys_unstable_apis` for the wasm target).
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .current_dir(workspace)
        .args(["build", "--package", APP_PACKAGE, "--target", WASM_TARGET]);
    if release {
        build.arg("--release");
    }
    let status = build.status()?;
    if !status.success() {
        return Err(format!("cargo build failed ({status})").into());
    }

    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace.join("target"));
    let wasm_path = target_dir
        .join(WASM_TARGET)
        .join(if release { "release" } else { "debug" })
        // Binary targets keep the package's hyphens (library targets would use `_`).
        .join(format!("{APP_PACKAGE}.wasm"));

    // 2. wasm-bindgen. `web` = output an ES module whose init function can be
    //    given the wasm bytes directly instead of fetching a separate file.
    let mut output = Bindgen::new()
        .input_path(&wasm_path)
        .web(true)?
        .typescript(false)
        .debug(!release)
        .generate_output()?;
    let js = output.js().to_owned();
    let wasm = output.wasm_mut().emit_wasm();

    // 3. The 3D models. Embedded as a script that runs before the app, as
    //    `window.SIX_DOF_MODEL` etc. (bytes); the app reads them from there, so the
    //    wasm doesn't have to contain them and the app also builds without them.
    // The stylesheet, inlined into <head>.
    let css = fs::read_to_string(workspace.join(STYLESHEET))?;
    if css.contains("</style") {
        return Err("style.css contains `</style`, which would break inlining".into());
    }

    let mut models_js = String::new();
    let mut models_size = 0;
    for (path, variable) in MODELS {
        let model = model::convert(&workspace.join(path))?;
        models_size += model.len();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&model);
        models_js.push_str(&format!(
            "window.{variable} = Uint8Array.from(atob(\"{b64}\"), (c) => c.charCodeAt(0));\n"
        ));
    }

    // 4. Inline everything. The JS glue declares `__wbg_init` (its default export);
    //    since it's inlined into the same module script, we can call it by name.
    if !js.contains("async function __wbg_init") {
        return Err("wasm-bindgen output has no `__wbg_init`; did its JS format change?".into());
    }
    if js.contains("</script") {
        return Err("wasm-bindgen output contains `</script`, which would break inlining".into());
    }
    let wasm_b64 = base64::engine::general_purpose::STANDARD.encode(&wasm);
    let html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Six-DOF test stand</title>
<style>
{css}</style>
</head>
<body>
<script>
{models_js}</script>
<script type="module">
{js}
const wasmBase64 = "{wasm_b64}";
const wasmBytes = Uint8Array.from(atob(wasmBase64), (c) => c.charCodeAt(0));
await __wbg_init({{ module_or_path: wasmBytes }});
</script>
</body>
</html>
"#
    );

    let out_path = workspace.join(OUTPUT);
    fs::create_dir_all(out_path.parent().ok_or("bad output path")?)?;
    fs::write(&out_path, &html)?;
    println!(
        "wrote {} ({} KiB; wasm {} KiB, models {} KiB)",
        out_path.display(),
        html.len() / 1024,
        wasm.len() / 1024,
        models_size / 1024
    );
    Ok(())
}
