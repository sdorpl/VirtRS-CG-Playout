#!/usr/bin/env node
// Engine benchmark: the same page rendered headless by Chromium (captured
// exactly as the sidecar does, src/capture.js) and by WPE WebKit (GStreamer
// `wpevideosrc`, raw RGBA, no PNG). Each run measures CPU and memory of the
// engine and of this process (decoding, cropping), and the rate of new
// pictures. The report is served at http://<host>:PORT/.

import { execFile, spawn } from "node:child_process";
import { mkdir, readFile, readdir, writeFile } from "node:fs/promises";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { setTimeout as sleep } from "node:timers/promises";

import sharp from "sharp";

import { Capture, findBrowser } from "../src/capture.js";
import { crop, visibleRects } from "../src/packets.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const env = process.env;
const [WIDTH, HEIGHT] = (env.BENCH_SIZE || "1920x1080").split("x").map(Number);
const FPS = Number(env.BENCH_FPS || 25);
const WARMUP = Number(env.BENCH_WARMUP || 10);
const DURATION = Number(env.BENCH_DURATION || 60);
const PORT = Number(env.PORT || 7790);
const RESULTS = env.BENCH_RESULTS || path.join(here, "results");
const ENGINES = (env.BENCH_ENGINES || "chromium,wpe").split(",").map((s) => s.trim()).filter(Boolean);
const SCENES = (env.BENCH_SCENES || (env.BENCH_URL ? "static,animated,url" : "static,animated")).split(",").map((s) => s.trim()).filter(Boolean);
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

/** The process `root` and all its descendants with their CPU ticks. */
async function tree(root) {
  const all = await children();
  const pids = new Set([root]);
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

// --- engines ---

class ChromiumEngine {
  constructor(url) {
    const engine = this;
    this.name = "chromium";
    this.frames = [];
    this.lastPng = null;
    // The sidecar's own capture: screencast every n-th frame, sharp decode, crop.
    this.capture = new (class extends Capture {
      setFrame(rects) {
        super.setFrame(rects);
        if (rects.length) engine.frames.push(performance.now());
      }
      async applyPng(png) {
        engine.lastPng = png;
        return super.applyPng(png);
      }
    })({ url, width: WIDTH, height: HEIGHT, fps: FPS, browser: findBrowser(), noSandbox: true, log: quiet });
  }

  start() {
    if (!this.capture.browser) throw new Error("Chromium not found (VIZRS_BROWSER)");
    this.capture.start();
  }

  get pid() {
    return this.capture.child?.pid;
  }

  async sample(file) {
    if (this.lastPng) await writeFile(file, this.lastPng);
  }

  stop() {
    this.capture.stop();
  }
}

class WpeEngine {
  constructor(url) {
    this.name = "wpe";
    this.url = url;
    this.frames = [];
    this.last = null;
    this.size = WIDTH * HEIGHT * 4;
  }

  start() {
    const args = [
      "-q", "wpevideosrc", `location=${this.url}`, "draw-background=false",
      "!", `video/x-raw,format=BGRA,width=${WIDTH},height=${HEIGHT},framerate=${FPS}/1`,
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
          // Raw frames come at the channel rate; only changed ones are cropped, as with Chromium.
          if (!this.last || !buffer.equals(this.last)) {
            const rects = visibleRects(buffer, WIDTH, HEIGHT);
            rects.map((rect) => crop(buffer, WIDTH, rect));
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

  get pid() {
    return this.child?.pid;
  }

  async sample(file) {
    if (this.last) await sharp(this.last, { raw: { width: WIDTH, height: HEIGHT, channels: 4 } }).png().toFile(file);
  }

  stop() {
    this.child?.kill("SIGKILL");
  }
}

// --- one run ---

function percentile(values, p) {
  if (!values.length) return 0;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(p * sorted.length))];
}

async function run(engineName, scene) {
  const url = scene === "url" ? env.BENCH_URL : `http://127.0.0.1:${PORT}/scene.html?scene=${scene}&fps=${FPS}`;
  const engine = engineName === "wpe" ? new WpeEngine(url) : new ChromiumEngine(url);
  const result = { engine: engineName, scene, url, ok: false };
  state.current = `${engineName} / ${scene}`;
  log.info(`run ${state.current}: ${url}`);

  try {
    const launched = performance.now();
    engine.start();
    while (!engine.frames.length) {
      if (performance.now() - launched > 60000) throw new Error("no picture within 60 s");
      await sleep(200);
    }
    result.startup_s = Number(((performance.now() - launched) / 1000).toFixed(1));
    state.current += " (warm-up)";
    await sleep(WARMUP * 1000);
    state.current = `${engineName} / ${scene} (measuring ${DURATION} s)`;

    const pid = engine.pid;
    let before = await tree(pid);
    let engineTicks = 0;
    const cpu0 = process.cpuUsage();
    const t0 = performance.now();
    const frames0 = engine.frames.length;
    const pssSamples = [];
    let rssMax = 0;

    while (performance.now() - t0 < DURATION * 1000) {
      await sleep(1000);
      const after = await tree(pid);
      for (const [p, ticks] of after) engineTicks += ticks - (before.get(p) ?? 0);
      before = after;
      pssSamples.push(await pss(after.keys()));
      rssMax = Math.max(rssMax, process.memoryUsage().rss / 1048576);
    }

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
      cpu_engine: Math.round(((engineTicks / CLK_TCK) / seconds) * 100),
      cpu_node: Math.round(((cpu.user + cpu.system) / 1e6 / seconds) * 100),
      pss_engine_mb: Math.round(Math.max(...pssSamples)),
      rss_node_mb: Math.round(rssMax),
    });
    result.cpu_total = result.cpu_engine + result.cpu_node;
    const file = `${engineName}-${scene}.png`;
    await engine.sample(path.join(RESULTS, file));
    result.frame = `frames/${file}`;
    log.info(`  fps ${result.fps}, CPU ${result.cpu_engine}% + ${result.cpu_node}% (node), PSS ${result.pss_engine_mb} MB`);
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
    chromium: await version(findBrowser() ?? "chromium-headless-shell", ["--version"]),
    wpe: await version("dpkg-query", ["-W", "-f=${Version}", "libwpewebkit-2.0-1"]),
    gstreamer: await version("gst-launch-1.0", ["--version"]),
    size: `${WIDTH}x${HEIGHT}`,
    fps: FPS,
    warmup_s: WARMUP,
    duration_s: DURATION,
  };
}

// --- report ---

const esc = (value) => String(value ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

function report() {
  const running = state.status !== "done";
  const rows = state.results.map((r) => r.ok
    ? `<tr><td>${esc(r.engine)}</td><td>${esc(r.scene)}</td><td>${r.fps}</td><td>${r.gap_p95_ms} / ${r.gap_max_ms}</td><td><b>${r.cpu_total}%</b></td><td>${r.cpu_engine}%</td><td>${r.cpu_node}%</td><td>${r.pss_engine_mb}</td><td>${r.rss_node_mb}</td><td>${r.processes}</td></tr>`
    : `<tr><td>${esc(r.engine)}</td><td>${esc(r.scene)}</td><td colspan="8" class="err">błąd: ${esc(r.error)}</td></tr>`).join("");
  const scenes = [...new Set(state.results.map((r) => r.scene))];
  const frames = scenes.map((scene) => `<h3>${esc(scene)}</h3><div class="frames">${state.results
    .filter((r) => r.scene === scene && r.frame)
    .map((r) => `<figure><img src="${esc(r.frame)}?${Date.now()}" alt=""><figcaption>${esc(r.engine)}</figcaption></figure>`).join("")}</div>`).join("");
  const sys = Object.entries(state.system).map(([k, v]) => `<tr><th>${esc(k)}</th><td>${esc(v)}</td></tr>`).join("");
  return `<!doctype html><html lang="pl"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
${running ? '<meta http-equiv="refresh" content="5">' : ""}<title>Benchmark silników</title><style>
:root{--bg:#fff;--fg:#111;--mut:#666;--line:#ddd;--err:#b91c1c}
@media (prefers-color-scheme:dark){:root{--bg:#111;--fg:#eee;--mut:#999;--line:#333;--err:#f87171}}
body{background:var(--bg);color:var(--fg);font:15px/1.5 system-ui,sans-serif;margin:0 auto;max-width:1100px;padding:16px}
table{border-collapse:collapse;width:100%;margin:8px 0 24px;overflow-x:auto;display:block}th,td{border-bottom:1px solid var(--line);padding:6px 10px;text-align:left;white-space:nowrap}
.mut{color:var(--mut)}.err{color:var(--err)}.frames{display:flex;gap:12px;flex-wrap:wrap}figure{margin:0;flex:1 1 320px}
img{width:100%;background:repeating-conic-gradient(#888 0 25%,#bbb 0 50%) 0 0/20px 20px;border:1px solid var(--line)}figcaption{color:var(--mut)}
</style></head><body><h1>Benchmark silników: Chromium a WPE WebKit</h1>
<p class="mut">Stan: ${esc(running ? `w toku – ${state.current ?? state.status}` : "zakończony")} · start ${esc(state.started)}</p>
<table><thead><tr><th>silnik</th><th>scena</th><th>nowe klatki/s</th><th>przerwa p95 / max [ms]</th><th>CPU razem</th><th>CPU silnik</th><th>CPU node</th><th>PSS silnika [MB]</th><th>RSS node [MB]</th><th>procesy</th></tr></thead><tbody>${rows}</tbody></table>
<p class="mut">CPU w procentach jednego rdzenia (100% = cały rdzeń). „CPU node” to dekodowanie PNG (Chromium) albo odbiór surowych klatek (WPE) i przycinanie – ta sama praca co w sidecarze. Nowe klatki/s: tylko zmienione obrazy z widoczną grafiką.</p>
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

for (const scene of SCENES) {
  if (scene === "url" && !env.BENCH_URL) continue;
  for (const engine of ENGINES) {
    state.results.push(await run(engine, scene));
    await writeFile(path.join(RESULTS, "results.json"), JSON.stringify(state, null, 2));
  }
}

state.status = "done";
state.current = null;
await writeFile(path.join(RESULTS, "results.json"), JSON.stringify(state, null, 2));
log.info("benchmark done");
console.table(state.results.map(({ engine, scene, fps, cpu_total, cpu_engine, cpu_node, pss_engine_mb, error }) => ({ engine, scene, fps, cpu_total, cpu_engine, cpu_node, pss_engine_mb, error })));
if (env.BENCH_EXIT === "1") process.exit(0);
