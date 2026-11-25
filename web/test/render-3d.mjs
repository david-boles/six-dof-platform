// Renders a page with the 3D view in a separate headless Chromium with software
// WebGL (Mesa) and takes a screenshot, driving it over the Chrome DevTools
// Protocol in real time. (With `--virtual-time-budget`, headless Chromium runs
// timers but stops delivering requestAnimationFrame, so the 3D view doesn't
// update; see README.md, "3D view".)
//
// Usage: node render-3d.mjs <url> <out.png> [sample ms,...] [WIDTHxHEIGHT] [js expression]
//   At each sample time (ms after navigating) the expression is evaluated and
//   printed; the screenshot is taken after the last sample.
// Needs Node 22+ (built-in WebSocket) and Mesa (apk add mesa-dri-gallium
// mesa-egl mesa-gles mesa-gbm).
// PREFERS_COLOR_SCHEME=dark (or light) emulates the browser's theme preference.
// SCREENSHOT_EACH=1 also saves a screenshot at every sample time, as
// <out without .png>-<ms>.png (e.g. to follow an animation).
import { spawn } from "node:child_process";
import { rmSync, writeFileSync } from "node:fs";

const [url, out, samplesArg = "3000", size = "900x500", expression = "document.title"] =
  process.argv.slice(2);
if (!url || !out) {
  console.error("usage: node render-3d.mjs <url> <out.png> [ms,...] [WxH] [expression]");
  process.exit(1);
}
const samples = samplesArg.split(",").map(Number);
const [width, height] = size.split("x").map(Number);
const port = 9333;
const profile = "/tmp/cr-gl";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

rmSync(profile, { recursive: true, force: true });
// Own process group (`detached`), so all of Chromium's helper processes can be
// killed together at the end.
const chrome = spawn(
  "chromium",
  [
    "--headless=new", "--no-sandbox", "--use-angle=gl-egl", "--ignore-gpu-blocklist",
    "--enable-gpu", `--user-data-dir=${profile}`, `--remote-debugging-port=${port}`,
    `--window-size=${width},${height}`, "about:blank",
  ],
  { stdio: "ignore", detached: true },
);
const killAll = () => {
  try {
    process.kill(-chrome.pid, "SIGKILL");
  } catch {}
};
const watchdog = setTimeout(() => {
  console.error("watchdog: no result after 60 s");
  killAll();
  process.exit(2);
}, 60000);

try {
  let pages;
  for (let i = 0; i < 50 && !pages; i++) {
    await sleep(200);
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      pages = list.filter((t) => t.type === "page");
    } catch {}
  }
  if (!pages?.length) throw new Error("Chromium didn't start");
  const ws = new WebSocket(pages[0].webSocketDebuggerUrl);
  await new Promise((r) => ws.addEventListener("open", r));
  let nextId = 1;
  const pending = new Map();
  ws.addEventListener("message", (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.id && pending.has(msg.id)) {
      pending.get(msg.id)(msg);
      pending.delete(msg.id);
    }
    // Print the page's console errors and uncaught exceptions (e.g. a panic or a
    // shader that fails to compile).
    if (msg.method === "Runtime.consoleAPICalled" && msg.params.type === "error") {
      const text = msg.params.args.map((a) => a.value ?? a.description ?? "").join(" ");
      console.log("console.error:", text.slice(0, 2000));
    }
    if (msg.method === "Runtime.exceptionThrown") {
      console.log("exception:", JSON.stringify(msg.params.exceptionDetails).slice(0, 2000));
    }
  });
  const send = (method, params = {}) =>
    new Promise((resolve) => {
      const id = nextId++;
      pending.set(id, resolve);
      ws.send(JSON.stringify({ id, method, params }));
    });

  await send("Page.enable");
  await send("Runtime.enable");
  if (process.env.PREFERS_COLOR_SCHEME) {
    await send("Emulation.setEmulatedMedia", {
      features: [{ name: "prefers-color-scheme", value: process.env.PREFERS_COLOR_SCHEME }],
    });
  }
  await send("Page.navigate", { url });
  const start = Date.now();
  for (const t of samples) {
    await sleep(Math.max(0, t - (Date.now() - start)));
    const r = await send("Runtime.evaluate", { expression, returnByValue: true });
    console.log(`@${t} ms:`, JSON.stringify(r.result?.result?.value));
    if (process.env.SCREENSHOT_EACH) {
      const shot = await send("Page.captureScreenshot", { format: "png" });
      const file = out.replace(/\.png$/, "") + `-${t}.png`;
      writeFileSync(file, Buffer.from(shot.result.data, "base64"));
    }
  }
  const shot = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(out, Buffer.from(shot.result.data, "base64"));
  console.log("wrote", out);
  ws.close();
} finally {
  clearTimeout(watchdog);
  killAll();
}
