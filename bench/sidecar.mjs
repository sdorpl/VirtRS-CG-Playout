#!/usr/bin/env node
// Sidecar benchmark as a black box: one build (Node.js, or another one with
// VIRTRS_CG_BIN, e.g. the Rust binary) burns bench/scene.html in for one
// session, and the CPU and memory of the sidecar process itself are measured
// (Chromium is the same in every build). Modes (BENCH_MODES):
//   etv      ETV packets for the script element (the Python client),
//   ffmpeg   raw yuva420p frames for the ffmpeg overlay at BENCH_FFMPEG_FPS.
//
//   node bench/sidecar.mjs
//   VIRTRS_CG_BIN=rust/target/release/virtrs-cg node bench/sidecar.mjs

import { readFileSync } from "node:fs";
import http from "node:http";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { findBrowser, startSidecar } from "../test/sidecar.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const env = process.env;
const [WIDTH, HEIGHT] = (env.BENCH_SIZE || "1920x1080").split("x").map(Number);
const FPS = Number(env.BENCH_FPS || 25);
const FFMPEG_FPS = Number(env.BENCH_FFMPEG_FPS || 50);
const WARMUP = Number(env.BENCH_WARMUP || 10);
const DURATION = Number(env.BENCH_DURATION || 30);
const SCENE = env.BENCH_SCENE || "animated";
const MODES = (env.BENCH_MODES || "etv,ffmpeg").split(",").filter(Boolean);
const CLK_TCK = 100;

/** CPU seconds (user + system) and resident memory of one process. */
function usage(pid) {
  const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
  const fields = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
  const rss = Number(/VmRSS:\s+(\d+)/.exec(readFileSync(`/proc/${pid}/status`, "utf8"))[1]);
  return { cpu: (Number(fields[11]) + Number(fields[12])) / CLK_TCK, rss_mb: rss / 1024 };
}

// The scene and its headline events, as bench/bench.mjs serves them.
const server = http.createServer((req, res) => {
  if (req.url.startsWith("/scene.html")) {
    res.setHeader("Content-Type", "text/html; charset=utf-8");
    res.end(readFileSync(path.join(here, "scene.html")));
  } else if (req.url === "/events") {
    res.writeHead(200, { "Content-Type": "text/event-stream" });
    const send = () => res.write(`data: ${JSON.stringify({ title: "Wiadomości", sub: `Serwis o ${new Date().toLocaleTimeString("pl")}` })}\n\n`);
    send();
    const timer = setInterval(send, 3000);
    req.on("close", () => clearInterval(timer));
  } else {
    res.statusCode = 404;
    res.end();
  }
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const scene = `http://127.0.0.1:${server.address().port}/scene.html?scene=${SCENE}&fps=${FPS}`;

const browser = findBrowser();
if (!browser) throw new Error("no Chromium (set VIZRS_BROWSER)");
const data = { Resolution: { Width: WIDTH, Height: HEIGHT }, FrameRate: FPS, MediaItem_Path: "/m/bench.mp4" };

async function run(mode) {
  const cg = await startSidecar(
    { vizrs: { address: "127.0.0.1:9", token: "" }, output_url: scene, lead_seconds: 1 },
    { VIZRS_BROWSER: browser, VIZRS_BROWSER_NO_SANDBOX: "1" },
  );
  const script = await fetch(`${cg.url}/etv?channel=1`, { method: "POST", body: JSON.stringify(data) });
  const readers = [script.body.getReader()];
  let bytes = 0;
  // The overlay takes the session over; the script session stays open (and silent), as in ErsatzTV.
  if (mode === "ffmpeg") {
    const claim = await (await fetch(`${cg.url}/ffmpeg?channel=1&wait=5000`)).json();
    const frames = await fetch(`${cg.url}/ffmpeg/frames?session=${claim.session}&fps=${FFMPEG_FPS}`);
    readers.push(frames.body.getReader());
  }
  const drain = Promise.all(readers.map(async (reader) => {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.length;
    }
  })).catch(() => {});

  await sleep(WARMUP * 1000);
  const seq0 = (await cg.health()).captures[0]?.seq ?? 0;
  const [u0, b0, t0] = [usage(cg.pid), bytes, performance.now()];
  await sleep(DURATION * 1000);
  const seconds = (performance.now() - t0) / 1000;
  const [u1, seq1] = [usage(cg.pid), (await cg.health()).captures[0]?.seq ?? 0];
  for (const reader of readers) reader.cancel().catch(() => {});
  await drain;
  await cg.stop();

  return {
    build: env.VIRTRS_CG_BIN ? path.basename(env.VIRTRS_CG_BIN) : "node",
    mode,
    size: `${WIDTH}x${HEIGHT}`,
    scene: SCENE,
    cpu_percent: Math.round(((u1.cpu - u0.cpu) / seconds) * 1000) / 10,
    rss_mb: Math.round(u1.rss_mb),
    pictures_per_s: Math.round(((seq1 - seq0) / seconds) * 10) / 10,
    mb_per_s: Math.round(((bytes - b0) / seconds / 1e6) * 10) / 10,
  };
}

const results = [];
for (const mode of MODES) results.push(await run(mode));
server.close();
console.table(results);
console.log(JSON.stringify(results));
