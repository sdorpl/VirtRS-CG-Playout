// Frame-locked Chromium capture in a worker thread (benchmark variant
// "chromium-frames"). Chromium draws only when asked
// (`HeadlessExperimental.beginFrame`), once per slot of the channel's frame
// grid, with the slot's time as frame time: animations advance exactly one
// frame per slot. No damage = no PNG. With `pages` > 1 that many browsers
// draw alternate slots, so PNG encoding runs in parallel (pages of one
// browser cannot: its compositor crashes on overlapping begin frames).
// DevTools, PNG decoding and cropping run here; the main thread only gets the
// visible rectangles, dated by slot.

import { spawn } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { parentPort, workerData } from "node:worker_threads";

import sharp from "sharp";

import { crop, visibleRects } from "../src/packets.js";

const { url, width, height, fps, browser, pages = 1 } = workerData;
const frameMs = 1000 / fps;
// Chromium's TimeTicks and process.hrtime are both CLOCK_MONOTONIC.
const mono = () => Number(process.hrtime.bigint()) / 1e6;

let stopped = false;
const children = new Set();
let lastPng = null;
// Screenshots being decoded; above pages + 1 a slow CPU drops frames instead of lagging.
let decoding = 0;

async function devtoolsUrl(profile) {
  const deadline = Date.now() + 30000;
  while (Date.now() < deadline) {
    try {
      const [port, wsPath] = (await readFile(path.join(profile, "DevToolsActivePort"), "utf8")).split("\n");
      if (port && wsPath) return `ws://127.0.0.1:${port.trim()}${wsPath.trim()}`;
    } catch {
      // not written yet
    }
    await sleep(100);
  }
  throw new Error("browser did not start");
}

function connect(endpoint) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(endpoint);
    const pending = new Map();
    let nextId = 0;
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const handler = pending.get(message.id);
      if (!handler) return;
      pending.delete(message.id);
      if (message.error) handler.reject(new Error(message.error.message));
      else handler.resolve(message.result);
    });
    socket.addEventListener("close", () => {
      for (const { reject: fail } of pending.values()) fail(new Error("DevTools closed"));
      pending.clear();
    });
    socket.addEventListener("error", () => reject(new Error(`DevTools connect ${endpoint}`)), { once: true });
    socket.addEventListener("open", () => resolve({
      call: (method, params = {}, sessionId) => new Promise((ok, fail) => {
        pending.set(++nextId, { resolve: ok, reject: fail });
        socket.send(JSON.stringify({ id: nextId, method, params, sessionId }));
      }),
      close: () => socket.close(),
    }), { once: true });
  });
}

async function decode(data, at) {
  const png = Buffer.from(data, "base64");
  lastPng = png;
  const { data: rgba, info } = await sharp(png).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  if (info.width !== width || info.height !== height) return;
  const rects = visibleRects(rgba, width, height).map((rect) => ({ ...rect, pixels: crop(rgba, width, rect) }));
  parentPort.postMessage({ type: "frame", at, rects });
}

/**
 * One page draws every `pages`-th slot of the grid, so PNG encoding of
 * several slots runs in parallel (each page in its own browser).
 */
async function pageLoop(devtools, index, stats) {
  const { targetId } = await devtools.call("Target.createTarget", { url: "about:blank", width, height, enableBeginFrameControl: true });
  const { sessionId } = await devtools.call("Target.attachToTarget", { targetId, flatten: true });
  const call = (method, params) => devtools.call(method, params, sessionId);
  await call("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: 1, mobile: false });
  await call("Emulation.setDefaultBackgroundColorOverride", { color: { r: 0, g: 0, b: 0, a: 0 } });
  call("Page.navigate", { url }).catch(() => {});

  const shot = { format: "png", optimizeForSpeed: true };
  const mine = (k) => k + ((index - (k % pages) + pages) % pages);
  let moving = false;
  let lastData = null;
  let lastShot = 0;
  let k = mine(Math.ceil(mono() / frameMs));

  while (!stopped) {
    const slot = k * frameMs;
    const wait = slot - mono();
    if (wait > 0) await sleep(wait);

    // A screenshot request always redraws and reports damage: while the page
    // moves, ask for one at once; once it stood still, probe for damage first.
    // A check screenshot every second catches a change the probes missed.
    const check = slot - lastShot >= 1000;
    let started = mono();
    let result = await call("HeadlessExperimental.beginFrame", { frameTimeTicks: slot, interval: frameMs, ...((moving || check) && { screenshot: shot }) });
    stats.drawMs += mono() - started;
    stats.slots += 1;
    if (result.screenshotData) lastShot = slot;
    if (!moving && !check && result.hasDamage) {
      lastShot = slot;
      started = mono();
      result = await call("HeadlessExperimental.beginFrame", { frameTimeTicks: slot + 0.001, interval: frameMs, screenshot: shot });
      stats.shotMs += mono() - started;
      stats.shots += 1;
    }

    const data = result.screenshotData;
    moving = Boolean(data) && data !== lastData;
    if (data && !moving) {
      // The page stood still: probes come next, but the frame after a
      // screenshot reports damage (Chromium 156) – an empty frame takes it.
      await call("HeadlessExperimental.beginFrame", { frameTimeTicks: slot + 0.002, interval: frameMs });
    }
    if (moving) {
      lastData = data;
      stats.damaged += 1;
      if (decoding < pages + 1) {
        decoding += 1;
        decode(data, slot)
          .catch((error) => parentPort.postMessage({ type: "warn", message: error.message }))
          .finally(() => { decoding -= 1; });
      } else {
        stats.dropped += 1;
      }
    }

    // Behind the grid: go on with this page's next slot from now; the missed ones repeat.
    const next = mine(Math.max(k + 1, Math.floor(mono() / frameMs)));
    stats.late += (next - k) / pages - 1;
    k = next;
    if (index === 0 && stats.slots % fps < pages) parentPort.postMessage({ type: "stats", stats });
  }
}

/** One browser drawing the slots of page `index`. */
async function runBrowser(index, stats) {
  const profile = await mkdtemp(path.join(os.tmpdir(), "virtrs-frames-"));
  const child = spawn(browser, [
    "--headless=new", "--disable-gpu", "--remote-debugging-port=0", `--user-data-dir=${profile}`,
    `--window-size=${width},${height}`, "--hide-scrollbars", "--mute-audio", "--no-first-run",
    "--no-default-browser-check", "--disable-extensions", "--force-device-scale-factor=1",
    "--disable-background-networking", "--disable-component-update", "--disable-default-apps",
    "--disable-sync", "--no-pings", "--renderer-process-limit=1", "--no-sandbox",
    // Not --disable-new-content-rendering-timeout: with it every frame reports damage.
    "--enable-begin-frame-control", "--run-all-compositor-stages-before-draw",
    "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  children.add(child);
  parentPort.postMessage({ type: "pid", pid: child.pid });
  // The last lines of Chromium's log go to the report when it dies.
  let tail = [];
  child.stderr.on("data", (data) => { tail = [...tail, ...data.toString().split("\n").filter(Boolean)].slice(-8); });
  child.once("exit", (code, signal) => {
    if (!stopped) parentPort.postMessage({ type: "error", message: `browser ${index} exited (${signal ?? code}): ${tail.join(" | ")}` });
  });

  try {
    const devtools = await connect(await devtoolsUrl(profile));
    await pageLoop(devtools, index, stats);
    devtools.close();
  } finally {
    child.kill("SIGKILL");
    await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
}

async function run() {
  const stats = { pages, slots: 0, damaged: 0, dropped: 0, late: 0, shots: 0, drawMs: 0, shotMs: 0 };
  await Promise.all(Array.from({ length: pages }, (_, index) => runBrowser(index, stats)));
}

parentPort.on("message", (message) => {
  if (message === "stop") {
    stopped = true;
    for (const child of children) child.kill("SIGKILL");
  } else if (message === "sample") {
    parentPort.postMessage({ type: "sample", png: lastPng });
  }
});

run().catch((error) => {
  if (!stopped) parentPort.postMessage({ type: "error", message: error.message });
});
