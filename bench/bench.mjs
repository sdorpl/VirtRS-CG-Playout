#!/usr/bin/env node
// Engine benchmark: the same page rendered headless by
//   chromium         the sidecar's capture (src/capture.js: screencast PNG,
//                    sharp decode, crop) and its packet stream (src/stream.js),
//   chromium-frames  frame-locked Chromium (frames-worker.mjs: one drawn
//                    frame per output slot, decoding in a worker thread),
//   wpe              WPE WebKit through GStreamer `wpevideosrc` (raw RGBA).
// Each run measures CPU and memory of the engine and of this process, the
// rate of new pictures and, on the animated scene, how evenly the motion
// advances in the 25 fps output. The report is served at http://<host>:PORT/.

import { execFile, spawn } from "node:child_process";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import { EventEmitter } from "node:events";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { setTimeout as sleep } from "node:timers/promises";
import { Worker } from "node:worker_threads";

import sharp from "sharp";

import { Capture, findBrowser } from "../src/capture.js";
import { MAGIC, Type, crop, visibleRects } from "../src/packets.js";
import { streamFrames } from "../src/stream.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const env = process.env;
const list = (value) => value.split(",").map((s) => s.trim()).filter(Boolean);
const SIZES = list(env.BENCH_SIZES || env.BENCH_SIZE || "1920x1080").map((size) => size.split("x").map(Number));
const FPS = Number(env.BENCH_FPS || 25);
const WARMUP = Number(env.BENCH_WARMUP || 10);
const DURATION = Number(env.BENCH_DURATION || 60);
const PORT = Number(env.PORT || 7790);
const RESULTS = env.BENCH_RESULTS || path.join(here, "results");
const ENGINES = list(env.BENCH_ENGINES || "chromium,chromium-frames");
const SCENES = list(env.BENCH_SCENES || (env.BENCH_URL ? "static,animated,url" : "static,animated"));
// Browsers of chromium-frames drawing alternate slots (parallel PNG encoding).
const FRAME_PAGES = Number(env.BENCH_FRAME_PAGES || 2);
const CLK_TCK = 100;

const log = {
  info: (message) => console.log(message),
  warn: (message) => console.warn(message),
  error: (message) => console.error(message),
};
const quiet = { ...log, info: () => {} };

const state = { status: "starting", current: null, results: [], system: {}, started: new Date().toISOString() };

// --- page server: scene, SSE headlines, report ---

const headlines = [
  ["Wiadomości", "Serwis informacyjny o 19:30"],
  ["Pogoda", "Jutro słonecznie, do 24°C"],
  ["Sport", "Wyniki kolejki ekstraklasy"],
  ["Za chwilę", "Film dokumentalny"],
];
const sse = new Set();
setInterval(() => {
  const [headline, sub] = headlines[Math.floor(Date.now() / 3000) % headlines.length];
  for (const res of sse) res.write(`data: ${JSON.stringify({ headline, sub })}\n\n`);
}, 3000).unref();

const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, "http://localhost");
  try {
    if (url.pathname === "/scene.html") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(await readFile(path.join(here, "scene.html")));
    } else if (url.pathname === "/events") {
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      res.write("retry: 1000\n\n");
      sse.add(res);
      req.on("close", () => sse.delete(res));
    } else if (url.pathname === "/results.json") {
      res.writeHead(200, { "content-type": "application/json" });
      res.end(JSON.stringify(state, null, 2));
    } else if (/^\/frames\/[\w-]+\.png$/.test(url.pathname)) {
      res.writeHead(200, { "content-type": "image/png", "cache-control": "no-cache" });
      res.end(await readFile(path.join(RESULTS, path.basename(url.pathname))));
    } else if (url.pathname === "/") {
      res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
      res.end(report());
    } else {
      res.writeHead(404).end();
    }
  } catch (error) {
    res.writeHead(500).end(error.message);
  }
});

// --- process statistics from /proc ---

async function children() {
  const map = new Map();
  for (const name of await readdir("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      const stat = await readFile(`/proc/${name}/stat`, "utf8");
      const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      map.set(Number(name), { ppid: Number(fields[1]), ticks: Number(fields[11]) + Number(fields[12]) });
    } catch {
      // gone
    }
  }
  return map;
}

/** The processes `roots` and all their descendants with their CPU ticks. */
async function tree(roots) {
  const all = await children();
  const pids = new Set(roots);
  let grew = true;
  while (grew) {
    grew = false;
    for (const [pid, info] of all) {
      if (!pids.has(pid) && pids.has(info.ppid)) {
        pids.add(pid);
        grew = true;
      }
    }
  }
  return new Map([...pids].filter((pid) => all.has(pid)).map((pid) => [pid, all.get(pid).ticks]));
}

async function pss(pids) {
  let kb = 0;
  for (const pid of pids) {
    try {
      const rollup = await readFile(`/proc/${pid}/smaps_rollup`, "utf8");
      kb += Number(/^Pss:\s+(\d+)/m.exec(rollup)?.[1] ?? 0);
    } catch {
      // gone
    }
  }
  return kb / 1024;
}

// --- output motion: the scene's marker (animation step) in every output frame ---

const markerStep = (r, g, a) => (a ? r + 256 * g : null);

/** Animation step in pixel (0, 0) of a Rectangles payload, or null. */
function packetStep(packet) {
  const count = packet.readUInt16BE(11);
  let offset = 13;
  for (let i = 0; i < count; i++) {
    const x = packet.readUInt16BE(offset);
    const y = packet.readUInt16BE(offset + 2);
    const w = packet.readUInt16BE(offset + 4);
    const h = packet.readUInt16BE(offset + 6);
    offset += 8;
    if (x === 0 && y === 0 && w > 0 && h > 0) return markerStep(packet[offset], packet[offset + 1], packet[offset + 3]);
    offset += w * h * 4;
  }
  return null;
}

/** The sidecar's packet stream on `capture`, read as ErsatzTV would: one step per output frame. */
function outputProbe(capture) {
  const steps = [];
  let last = null;
  const response = new EventEmitter();
  response.write = (packet) => {
    if (packet.readUInt32BE(0) !== MAGIC) return true;
    const type = packet[6];
    if (type === Type.Rectangles) last = packetStep(packet);
    else if (type === Type.Clear) last = null;
    const frames = type === Type.Repeat ? packet.readUInt32BE(11) + 1 : 1;
    for (let i = 0; i < frames; i++) steps.push(last);
    return true;
  };
  const stream = streamFrames(capture, response, { fps: FPS, leadSeconds: 0 });
  return { steps, stop: () => stream.stop() };
}

/** Share of output frames whose animation moved by exactly one step, stood or skipped. */
function motion(steps) {
  let one = 0;
  let stood = 0;
  let skipped = 0;
  for (let i = 1; i < steps.length; i++) {
    if (steps[i] === null || steps[i - 1] === null) continue;
    const delta = (steps[i] - steps[i - 1] + 65536) % 65536;
    if (delta === 1) one += 1;
    else if (delta === 0) stood += 1;
    else if (delta < 1000) skipped += 1;
  }
  const total = one + stood + skipped;
  if (!total) return null;
  const pct = (n) => Math.round((n / total) * 1000) / 10;
  return { even: pct(one), stood: pct(stood), skipped: pct(skipped) };
}

// --- engines ---

class ChromiumEngine {
  constructor(url, width, height) {
    const engine = this;
    this.frames = [];
    this.lastPng = null;
    // The sidecar's own capture: screencast every n-th frame, sharp decode, crop.
    this.capture = new (class extends Capture {
      setFrame(rects, at) {
        super.setFrame(rects, at);
        if (rects.length) engine.frames.push(performance.now());
      }
      async applyPng(png, at) {
        engine.lastPng = png;
        return super.applyPng(png, at);
      }
    })({ url, width, height, fps: FPS, browser: findBrowser(), noSandbox: true, log: quiet });
  }

  start() {
    if (!this.capture.browser) throw new Error("Chromium not found (VIZRS_BROWSER)");
    this.capture.start();
  }

  get pids() {
    return [this.capture.child?.pid];
  }

  probe() {
    return outputProbe(this.capture);
  }

  async sample(file) {
    if (this.lastPng) await writeFile(file, this.lastPng);
  }

  stop() {
    this.capture.stop();
  }
}

/**
 * Frames of chromium-frames by slot time, for the sidecar's packet stream
 * (`frame`, `frameAt` as in Capture). Pages finish out of order: a frame is
 * inserted at its slot; one older than the history is dropped.
 */
class FrameStore {
  constructor() {
    this.seq = 0;
    this.frame = { seq: 0, rects: [], at: performance.now() };
    this.history = [this.frame];
  }

  add(rects, at) {
    if (at < this.history[0].at) return false;
    const frame = { seq: ++this.seq, rects, at };
    let i = this.history.length;
    while (i > 0 && this.history[i - 1].at > at) i -= 1;
    this.history.splice(i, 0, frame);
    this.frame = this.history.at(-1);
    while (this.history.length > 1 && this.history[1].at <= this.frame.at - 500) this.history.shift();
    return true;
  }

  frameAt(time) {
    for (let i = this.history.length - 1; i > 0; i--) {
      if (this.history[i].at <= time) return this.history[i];
    }
    return this.history[0];
  }
}

class FramesEngine {
  constructor(url, width, height) {
    this.url = url;
    this.width = width;
    this.height = height;
    this.frames = [];
    this.stats = {};
    this.pidList = [];
    this.capture = new FrameStore();
  }

  start() {
    const browser = findBrowser();
    if (!browser) throw new Error("Chromium not found (VIZRS_BROWSER)");
    // Slots are dated on CLOCK_MONOTONIC; frames are kept on performance.now().
    const offset = Number(process.hrtime.bigint()) / 1e6 - performance.now();
    this.worker = new Worker(new URL("./frames-worker.mjs", import.meta.url), {
      workerData: { url: this.url, width: this.width, height: this.height, fps: FPS, browser, pages: FRAME_PAGES },
    });
    this.worker.on("message", (message) => {
      if (message.type === "pid") this.pidList.push(message.pid);
      else if (message.type === "stats") this.stats = message.stats;
      else if (message.type === "warn") log.warn(`chromium-frames: ${message.message}`);
      else if (message.type === "error") log.error(`chromium-frames: ${message.message}`);
      else if (message.type === "frame") {
        const rects = message.rects.map((rect) => ({ ...rect, pixels: Buffer.from(rect.pixels.buffer, rect.pixels.byteOffset, rect.pixels.length) }));
        if (this.capture.add(rects, message.at - offset) && rects.length) this.frames.push(performance.now());
      } else if (message.type === "sample") this.samplePng?.(message.png);
    });
    this.worker.on("error", (error) => log.error(`chromium-frames: ${error.message}`));
  }

  get pids() {
    return this.pidList;
  }

  probe() {
    return outputProbe(this.capture);
  }

  async sample(file) {
    const png = await new Promise((resolve) => {
      this.samplePng = resolve;
      this.worker.postMessage("sample");
      setTimeout(() => resolve(null), 2000);
    });
    if (png) await writeFile(file, Buffer.from(png));
  }

  stop() {
    this.worker?.postMessage("stop");
    for (const pid of this.pidList) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // gone
      }
    }
    setTimeout(() => this.worker?.terminate(), 2000).unref();
  }
}

class WpeEngine {
  constructor(url, width, height) {
    this.url = url;
    this.width = width;
    this.height = height;
    this.frames = [];
    this.steps = null;
    this.last = null;
    this.size = width * height * 4;
  }

  start() {
    const args = [
      "-q", "wpevideosrc", `location=${this.url}`, "draw-background=false",
      "!", `video/x-raw,format=BGRA,width=${this.width},height=${this.height},framerate=${FPS}/1`,
      "!", "videoconvert", "!", "video/x-raw,format=RGBA",
      "!", "fdsink", "fd=1",
    ];
    // WebKit's own sandbox (bubblewrap) cannot start in a container, as Chromium's.
    this.child = spawn("gst-launch-1.0", args, {
      stdio: ["ignore", "pipe", "pipe"],
      env: { ...env, WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS: "1" },
    });
    this.child.on("error", (error) => log.error(`wpe: ${error.message}`));
    this.child.stderr.on("data", (data) => {
      const text = data.toString().trim();
      if (text) log.warn(`wpe: ${text.split("\n")[0]}`);
    });

    let buffer = Buffer.alloc(this.size);
    let filled = 0;
    this.child.stdout.on("data", (chunk) => {
      let offset = 0;
      while (offset < chunk.length) {
        const n = Math.min(chunk.length - offset, this.size - filled);
        chunk.copy(buffer, filled, offset, offset + n);
        filled += n;
        offset += n;
        if (filled === this.size) {
          // Every raw frame is an output frame.
          this.steps?.push(markerStep(buffer[0], buffer[1], buffer[3]));
          // Only changed ones are cropped, as with Chromium.
          if (!this.last || !buffer.equals(this.last)) {
            const rects = visibleRects(buffer, this.width, this.height);
            rects.map((rect) => crop(buffer, this.width, rect));
            if (rects.length) this.frames.push(performance.now());
            const done = this.last ?? Buffer.alloc(this.size);
            this.last = buffer;
            buffer = done;
          }
          filled = 0;
        }
      }
    });
  }

  get pids() {
    return [this.child?.pid];
  }

  probe() {
    this.steps = [];
    return { steps: this.steps, stop: () => { this.steps = null; } };
  }

  async sample(file) {
    if (this.last) await sharp(this.last, { raw: { width: this.width, height: this.height, channels: 4 } }).png().toFile(file);
  }

  stop() {
    this.child?.kill("SIGKILL");
  }
}

const ENGINE_CLASSES = { chromium: ChromiumEngine, "chromium-frames": FramesEngine, wpe: WpeEngine };

// --- one run ---

function percentile(values, p) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
}

async function run(engineName, scene, [width, height]) {
  const size = `${width}x${height}`;
  const url = scene === "url" ? env.BENCH_URL : `http://127.0.0.1:${PORT}/scene.html?scene=${scene}&fps=${FPS}`;
  const result = { engine: engineName, scene, size, url, ok: false };
  const Engine = ENGINE_CLASSES[engineName];
  if (!Engine) return { ...result, error: `unknown engine (${Object.keys(ENGINE_CLASSES).join(", ")})` };
  let engine = new Engine(url, width, height);
  const label = `${engineName} / ${scene} / ${size}`;
  state.current = label;
  log.info(`run ${label}: ${url}`);

  try {
    // A start that shows nothing gets one more try: one race should not cost the whole run.
    for (let attempt = 1; ; attempt++) {
      const launched = performance.now();
      engine.start();
      while (!engine.frames.length && performance.now() - launched < 60000) await sleep(200);
      if (engine.frames.length) {
        result.startup_s = Number(((performance.now() - launched) / 1000).toFixed(1));
        break;
      }
      engine.stop();
      if (attempt === 2) throw new Error("no picture within 60 s (2 tries)");
      log.warn("  no picture within 60 s, starting again");
      await sleep(3000);
      engine = new Engine(url, width, height);
    }
    state.current = `${label} (warm-up)`;
    await sleep(WARMUP * 1000);
    state.current = `${label} (measuring ${DURATION} s)`;

    const pids = engine.pids;
    let before = await tree(pids);
    let engineTicks = 0;
    const cpu0 = process.cpuUsage();
    const t0 = performance.now();
    const frames0 = engine.frames.length;
    const probe = engine.probe();
    const pssSamples = [];
    let rssMax = 0;

    while (performance.now() - t0 < DURATION * 1000) {
      await sleep(1000);
      const after = await tree(pids);
      for (const [p, ticks] of after) engineTicks += ticks - (before.get(p) ?? 0);
      before = after;
      pssSamples.push(await pss(after.keys()));
      rssMax = Math.max(rssMax, process.memoryUsage().rss / 1048576);
    }

    probe.stop();
    const seconds = (performance.now() - t0) / 1000;
    const cpu = process.cpuUsage(cpu0);
    const frames = engine.frames.slice(frames0);
    const gaps = frames.slice(1).map((t, i) => t - frames[i]);
    Object.assign(result, {
      ok: true,
      seconds: Number(seconds.toFixed(1)),
      processes: before.size,
      fps: Number((frames.length / seconds).toFixed(1)),
      gap_p95_ms: Math.round(percentile(gaps, 0.95)),
      gap_max_ms: Math.round(Math.max(0, ...gaps)),
      motion: motion(probe.steps),
      cpu_engine: Math.round(((engineTicks / CLK_TCK) / seconds) * 100),
      cpu_node: Math.round(((cpu.user + cpu.system) / 1e6 / seconds) * 100),
      pss_engine_mb: Math.round(Math.max(...pssSamples)),
      rss_node_mb: Math.round(rssMax),
    });
    if (engine.stats) result.frames_worker = engine.stats;
    result.cpu_total = result.cpu_engine + result.cpu_node;
    const file = `${engineName}-${scene}-${size}.png`;
    await engine.sample(path.join(RESULTS, file));
    result.frame = `frames/${file}`;
    const even = result.motion ? `, motion ${result.motion.even}% even / ${result.motion.stood}% stood / ${result.motion.skipped}% skipped` : "";
    log.info(`  fps ${result.fps}, CPU ${result.cpu_engine}% + ${result.cpu_node}% (node), PSS ${result.pss_engine_mb} MB${even}`);
  } catch (error) {
    result.error = error.message;
    log.error(`  failed: ${error.message}`);
  } finally {
    engine.stop();
    await sleep(3000);
  }
  return result;
}

// --- environment ---

const exec = promisify(execFile);
async function version(cmd, args) {
  try {
    return (await exec(cmd, args)).stdout.trim().split("\n")[0];
  } catch {
    return "brak";
  }
}

async function system() {
  return {
    arch: os.arch(),
    cpu: os.cpus()[0]?.model ?? "?",
    cores: os.availableParallelism(),
    memory_gb: Math.round(os.totalmem() / 1073741824),
    node: process.version,
    chromium: await version(findBrowser() ?? "chromium-headless-shell", ["--version"]),
    wpe: await version("dpkg-query", ["-W", "-f=${Version}", "libwpewebkit-2.0-1"]),
    gstreamer: await version("gst-launch-1.0", ["--version"]),
    sizes: SIZES.map(([w, h]) => `${w}x${h}`).join(", "),
    fps: FPS,
    warmup_s: WARMUP,
    duration_s: DURATION,
  };
}

// --- report ---

const esc = (value) => String(value ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

function report() {
  const running = state.status !== "done";
  const motionCell = (m) => (m ? `<b>${m.even}%</b> / ${m.stood}% / ${m.skipped}%` : '<span class="mut">–</span>');
  const rows = state.results.map((r) => r.ok
    ? `<tr><td>${esc(r.engine)}</td><td>${esc(r.scene)}</td><td>${esc(r.size)}</td><td>${r.fps}</td><td>${motionCell(r.motion)}</td><td>${r.gap_p95_ms} / ${r.gap_max_ms}</td><td><b>${r.cpu_total}%</b></td><td>${r.cpu_engine}%</td><td>${r.cpu_node}%</td><td>${r.pss_engine_mb}</td><td>${r.rss_node_mb}</td></tr>`
    : `<tr><td>${esc(r.engine)}</td><td>${esc(r.scene)}</td><td>${esc(r.size)}</td><td colspan="8" class="err">błąd: ${esc(r.error)}</td></tr>`).join("");
  const groups = [...new Set(state.results.map((r) => `${r.scene} ${r.size}`))];
  const frames = groups.map((group) => `<h3>${esc(group)}</h3><div class="frames">${state.results
    .filter((r) => `${r.scene} ${r.size}` === group && r.frame)
    .map((r) => `<figure><img src="${esc(r.frame)}?${Date.now()}" alt=""><figcaption>${esc(r.engine)}</figcaption></figure>`).join("")}</div>`).join("");
  const sys = Object.entries(state.system).map(([k, v]) => `<tr><th>${esc(k)}</th><td>${esc(v)}</td></tr>`).join("");
  return `<!doctype html><html lang="pl"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
${running ? '<meta http-equiv="refresh" content="5">' : ""}<title>Benchmark silników</title><style>
:root{--bg:#fff;--fg:#111;--mut:#666;--line:#ddd;--err:#b91c1c}
@media (prefers-color-scheme:dark){:root{--bg:#111;--fg:#eee;--mut:#999;--line:#333;--err:#f87171}}
body{background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif;margin:0 auto;max-width:1200px;padding:16px}
table{border-collapse:collapse;width:100%;margin:8px 0 24px;overflow-x:auto;display:block}th,td{border-bottom:1px solid var(--line);padding:6px 10px;text-align:left;white-space:nowrap}
.mut{color:var(--mut)}.err{color:var(--err)}.frames{display:flex;gap:12px;flex-wrap:wrap}figure{margin:0;flex:1 1 320px}
img{width:100%;background:repeating-conic-gradient(#888 0 25%,#bbb 0 50%) 0 0/20px 20px;border:1px solid var(--line)}figcaption{color:var(--mut)}
</style></head><body><h1>Benchmark silników</h1>
<p class="mut">Stan: ${esc(running ? `w toku – ${state.current ?? state.status}` : "zakończony")} · start ${esc(state.started)}</p>
<table><thead><tr><th>silnik</th><th>scena</th><th>rozmiar</th><th>nowe klatki/s</th><th>ruch na wyjściu: równo / stoi / przeskok</th><th>przerwa p95 / max [ms]</th><th>CPU razem</th><th>CPU silnik</th><th>CPU node</th><th>PSS silnika [MB]</th><th>RSS node [MB]</th></tr></thead><tbody>${rows}</tbody></table>
<p class="mut">Ruch na wyjściu (scena animated): klatki wyjścia ${FPS} kl./s, w których animacja poszła dokładnie o jeden krok (ideał 100%), stała w miejscu albo przeskoczyła krok – odczytane ze znacznika w rogu sceny. CPU w procentach jednego rdzenia (100% = cały rdzeń). „CPU node” to dekodowanie PNG albo odbiór surowych klatek i przycinanie – praca sidecara.</p>
<h2>Klatki (na szachownicy = przezroczystość)</h2>${frames || '<p class="mut">jeszcze brak</p>'}
<h2>System</h2><table>${sys}</table><p class="mut"><a href="results.json">results.json</a></p></body></html>`;
}

// --- main ---

await mkdir(RESULTS, { recursive: true });
await new Promise((resolve) => server.listen(PORT, "0.0.0.0", resolve));
log.info(`benchmark: report at http://0.0.0.0:${PORT}/`);
state.system = await system();
log.info(JSON.stringify(state.system));
state.status = "running";

for (const size of SIZES) {
  for (const scene of SCENES) {
    if (scene === "url" && !env.BENCH_URL) continue;
    for (const engine of ENGINES) {
      state.results.push(await run(engine, scene, size));
      await writeFile(path.join(RESULTS, "results.json"), JSON.stringify(state, null, 2));
    }
  }
}

state.status = "done";
state.current = null;
await writeFile(path.join(RESULTS, "results.json"), JSON.stringify(state, null, 2));
log.info("benchmark done");
console.table(state.results.map(({ engine, scene, size, fps, motion: m, cpu_total, cpu_engine, cpu_node, pss_engine_mb, error }) => ({
  engine, scene, size, fps, even: m?.even, stood: m?.stood, skipped: m?.skipped, cpu_total, cpu_engine, cpu_node, pss_engine_mb, error,
})));
if (env.BENCH_EXIT === "1") process.exit(0);
