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
- Interface check (edit `MOCK.endpoints` before connecting): a different
  `interface/hash/get` body → "built from different interface versions"; the entry
  deleted → "the firmware is older than the interface version check". In both cases
  the port is closed again (`FAKE_SERIAL.closes`) and Connect works afterwards.

Not covered: the real server shortens keys (to the fewest bytes that keep them
unique) and the client then does the same; the fake always uses 8-byte keys.

Last run: 2026-11-24, headless Chromium, all as above.
