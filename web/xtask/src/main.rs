//! `cargo xtask [--dev]`: build the web app into one self-contained HTML file.
//!
//! Steps:
//! 1. `cargo build -p six-dof-platform-web --target wasm32-unknown-unknown [--release]`.
//! 2. Run wasm-bindgen (as a library, so no separately installed CLI is needed) to
//!    generate the JS glue that loads the wasm and connects it to browser APIs.
//! 3. Inline the JS and the base64-encoded wasm into a single HTML page:
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

const APP_PACKAGE: &str = "six-dof-platform-web";
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

    // 3. Inline everything. The JS glue declares `__wbg_init` (its default export);
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
</head>
<body>
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
        "wrote {} ({} KiB; wasm {} KiB)",
        out_path.display(),
        html.len() / 1024,
        wasm.len() / 1024
    );
    Ok(())
}
