# Six-DOF force/torque test stand — agent context

## About the user
- David (`deb`). Reasonably familiar with embedded Rust and this hardware, but **explain anything novel to him in detail** (teaching is a goal, not just getting things done). When unsure whether something is new to him, err on the side of too verbose.
- Tone: plain and factual. No emotional language, praise, sign-offs or emoji.
- Before quoting docs, links, datasheet values or other facts, verify them (fetch the page, read the source or datasheet, measure on the hardware). Say when something is unverified, and correct earlier mistakes explicitly.
- Work plan (once it exists) lives in `TODO.md`:
  - Sections are ordered by priority. `[?]` means the item needs a decision first.
  - Items have IDs (`section.item.sub`, e.g. `1.1.4`). Items may be renumbered when inserting one higher in the list. When renumbering, update cross-references in `TODO.md`/`AGENTS.md` and mention the renumbering to David.
  - Keep it updated as items get done or decided, and record measurements and decisions in the items themselves, e.g. `- [x] **1.2** (done: …) description`.

## Where we left off (update at the end of each session)
- **2026-11-24:** End-to-end path works on hardware (TODO 2.0): firmware (postcard-rpc over USB-Serial-JTAG) → browser UI (`web/dist/six-dof.html`, Web Serial) showing raw counts, ADC status and device log. Repo restructured into `firmware/`, `protocol/`, `web/`.
- Known issue: `missed` jumps after connecting (stale queued readings), TODO 2.1.3.
- Open from the code review / setup: `.vscode` paths for the new layout (David), rust-analyzer checks `web/` for the host target (Web Serial code shows false errors), whether to commit `protocol/Cargo.lock`, stale root `target/`. Fake-device test scaffolding lives only in the VM's `/tmp` (lost on VM reset); David hasn't decided whether to move it into the repo.
- Next candidates: 2.1.3, unexercised hardware cases in 2.0 (errors, unplug, page closed, probe-rs + serial at once), zeroing (3.1), 3D view (4.1/4.2); graphs on hold (4.3).

## Standing decisions (don't re-suggest)
- **David handles all git commits and pushes himself.** The agent may edit files and use read-only git commands (`status`, `diff`, `log`), but must not run `git add`/`commit`/`push` or other commands that change history. When a logical chunk of work is done, say so briefly. Don't suggest commit messages.
- **David runs long-running and interactive jobs himself** (e.g. flashing and running on hardware, long captures). Give him a plain foreground command to run instead of starting background jobs.
- Change files with the edit/write tools, not via scripts in bash (e.g. Python or `sed -i`).
- Read-only exploration is fine. Ask before making changes outside the repo. Don't edit application or user settings; tell David what to change instead.

## Project
- Purpose: characterize a future monocopter (likely ESP32-C6 flight controller, wireless telemetry/OTA), possibly with closed-loop HIL simulations on the stand. Prefer designs (especially comms) that can be reused on the monocopter.
- ESP32-C6 firmware in Rust (`esp-hal` 1.2, `esp-rtos` + Embassy, defmt over RTT, probe-rs). Target `riscv32imac-unknown-none-elf`, stable toolchain.
- 3× Adafruit ADS122C04 (24-bit ADC) boards on I2C0 (SDA GPIO22, SCL GPIO23, 1 MHz) at `0x40`, `0x41`, `0x44`. Each board reads two load cells (AIN0–AIN1 and AIN2–AIN3, gain 128, external reference).
- Repo layout: three separate Cargo projects (not one workspace, because firmware and web need different targets), each with its own `.cargo/config.toml` / `rust-toolchain.toml`:
  - `firmware/`: `src/ads122c04.rs` register/command driver; `src/input_adcs.rs` timer ISR sampling (2 ms per channel → 6 readings at 250 Hz); `src/comms.rs` postcard-rpc server + pump/publisher tasks; `src/bin/main.rs` setup.
  - `protocol/` (`six-dof-protocol`): `no_std` ICD (endpoints, topics, message types) shared by firmware and web. Features `defmt`, `use-std`.
  - `web/`: workspace with `app` (`six-dof-web`, Leptos UI, wasm) and `xtask` (bundler).
- postcard-rpc comes from David's fork (`david-boles/postcard-rpc`, rev `301d138…`); keep the rev identical in all three `Cargo.toml`s.
- References in `reference/`: ADS122C04 datasheet, Adafruit board schematic, link to the Adafruit guide (`Adafruit.md`).
- Architecture (TODO 1.x): `postcard-rpc` over the USB-Serial-JTAG serial port (transport-agnostic; Wi-Fi later for the monocopter); browser-only Rust/WASM UI over Web Serial (Leptos + `three-d` + canvas plots), no host program.
- Build checks (agent): `cargo build` in `firmware/` and `protocol/`; in `web/`: `cargo clippy -p six-dof-web --target wasm32-unknown-unknown` and `cargo xtask` (writes `web/dist/six-dof.html`). Run on hardware (David): `cargo run` in `firmware/` (probe-rs runner in `firmware/.cargo/config.toml`), then open `web/dist/six-dof.html` in Chromium.
- UI testing without hardware: headless Chromium (browser tool) against a fake `navigator.serial` fed with frames generated from the real protocol crate. Never click Connect without the fake installed (the real port chooser can't be answered headless).
- After editing Rust code, run `cargo fmt` and `cargo clippy`.
