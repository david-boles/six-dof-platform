# Testing the UI without hardware

A fake serial port replaces `navigator.serial` in the browser, so the real UI
(`dist/six-dof.html`) can be tested in Chromium, including headless, without a device.

- `../mockgen/` (`six-dof-platform-mockgen`) generates `mock-data.js` (gitignored):
  device messages built with the real interface types and keys from
  `six-dof-platform-common`, so it can't drift from the firmware's format.
- `fake-serial.js` is the fake port. It answers every request by its key, with the
  request's sequence number (so the order of requests doesn't matter), and streams
  the generated messages. Details in the file.

## Running

In `web/`:

```sh
cargo run -p six-dof-platform-mockgen   # writes test/mock-data.js
cargo xtask                             # writes dist/six-dof.html
```

Open `dist/six-dof.html`, then load both scripts **before** clicking Connect
(otherwise the browser's real port chooser opens), e.g. from the developer
console:

```js
for (const f of ["mock-data.js", "fake-serial.js"]) {
  const s = document.createElement("script");
  s.src = new URL("../test/" + f, location.href);
  s.async = false; // keep the order: fake-serial.js needs MOCK
  document.head.appendChild(s);
}
```

Then click Connect. `window.FAKE_SERIAL` shows the requests received, open/close
counts and how many live frames were sent.

## Scenario and expected results

Defined at the top of `../mockgen/src/main.rs`. After the stream ends (about 4 s):

- Status line: `Connected: device 0000123456789abc, sample period 4000 µs`.
- Statistics: received 997, missed 3, skipped 0 queued from before connecting;
  rate about 250 Hz while streaming. (The 16 backlog readings arrive before the UI
  subscribes, because it checks the interface hash first; the postcard-rpc client
  discards topic messages nobody is subscribed to. Stale readings arriving after
  subscribing would be counted as skipped.)
- Raw counts about −21,918 on every cell (1 kg at the center, nominal scale, ±3
  counts noise).
- Wrench: Fz = −9.807 N, everything else about 0. Calibration "nominal", zero "not
  zeroed".
- ADC status: terminated; device log: one `ADC error: I2c(Timeout) (fake)`.
- Requests (`FAKE_SERIAL.requests`): `interface/hash/get`, `info/get`,
  `adc/status/get`, `time/get`, `calibration/get`, `zero/get` (then `average` per
  click).
- "Average 1 s": "averaging..." with the button disabled for 1 s, then "250
  readings" and a table with mean about −21,918 and standard deviation about 2.0
  per cell (the noise is uniform over −3..3 counts: √((7² − 1)/12) = 2). Body
  `0100` (= `Err(AdcError)`) or `0101` (= `Err(Timeout)`) for the `average` entry →
  the matching error message.
- Disconnect and Connect again: the same results again.
- Zero and calibration (2.5; the fake device answers `*/set` with `Ok` but always
  reports the nominal C and zero offsets 0 on connect, and its wrench stream is
  fixed, so only the UI's labels and requests change):
  - "Set the average as zero" / "Clear zero": one `zero/set` each, "Zero:
    zeroed" / "not zeroed".
  - Reference point x = 50 mm: My = −0.4903 N·m (1 kg at the center seen from
    50 mm on the +x side), stored in `localStorage` and still there after a reload.
  - Save with a note, export (JSON download), import (via the file input; in a
    test, set `input.files` from a `DataTransfer` and dispatch `change`): a
    different saved C than the device's shows the "Apply it?" offer on connect;
    Apply / "Apply the saved calibration" / "Reset the device to nominal" send one
    `calibration/set` each. Invalid files (other JSON, not JSON, non-finite
    numbers) are refused and leave the saved calibration unchanged.
  - The test leaves a saved calibration and reference point in the browser's
    `localStorage` (`six-dof-platform.*` keys); clear them for a clean start.
- Interface check (edit `MOCK.endpoints` before connecting): a different
  `interface/hash/get` body → "built from different interface versions"; the entry
  deleted → "the firmware is older than the interface version check". In both cases
  the port is closed again (`FAKE_SERIAL.closes`) and Connect works afterwards.

Not covered: the real server shortens keys (to the fewest bytes that keep them
unique) and the client then does the same; the fake always uses 8-byte keys.

Last run: 2026-11-24, headless Chromium, all as above.

## Calibration wizard and center of mass

- Wizard: attach the generated sequence before connecting,
  `MOCK.endpoints[<average key>].sequence = MOCK.wizardAverages` (the fake then
  answers each `average` with the next element). It is one 6-case run (1 kg singles):
  empty, M0..M5, empty + 5 counts drift, generated with per-cell scales nominal ×
  0.95, 0.97, …, 1.05. Start (1000 g, no pairs), capture 8 times → results: ratios
  0.9500 … 1.0500, RMS error 0.0 mN, drift 5 counts, every case 1000.0 g at 0.0 mm
  position error. Apply sends one `calibration/set`; Back/Cancel step back.
- Center of mass (checkbox): "mass 1000.0 g, x 0.0 mm, y 0.0 mm (not zeroed!)",
  horizontal force and Mz 0.

## 3D view

The browser tool's headless Chromium runs without a GPU and Alpine's Chromium has
no software WebGL (SwiftShader), so the 3D view shows "3D view unavailable" there.
To render it in the VM, Mesa's software renderer is needed (`apk add
mesa-dri-gallium mesa-egl mesa-gles mesa-gbm`; part of David's VM image since
2026-11-25) and a separate Chromium, driven by `render-3d.mjs` over the DevTools
protocol in real time:

```sh
node web/test/render-3d.mjs file:///tmp/test-3d.html /tmp/view.png 4500 900x500
python3 web/test/pngdiff.py /tmp/without.png /tmp/with.png
```

Don't use `--virtual-time-budget` screenshots for anything animated: in that mode
headless Chromium runs timers but stops delivering `requestAnimationFrame`, so the
view stops updating (found 2026-11-25; the wizard's weights never appeared).
Viewing the PNGs with the agent's read tool currently aborts (TODO 3.1.1), so
compare screenshots with `pngdiff.py` instead (count, bounding box and colors of
the differing pixels).

`/tmp/test-3d.html` is a copy of `dist/six-dof.html` with, before `</body>`: a
style hiding everything but the canvas (`body > :not(canvas) { display: none }`),
optionally `localStorage.setItem('six-dof-platform.reference-point', '[50, 0, 0]')`,
the two fake-device scripts, and a `setTimeout` that clicks Connect after 1 s.
Expected (1 kg at the center): the whole stand in view; load cells slightly blue
(2 N compression each); orange force arrow ending at the reference point; with
x = 50 mm a purple double arrow of about 98 mm along −y (My = −0.49 N·m).
Wizard at case 1 (weight on M0) vs. the same page without the capture click: about
4,100 differing pixels in a 52 × 90 px region left of center, grey (195, 195, 194)
instead of the tan platform (2026-11-25). Orbit and zoom need a real browser.
