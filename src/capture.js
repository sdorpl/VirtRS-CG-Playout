// Burn-in source: one headless Chromium per channel size renders the VizRS
// `/output` page with a transparent background. Chromium sends a PNG
// (DevTools `Page.startScreencast`) only when the page changes; the newest
// one is decoded (libvips through `sharp` off the main thread, else the JS
// decoder) and kept as the current frame for all sessions, with a short
// history so each output frame can take the picture of its own moment. A
// picture is dated when Chromium drew it, not when its decoding finished.

import { spawn } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

import { crop, visibleRects } from "./packets.js";
import { decodePng } from "./png.js";
import { scaleRects } from "./scale.js";

// First start on a cold host (CI runner, small ARM VM) can take 15–20 s.
const START_TIMEOUT = 30000;
const RESTART_DELAY = 3000;
// Without sessions (ErsatzTV transcodes on demand) the page stops being
// captured after this long; the gap between two items is much shorter.
const IDLE_MS = 30000;
// Chromium composites at 60 Hz; the screencast takes every n-th frame.
const BROWSER_FPS = 60;
// With n > 1 a lone change (static page, last frame of an animation) may be
// skipped: after this long without frames a screenshot fetches the final state.
const SETTLE_MS = 150;
// Frames kept for sessions that write late (backpressure, busy event loop).
const HISTORY_MS = 500;
const BROWSERS = ["chromium-headless-shell", "chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome", "headless_shell"];

export function findBrowser(env = process.env) {
  if (env.VIZRS_BROWSER) return env.VIZRS_BROWSER;

  for (const dir of (env.PATH ?? "").split(path.delimiter)) {
    for (const name of BROWSERS) {
      const file = path.join(dir, name);
      try {
        accessSync(file, constants.X_OK);
        return file;
      } catch {
        // next
      }
    }
  }

  return null;
}

/** Screencast step that keeps frames at or above the channel rate: 25 fps takes every second frame. */
export function everyNthFrame(fps) {
  return Math.max(1, Math.floor(BROWSER_FPS / Math.max(1, fps || 0)));
}

/**
 * When Chromium drew a screencast frame (`metadata.timestamp`, seconds since
 * the epoch) on the `performance.now()` clock; arrival time if missing or off.
 */
export function drawnAt(timestamp, now = performance.now()) {
  const at = timestamp * 1000 - performance.timeOrigin;
  return Number.isFinite(at) && at <= now && at > now - 1000 ? at : now;
}

/** Native PNG decoder when `sharp` is installed: ~20 ms instead of ~150 ms at 1080p. */
const sharp = await import("sharp").then((m) => m.default, () => null);

export async function decodeFrame(png) {
  if (!sharp) return decodePng(png);
  const { data, info } = await sharp(png).ensureAlpha().raw().toBuffer({ resolveWithObject: true });
  return { width: info.width, height: info.height, rgba: data };
}

class DevTools {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 0;
    this.pending = new Map();
    this.onEvent = () => {};

    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);

      if (message.id !== undefined && this.pending.has(message.id)) {
        const { resolve, reject } = this.pending.get(message.id);
        this.pending.delete(message.id);
        if (message.error) reject(new Error(`${message.error.message}`));
        else resolve(message.result);
      } else if (message.method) {
        this.onEvent(message);
      }
    });
    socket.addEventListener("close", () => {
      for (const { reject } of this.pending.values()) reject(new Error("DevTools closed"));
      this.pending.clear();
      this.onEvent({ method: "DevTools.closed" });
    });
  }

  static open(url) {
    return new Promise((resolve, reject) => {
      const socket = new WebSocket(url);
      socket.addEventListener("open", () => resolve(new DevTools(socket)), { once: true });
      socket.addEventListener("error", () => reject(new Error(`DevTools connect ${url}`)), { once: true });
    });
  }

  call(method, params = {}, sessionId) {
    const id = ++this.nextId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject: (error) => reject(new Error(`${method}: ${error.message}`)) });
      this.socket.send(JSON.stringify({ id, method, params, sessionId }));
    });
  }

  close() {
    this.socket.close();
  }
}

export class Capture {
  /** Renders at `width`x`height`; frames come out at `outWidth`x`outHeight` (the channel). */
  constructor({ url, width, height, outWidth = width, outHeight = height, fps, browser, noSandbox, extraArgs = [], idleMs = IDLE_MS, log = console }) {
    this.url = url;
    this.width = width;
    this.height = height;
    this.outWidth = outWidth;
    this.outHeight = outHeight;
    this.fps = fps;
    this.browser = browser;
    this.noSandbox = noSandbox;
    this.extraArgs = extraArgs;
    this.log = log;
    // seq grows with every new picture; sessions compare it to what they sent.
    this.frame = { seq: 0, rects: [], at: performance.now() };
    this.history = [this.frame];
    this.stopped = false;
    this.status = "starting";
    this.pendingPng = null;
    this.decoding = false;
    this.idleMs = idleMs;
    this.users = 0;
    this.active = true;
    this.idleTimer = null;
    this.screencast = null;
  }

  get size() {
    return `${this.outWidth}x${this.outHeight}`;
  }

  get render() {
    return `${this.width}x${this.height}`;
  }

  get key() {
    return this.size === this.render ? this.size : `${this.size} (render ${this.render})`;
  }

  get scaled() {
    return this.size !== this.render;
  }

  start() {
    this.loop();
    return this;
  }

  /** A session starts reading frames. */
  acquire() {
    this.users += 1;
    clearTimeout(this.idleTimer);
    this.idleTimer = null;
    if (!this.active) this.setActive(true);
  }

  /** The last session left: pause the capture once idle for `idleMs`. */
  release() {
    this.users = Math.max(0, this.users - 1);
    if (this.users || this.idleTimer) return;
    this.idleTimer = setTimeout(() => {
      this.idleTimer = null;
      this.setActive(false);
    }, this.idleMs);
    this.idleTimer.unref();
  }

  setActive(active) {
    this.active = active;
    this.log.info(`burn-in ${this.key}: ${active ? "resumed" : "paused, no sessions"}`);
    if (!this.screencast) return;
    this.status = active ? "capturing" : "paused";
    this.screencast(active).catch((error) => this.log.warn(`burn-in ${this.key}: ${error.message}`));
  }

  async loop() {
    while (!this.stopped) {
      try {
        await this.runBrowser();
      } catch (error) {
        if (!this.stopped) this.log.error(`burn-in ${this.key}: ${error.message}`);
      }

      if (this.stopped) break;
      // No graphics until the browser is back; playout continues.
      this.setFrame([]);
      this.status = "restarting";
      await sleep(RESTART_DELAY);
    }
  }

  /** `at`: when the picture was drawn (`performance.now()`); history stays in order. */
  setFrame(rects, at = performance.now()) {
    if (!rects.length && !this.frame.rects.length) return;
    at = Math.max(at, this.frame.at);
    this.frame = { seq: this.frame.seq + 1, rects, at };
    this.history.push(this.frame);
    // Keep the frame that was current when the window starts.
    while (this.history.length > 1 && this.history[1].at <= at - HISTORY_MS) this.history.shift();
  }

  /** The picture shown at `time` (`performance.now()`), like a playout taking the current frame. */
  frameAt(time) {
    for (let i = this.history.length - 1; i > 0; i--) {
      if (this.history[i].at <= time) return this.history[i];
    }
    return this.history[0];
  }

  async runBrowser() {
    if (!this.browser) throw new Error("no Chromium found (set VIZRS_BROWSER)");

    const profile = await mkdtemp(path.join(os.tmpdir(), "virtrs-cg-"));
    const args = [
      "--headless=new",
      "--disable-gpu",
      "--remote-debugging-port=0",
      `--user-data-dir=${profile}`,
      `--window-size=${this.width},${this.height}`,
      "--hide-scrollbars",
      "--mute-audio",
      "--no-first-run",
      "--no-default-browser-check",
      "--disable-extensions",
      "--disable-background-timer-throttling",
      "--disable-renderer-backgrounding",
      "--disable-backgrounding-occluded-windows",
      "--force-device-scale-factor=1",
      "--disable-background-networking",
      "--disable-component-update",
      "--disable-default-apps",
      "--disable-sync",
      "--metrics-recording-only",
      "--no-pings",
      "--renderer-process-limit=1",
      "--disable-features=Translate,MediaRouter,OptimizationHints,AutofillServerCommunication",
      ...(this.noSandbox ? ["--no-sandbox"] : []),
      ...this.extraArgs,
      this.url,
    ];
    const child = spawn(this.browser, args, { stdio: "ignore" });
    const exited = new Promise((resolve) => child.once("exit", resolve));
    child.once("error", (error) => this.log.error(`burn-in ${this.key}: ${error.message}`));
    this.child = child;

    try {
      const endpoint = await this.devtoolsUrl(profile, exited);
      const devtools = await DevTools.open(endpoint);
      this.devtools = devtools;

      try {
        await this.session(devtools, exited);
      } finally {
        devtools.close();
      }
    } finally {
      child.kill("SIGKILL");
      await exited;
      await rm(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    }
  }

  async devtoolsUrl(profile, exited) {
    const deadline = Date.now() + START_TIMEOUT;
    let done = false;
    exited.then(() => { done = true; });

    while (Date.now() < deadline && !done) {
      try {
        const [port, wsPath] = (await readFile(path.join(profile, "DevToolsActivePort"), "utf8")).split("\n");
        if (port && wsPath) return `ws://127.0.0.1:${port.trim()}${wsPath.trim()}`;
      } catch {
        // not written yet
      }
      await sleep(100);
    }

    throw new Error(done ? "browser exited" : "browser did not start");
  }

  /** The startup tab once it shows the page (it starts as `about:blank`). */
  async pageTarget(devtools) {
    const deadline = Date.now() + START_TIMEOUT;

    while (Date.now() < deadline) {
      const { targetInfos } = await devtools.call("Target.getTargets");
      const page = targetInfos.find((info) => info.type === "page" && info.url.startsWith("http"));
      if (page) return page.targetId;
      await sleep(100);
    }

    throw new Error("page did not open");
  }

  async session(devtools, exited) {
    const targetId = await this.pageTarget(devtools);
    const { sessionId } = await devtools.call("Target.attachToTarget", { targetId, flatten: true });
    const call = (method, params) => devtools.call(method, params, sessionId);

    const step = everyNthFrame(this.fps);
    let settleTimer = null;
    const settle = () => {
      clearTimeout(settleTimer);
      if (step === 1 || !this.active) return;
      settleTimer = setTimeout(() => {
        call("Page.captureScreenshot", { format: "png" })
          .then(({ data }) => this.active && this.pushPng(data, performance.now()))
          .catch((error) => this.log.warn(`burn-in ${this.key}: ${error.message}`));
      }, SETTLE_MS);
    };

    const ended = new Promise((resolve, reject) => {
      exited.then(() => reject(new Error("browser exited")));
      devtools.onEvent = (message) => {
        if (message.sessionId === sessionId && message.method === "Page.screencastFrame") {
          // Ack at once so Chromium renders the next frame while this one is decoded.
          call("Page.screencastFrameAck", { sessionId: message.params.sessionId }).catch(() => {});
          this.pushPng(message.params.data, drawnAt(message.params.metadata?.timestamp));
          settle();
        } else if (["Inspector.targetCrashed", "Target.targetCrashed", "Target.detachedFromTarget", "DevTools.closed"].includes(message.method)) {
          reject(new Error(`page closed (${message.method})`));
        }
      };
    });
    // Rejections before the setup is done surface through `await ended` below.
    ended.catch(() => {});

    await call("Emulation.setDeviceMetricsOverride", { width: this.width, height: this.height, deviceScaleFactor: 1, mobile: false });
    // Transparency only through DevTools: `--default-background-color` crashes current renderers.
    await call("Emulation.setDefaultBackgroundColorOverride", { color: { r: 0, g: 0, b: 0, a: 0 } });
    await call("Page.enable");

    // Not faster than the channel needs, as in VizRS ffPlayout System: half the PNG work at 24–30 fps.
    const screencast = { format: "png", maxWidth: this.width, maxHeight: this.height, everyNthFrame: step };
    this.screencast = async (active) => {
      if (active) {
        await call("Page.startScreencast", screencast);
        settle();
      } else {
        await call("Page.stopScreencast");
      }
    };
    if (this.active) await this.screencast(true);

    this.status = this.active ? "capturing" : "paused";
    this.log.info(`burn-in ${this.key}: capturing ${this.url}`);
    try {
      await ended;
    } finally {
      clearTimeout(settleTimer);
      this.screencast = null;
    }
  }

  /** Keeps only the newest undecoded frame: animations drop frames, never lag. */
  pushPng(data, at = performance.now()) {
    this.pendingPng = { data, at };
    if (this.decoding) return;
    this.decoding = true;

    (async () => {
      try {
        while (this.pendingPng) {
          const { data, at } = this.pendingPng;
          this.pendingPng = null;
          await this.applyPng(Buffer.from(data, "base64"), at);
        }
      } finally {
        this.decoding = false;
      }
    })();
  }

  async applyPng(png, at) {
    try {
      const { width, height, rgba } = await decodeFrame(png);
      if (width !== this.width || height !== this.height) {
        this.log.warn(`burn-in ${this.key}: frame ${width}x${height} skipped`);
        return;
      }

      const rects = visibleRects(rgba, width, height);
      this.setFrame(this.scaled
        ? await scaleRects(rgba, width, height, rects, this.outWidth, this.outHeight)
        : rects.map((rect) => ({ ...rect, pixels: crop(rgba, width, rect) })), at);
    } catch (error) {
      this.log.warn(`burn-in ${this.key}: ${error.message}`);
    }
  }

  stop() {
    this.stopped = true;
    clearTimeout(this.idleTimer);
    this.child?.kill("SIGKILL");
  }
}
