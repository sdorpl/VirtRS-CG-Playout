// Sidecar end to end: fake VizRS (TCP), fake capture, the real Python client.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { chmodSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { loadConfig } from "../src/config.js";
import { createServer } from "../src/server.js";
import { ChannelEvents } from "../src/vizrs.js";
import { yuvaFrame, yuvaSize } from "../src/yuva.js";

import { readFrames } from "./helpers.mjs";

const CLIENT = fileURLToPath(new URL("../client/vizrs-etv-client.py", import.meta.url));
const WRAPPER = fileURLToPath(new URL("../client/vizrs-ffmpeg.py", import.meta.url));
const quiet = { debug() {}, info() {}, warn() {}, error() {} };

/** VizRS stand-in: records commands, `STATE` returns `layers`. */
async function fakeVizrs(layers = []) {
  const received = [];
  const sockets = new Set();
  const server = net.createServer((socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    let buffer = "";
    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      buffer += chunk;
      let index;
      while ((index = buffer.indexOf("\n")) >= 0) {
        const line = buffer.slice(0, index);
        buffer = buffer.slice(index + 1);
        received.push(line);
        socket.write(line === "STATE" ? `OK ${JSON.stringify({ layers })}\n` : "OK {}\n");
      }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const close = () => {
    for (const socket of sockets) socket.destroy();
    server.close();
  };
  return { received, address: `127.0.0.1:${server.address().port}`, close };
}

function listen(server) {
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server.address().port)));
}

/** Runs the client like ErsatzTV does, for `ms`, and returns its stdout. */
function runClient(url, data, ms) {
  return new Promise((resolve, reject) => {
    const child = spawn("python3", ["-I", CLIENT, url]);
    const out = [];
    child.stdout.on("data", (chunk) => out.push(chunk));
    child.on("error", reject);
    child.on("exit", () => resolve(Buffer.concat(out)));
    child.stdin.end(JSON.stringify(data));
    setTimeout(() => child.kill("SIGTERM"), ms);
  });
}

const item = (path, start, extra = {}) => ({
  Resolution: { Width: 8, Height: 4 },
  FrameRate: 25,
  MediaItem_Path: path,
  MediaItem_Title: path.split("/").pop().replace(".mp4", ""),
  MediaItem_Start: start,
  MediaItem_Stop: start.replace(":00:00", ":30:00"),
  ...extra,
});

test("client streams frames paced by the clock, commands reach VizRS once per item", async (t) => {
  const vizrs = await fakeVizrs();
  t.after(vizrs.close);
  const capture = {
    frame: { seq: 0, rects: [] }, key: "8x4", status: "fake", users: 0,
    acquire() { this.users += 1; },
    release() { this.users -= 1; },
  };
  const config = {
    ...loadConfig(null, {}),
    vizrs: { address: vizrs.address, token: "secret" },
    output_url: "http://fake/output",
    on_clip: "TAKE 4001 headline={title}",
    lead_seconds: 0.4,
  };
  const server = createServer(config, { log: quiet, captureFactory: () => capture });
  const port = await listen(server);
  t.after(() => server.close());
  const url = `http://127.0.0.1:${port}/etv?channel=1`;

  // Graphics appear 300 ms into the item.
  setTimeout(() => {
    capture.frame = { seq: 1, rects: [{ x: 2, y: 1, w: 2, h: 1, pixels: Buffer.alloc(8, 200) }] };
  }, 300);
  const stream = await runClient(url, item("/m/News.mp4", "2026-10-24T20:00:00+02:00"), 1000);
  const frames = readFrames(stream, 8, 4);

  // ~1 s at 25 fps plus 0.4 s lead; the process start eats a little.
  assert.ok(frames.length >= 25 && frames.length <= 36, `frames: ${frames.length}`);
  assert.ok(frames[0].every((b) => b === 0), "starts transparent");
  assert.equal(frames.at(-1)[(1 * 8 + 2) * 4], 200, "ends with the graphics");

  // A second session on the same item (viewer joins, seek) sends nothing again.
  await runClient(url, item("/m/News.mp4", "2026-10-24T20:00:00+02:00", { MediaItem_StreamSeek: "00:05:00" }), 300);
  await sleep(100);
  assert.deepEqual(vizrs.received, ["AUTH secret", "TAKE 4001 headline=News"]);
  assert.equal(capture.users, 0, "every session released the capture");

});

test("render size: config default, per-channel override, one capture per size pair", async (t) => {
  const vizrs = await fakeVizrs();
  t.after(vizrs.close);
  const made = [];
  const config = {
    ...loadConfig(null, { VIZRS_RENDER_SIZE: "720p" }),
    vizrs: { address: vizrs.address, token: "" },
    output_url: "http://fake/output",
    lead_seconds: 0,
  };
  const server = createServer(config, {
    log: quiet,
    captureFactory: (size) => {
      made.push(size);
      return { frame: { seq: 0, rects: [] }, key: "fake", status: "fake" };
    },
  });
  const port = await listen(server);
  t.after(() => server.close());
  const hd = { ...item("/m/News.mp4", "2026-10-24T20:00:00+02:00"), Resolution: { Width: 1920, Height: 1080 } };

  await runClient(`http://127.0.0.1:${port}/etv?channel=1`, hd, 400);
  await runClient(`http://127.0.0.1:${port}/etv?channel=2`, hd, 400);
  await runClient(`http://127.0.0.1:${port}/etv?channel=3&render=auto`, hd, 400);
  await runClient(`http://127.0.0.1:${port}/etv?channel=4&render=bogus`, hd, 400);

  assert.deepEqual(made.map(({ width, height, outWidth, outHeight }) => [width, height, outWidth, outHeight]), [
    [1280, 720, 1920, 1080],
    [1920, 1080, 1920, 1080],
  ]);
});

test("client exits quietly when the sidecar is down", async () => {
  const stream = await runClient("http://127.0.0.1:9/etv", {}, 3000);
  assert.equal(stream.length, 0);
});

test("ads take graphics out; the first clip after the break takes them back", async (t) => {
  const vizrs = await fakeVizrs([{ layer: "main", page: "1001" }, { layer: "logo", page: "3001" }]);
  t.after(vizrs.close);
  const events = new ChannelEvents(
    {
      vizrs: { address: vizrs.address, token: "" },
      on_clip: "UPDATE 2001 t={title}",
      on_ad: "",
      ad_match: { path: "/reklamy/" },
    },
    quiet,
  );

  const ctx = (source, start) => ({ source, start, title: "T", next_start_full: "", duration: 0, next_duration: 0 });
  await events.clip(ctx("/reklamy/a.mp4", "1"));
  await events.clip(ctx("/reklamy/b.mp4", "2"));
  await events.clip(ctx("/m/film.mp4", "3"));

  assert.deepEqual(vizrs.received, [
    "STATE", "OUT main,logo",
    "STATE", "OUT main,logo",
    "TAKE 1001,3001", "UPDATE 2001 t=T",
  ]);
});

test("an item without the VizRS element is an ad; the next item with it restores", async (t) => {
  const vizrs = await fakeVizrs([{ layer: "main", page: "1001" }]);
  t.after(vizrs.close);
  const events = new ChannelEvents(
    {
      vizrs: { address: vizrs.address, token: "" },
      on_clip: "UPDATE 2001 t={title}",
      on_ad: "",
      ad_match: {},
      ad_without_element: true,
      gap_grace_ms: 50,
    },
    quiet,
  );

  const ctx = (source, start, stop) => ({ source, start, stop, title: "T", next_start_full: "", duration: 0, next_duration: 0 });
  const now = Date.now();
  // Back-to-back items with the element: no ad in between.
  const endA = events.session(ctx("/m/a.mp4", "1", new Date(now).toISOString()));
  const endB = events.session(ctx("/m/b.mp4", "2", new Date(now + 100).toISOString()));
  endA();
  endB();
  await sleep(300);
  await events.queue;
  assert.deepEqual(vizrs.received, ["UPDATE 2001 t=T", "UPDATE 2001 t=T", "STATE", "OUT main"]);

  events.session(ctx("/m/c.mp4", "3", new Date(now + 10000).toISOString()));
  await events.queue;
  assert.deepEqual(vizrs.received.slice(4), ["TAKE 1001", "UPDATE 2001 t=T"]);
});

// ffmpeg stand-in: prints its arguments and, for a `pipe:N` input, how many
// bytes it read (FRAMES frames of FRAME_BYTES) and the first frame in hex.
const FAKE_FFMPEG = `#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
result = {"args": args}
for arg in args:
    if arg.startswith("pipe:") and int(arg[5:]) > 2:
        fd, want, data = int(arg[5:]), int(os.environ["FRAME_BYTES"]) * int(os.environ["FRAMES"]), b""
        while len(data) < want:
            chunk = os.read(fd, want - len(data))
            if not chunk:
                break
            data += chunk
        result["read"] = len(data)
        result["first"] = data[: int(os.environ["FRAME_BYTES"])].hex()
print(json.dumps(result), flush=True)
`;

/** Runs the wrapper like ErsatzTV runs ffmpeg; resolves with the stand-in's report. */
function runWrapper(args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn("python3", ["-I", WRAPPER, ...args], { env: { ...process.env, ...env } });
    const out = [];
    const err = [];
    child.stdout.on("data", (chunk) => out.push(chunk));
    child.stderr.on("data", (chunk) => err.push(chunk));
    child.on("error", reject);
    child.on("exit", (code) => {
      try {
        resolve({ code, stderr: Buffer.concat(err).toString(), ...JSON.parse(Buffer.concat(out).toString()) });
      } catch (error) {
        reject(new Error(`${error.message}: ${Buffer.concat(err)}`));
      }
    });
  });
}

test("ffmpeg overlay: raw frames from the sidecar, the script session stops its packets", async (t) => {
  const vizrs = await fakeVizrs();
  t.after(vizrs.close);
  const rects = [{ x: 2, y: 1, w: 2, h: 1, pixels: Buffer.from([255, 255, 255, 255, 255, 0, 0, 128]) }];
  const capture = { frame: { seq: 1, rects }, key: "8x4", status: "fake", acquire() {}, release() {} };
  const config = {
    ...loadConfig(null, {}),
    vizrs: { address: vizrs.address, token: "" },
    output_url: "http://fake/output",
    on_clip: "TAKE 4001 headline={title}",
    lead_seconds: 0.4,
  };
  const server = createServer(config, { log: quiet, captureFactory: () => capture });
  const port = await listen(server);
  t.after(() => server.close());

  // ErsatzTV stand-in: the canvas request starts the script element with the item's data.
  const canvas = { requests: [], packetBytes: 0 };
  const etv = http.createServer((request, response) => {
    canvas.requests.push(request.headers);
    const child = spawn("python3", ["-I", CLIENT, `http://127.0.0.1:${port}/etv?channel=1`]);
    child.stdout.on("data", (chunk) => { canvas.packetBytes += chunk.length; });
    child.stdin.end(JSON.stringify(item("/m/News.mp4", "2026-10-24T20:00:00+02:00")));
    response.writeHead(200);
    response.flushHeaders();
    response.on("close", () => child.kill());
  });
  const etvPort = await listen(etv);
  t.after(() => etv.close());

  const dir = mkdtempSync(path.join(os.tmpdir(), "virtrs-ffmpeg-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const ffmpeg = path.join(dir, "ffmpeg");
  writeFileSync(ffmpeg, FAKE_FFMPEG);
  chmodSync(ffmpeg, 0o755);

  const headers = "x-etv-channel:1\r\nx-etv-offset-ms:0\r\nx-etv-duration-ms:44000\r\nx-etv-frame-rate:25/1\r\n";
  const args = [
    "-threads", "1", "-i", "/m/News.mp4",
    "-protocol_whitelist", "file,http", "-headers", headers, "-t", "44000ms",
    "-i", `http://127.0.0.1:${etvPort}/internal/graphics/1/28750?exp=1&sig=x`,
    "-filter_complex", "[1:0]format=yuva420p[g]", "-f", "null", "-",
  ];
  const env = { VIZRS_FFMPEG: ffmpeg, VIZRS_CG_URL: `http://127.0.0.1:${port}`, FRAME_BYTES: String(yuvaSize(8, 4)), FRAMES: "3" };

  const first = await runWrapper(args, env);
  const pipe = first.args.find((arg) => arg.startsWith("pipe:"));
  assert.deepEqual(first.args, [
    "-threads", "1", "-i", "/m/News.mp4",
    "-t", "44000ms", "-f", "rawvideo", "-pix_fmt", "yuva420p", "-video_size", "8x4", "-framerate", "25/1", "-i", pipe,
    "-filter_complex", "[1:0]format=yuva420p[g]", "-f", "null", "-",
  ]);
  assert.equal(first.read, 3 * yuvaSize(8, 4));
  assert.equal(first.first, yuvaFrame(rects, 8, 4).toString("hex"));
  assert.equal(canvas.requests[0]["x-etv-frame-rate"], "25/1", "the canvas is opened with ErsatzTV's headers");

  // The channel now runs through the overlay: its next script session sends ErsatzTV nothing.
  await sleep(300);
  const before = canvas.packetBytes;
  const second = await runWrapper(args, env);
  assert.equal(second.read, 3 * yuvaSize(8, 4));
  await sleep(300);
  assert.equal(canvas.packetBytes, before);
  assert.deepEqual(vizrs.received, ["TAKE 4001 headline=News"], "commands once, from the script session");

  // Sidecar down: the original command, unchanged.
  const fallback = await runWrapper(args, { ...env, VIZRS_CG_URL: "http://127.0.0.1:9" });
  assert.deepEqual(fallback.args, args);
  assert.match(fallback.stderr, /ErsatzTV graphics instead/);

  // Calls without the canvas go straight through.
  assert.deepEqual((await runWrapper(["-version"], env)).args, ["-version"]);
});

test("ffmpeg overlay at a forced rate: fps before the overlay, -r on the HLS output", async (t) => {
  const vizrs = await fakeVizrs();
  t.after(vizrs.close);
  const capture = { frame: { seq: 1, rects: [] }, fps: 25, acquire() {}, release() {}, raiseFps(fps) { this.fps = Math.max(this.fps, fps); } };
  const config = { ...loadConfig(null, {}), vizrs: { address: vizrs.address, token: "" }, output_url: "http://fake/output", lead_seconds: 0.4 };
  const server = createServer(config, { log: quiet, captureFactory: () => capture });
  const port = await listen(server);
  t.after(() => server.close());
  const etv = http.createServer((request, response) => {
    const child = spawn("python3", ["-I", CLIENT, `http://127.0.0.1:${port}/etv?channel=1`]);
    child.stdout.resume();
    child.stdin.end(JSON.stringify(item("/m/News.mp4", "2026-10-24T20:00:00+02:00")));
    response.writeHead(200);
    response.flushHeaders();
    response.on("close", () => child.kill());
  });
  const etvPort = await listen(etv);
  t.after(() => etv.close());
  const dir = mkdtempSync(path.join(os.tmpdir(), "virtrs-ffmpeg-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  const ffmpeg = path.join(dir, "ffmpeg");
  writeFileSync(ffmpeg, FAKE_FFMPEG);
  chmodSync(ffmpeg, 0o755);
  const env = {
    VIZRS_FFMPEG: ffmpeg, VIZRS_CG_URL: `http://127.0.0.1:${port}`, VIZRS_FFMPEG_FPS: "50",
    FRAME_BYTES: String(yuvaSize(8, 4)), FRAMES: "2",
  };
  const hls = ["-g", "50", "-f", "hls", "-hls_time", "4", "/t/ffmpeg.m3u8"];
  const filter = "[0:0]scale=8:4[v_m0];[1:0]format=yuva420p[v_s0];[v_m0][v_s0]overlay=x=0:y=0[v_o0]";

  const overlay = await runWrapper([
    "-i", "/m/News.mp4", "-headers", "x-etv-frame-rate:25/1\r\n",
    "-i", `http://127.0.0.1:${etvPort}/internal/graphics/1/7?exp=1&sig=x`,
    "-filter_complex", filter, "-map", "[v_o0]", ...hls,
  ], env);
  const pipe = overlay.args.find((arg) => arg.startsWith("pipe:"));
  assert.deepEqual(overlay.args, [
    "-i", "/m/News.mp4",
    "-f", "rawvideo", "-pix_fmt", "yuva420p", "-video_size", "8x4", "-framerate", "50", "-i", pipe,
    "-filter_complex", "[0:0]scale=8:4[v_m0];[1:0]format=yuva420p[v_s0];[v_m0]fps=50[v_m0_fps];[v_m0_fps][v_s0]overlay=x=0:y=0[v_o0]",
    "-map", "[v_o0]", "-g", "50", "-r", "50", "-f", "hls", "-hls_time", "4", "/t/ffmpeg.m3u8",
  ]);
  assert.equal(overlay.read, 2 * yuvaSize(8, 4));
  assert.equal(capture.fps, 50, "the capture follows the overlay's rate");

  // Items without the canvas: only the output rate.
  assert.deepEqual((await runWrapper(["-i", "/m/Ad.mp4", ...hls], env)).args, ["-i", "/m/Ad.mp4", "-g", "50", "-r", "50", "-f", "hls", "-hls_time", "4", "/t/ffmpeg.m3u8"]);
  // Not a transcode to HLS (the canvas encoder, copies): unchanged.
  const canvasEncoder = ["-f", "rawvideo", "-i", "pipe:0", "-c:v", "ffv1", "-f", "nut", "pipe:1"];
  assert.deepEqual((await runWrapper(canvasEncoder, env)).args, canvasEncoder);
  const copy = ["-i", "/m/News.mp4", "-c", "copy", ...hls];
  assert.deepEqual((await runWrapper(copy, env)).args, copy);
});
