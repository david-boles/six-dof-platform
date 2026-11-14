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
- Nothing yet.

## Standing decisions (don't re-suggest)
- **David handles all git commits and pushes himself.** The agent may edit files and use read-only git commands (`status`, `diff`, `log`), but must not run `git add`/`commit`/`push` or other commands that change history. When a logical chunk of work is done, say so briefly. Don't suggest commit messages.
- **David runs long-running and interactive jobs himself** (e.g. flashing and running on hardware, long captures). Give him a plain foreground command to run instead of starting background jobs.
- Read-only exploration is fine. Ask before making changes outside the repo. Don't edit application or user settings; tell David what to change instead.

## Project
- ESP32-C6 firmware in Rust (`esp-hal` 1.2, `esp-rtos` + Embassy, defmt over RTT, probe-rs). Target `riscv32imac-unknown-none-elf`, stable toolchain.
- 3× Adafruit ADS122C04 (24-bit ADC) boards on I2C0 (SDA GPIO22, SCL GPIO23, 1 MHz) at `0x40`, `0x41`, `0x44`. Each board reads two load cells (AIN0–AIN1 and AIN2–AIN3, gain 128, external reference).
- `src/ads122c04.rs`: register/command driver. `src/input_adcs.rs`: timer ISR sampling (2 ms per channel → 6 readings at 250 Hz). `src/bin/main.rs`: setup and logging.
- References in `reference/`: ADS122C04 datasheet, Adafruit board schematic, link to the Adafruit guide (`Adafruit.md`).
- Build check (agent): `cargo build`. Run on hardware (David): `cargo run` (probe-rs runner in `.cargo/config.toml`).
- After editing Rust code, run `cargo fmt` and `cargo clippy`.
