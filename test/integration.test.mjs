// Sidecar end to end: fake VizRS (TCP), fake capture, the real Python client.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import net from "node:net";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { loadConfig } from "../src/config.js";
import { createServer } from "../src/server.js";
import { ChannelEvents } from "../src/vizrs.js";

import { readFrames } from "./helpers.mjs";

const CLIENT = fileURLToPath(new URL("../client/vizrs-etv-client.py", import.meta.url));
const quiet = { debug() {}, info() {}, warn() {}, error() {} };

/** VizRS stand-in: records commands, `STATE` returns `layers`. */
async function fakeVizrs(layers = []) {
  const received = [];
  const server = net.createServer((socket) => {
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
  return { received, address: `127.0.0.1:${server.address().port}`, close: () => server.close() };
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

test("client streams frames paced by the clock, commands reach VizRS once per item", async () => {
  const vizrs = await fakeVizrs();
  const capture = { frame: { seq: 0, rect: null, pixels: null }, key: "8x4", status: "fake" };
  const config = {
    ...loadConfig(null, {}),
    vizrs: { address: vizrs.address, token: "secret" },
    output_url: "http://fake/output",
    on_clip: "TAKE 4001 headline={title}",
    lead_seconds: 0.4,
  };
  const server = createServer(config, { log: quiet, captureFactory: () => capture });
  const port = await listen(server);
  const url = `http://127.0.0.1:${port}/etv?channel=1`;

  // Graphics appear 300 ms into the item.
  setTimeout(() => {
    capture.frame = { seq: 1, rect: { x: 2, y: 1, w: 2, h: 1 }, pixels: Buffer.alloc(8, 200) };
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

  server.close();
  vizrs.close();
});

test("client exits quietly when the sidecar is down", async () => {
  const stream = await runClient("http://127.0.0.1:9/etv", {}, 3000);
  assert.equal(stream.length, 0);
});

test("ads take graphics out; the first clip after the break takes them back", async () => {
  const vizrs = await fakeVizrs([{ layer: "main", page: "1001" }, { layer: "logo", page: "3001" }]);
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
  vizrs.close();
});

test("an item without the VizRS element is an ad; the next item with it restores", async () => {
  const vizrs = await fakeVizrs([{ layer: "main", page: "1001" }]);
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
  vizrs.close();
});
