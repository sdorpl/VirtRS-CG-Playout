// Real headless Chromium on a local transparent page; skipped without a browser.

import assert from "node:assert/strict";
import http from "node:http";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

import { Capture } from "../src/capture.js";

import { readFrames, yuvaSize } from "./helpers.mjs";
import { findBrowser, runClient, startSidecar } from "./sidecar.mjs";

const browser = findBrowser();
// Without the sandbox as root, and where the host forbids it (CI runners, containers).
const noSandbox = process.getuid?.() === 0 || process.env.VIZRS_BROWSER_NO_SANDBOX === "1";
const quiet = { debug() {}, info() {}, warn: (m) => process.env.TRACE && console.error(m), error: (m) => process.env.TRACE && console.error(m) };

const PAGE = `<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:40px;top:300px;width:400px;height:60px;background:rgb(255,0,0)"></div>
</body></html>`;

test("Chromium capture gives the visible box of a transparent page", { skip: !browser && "no Chromium", timeout: 90000 }, async () => {
  const server = http.createServer((_, res) => res.end(PAGE));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  const capture = new Capture({
    url: `http://127.0.0.1:${server.address().port}/`,
    width: 640,
    height: 360,
    fps: 25,
    browser,
    noSandbox,
    log: quiet,
  }).start();

  try {
    // A cold browser may first send a half-painted frame; the picture must settle on the page.
    const expected = { x: 40, y: 300, w: 400, h: 60 };
    const boxes = () => capture.frame.rects.map(({ pixels, ...rect }) => rect);
    const deadline = Date.now() + 60000;
    while (JSON.stringify(boxes()) !== JSON.stringify([expected]) && Date.now() < deadline) await sleep(100);

    assert.deepEqual(boxes(), [expected]);
    const [r, g, b, a] = capture.frame.rects[0].pixels.subarray(0, 4);
    assert.deepEqual([r, g, b, a], [255, 0, 0, 255]);
  } finally {
    capture.stop();
    server.close();
  }
});

test("Chromium capture pauses without sessions and resumes with a fresh frame", { skip: !browser && "no Chromium", timeout: 90000 }, async () => {
  const server = http.createServer((_, res) => res.end(PAGE));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  const capture = new Capture({
    url: `http://127.0.0.1:${server.address().port}/`,
    width: 640,
    height: 360,
    fps: 25,
    browser,
    noSandbox,
    idleMs: 200,
    log: quiet,
  }).start();
  const until = async (check) => {
    const deadline = Date.now() + 60000;
    while (!check() && Date.now() < deadline) await sleep(50);
    assert.ok(check());
  };

  try {
    capture.acquire();
    await until(() => capture.frame.rects.length);
    capture.release();
    await until(() => capture.status === "paused");

    // A static page still sends a frame when the screencast starts again.
    const seq = capture.frame.seq;
    capture.acquire();
    assert.equal(capture.status, "capturing");
    await until(() => capture.frame.seq > seq);
    assert.equal(capture.frame.rects.length, 1);
  } finally {
    capture.stop();
    server.close();
  }
});

test("Chromium capture renders smaller and scales to the channel", { skip: !browser && "no Chromium", timeout: 90000 }, async () => {
  // Proportional layout, like VizRS scaling its 1920x1080 stage to the viewport.
  const page = `<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:10%;top:80%;width:50%;height:10%;background:rgb(255,0,0)"></div></body></html>`;
  const server = http.createServer((_, res) => res.end(page));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));

  const capture = new Capture({
    url: `http://127.0.0.1:${server.address().port}/`,
    width: 320,
    height: 180,
    outWidth: 640,
    outHeight: 360,
    fps: 25,
    browser,
    noSandbox,
    log: quiet,
  }).start();

  try {
    const deadline = Date.now() + 60000;
    while (!capture.frame.rects.length && Date.now() < deadline) await sleep(100);

    // Red box 32,144 160x18 at 320x180, plus 4 px margin, doubled.
    const [{ pixels, ...rect }] = capture.frame.rects;
    assert.deepEqual(rect, { x: 56, y: 280, w: 336, h: 52 });
    const middle = ((rect.h >> 1) * rect.w + (rect.w >> 1)) * 4;
    assert.deepEqual([...pixels.subarray(middle, middle + 4)], [255, 0, 0, 255]);
    assert.equal(capture.key, "640x360 (render 320x180)");
  } finally {
    capture.stop();
    server.close();
  }
});

test("sidecar burns the page in: ETV packets and yuva420p frames for ffmpeg", { skip: !browser && "no Chromium", timeout: 120000 }, async (t) => {
  const page = `<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:40px;top:100px;width:120px;height:20px;background:rgb(255,0,0)"></div></body></html>`;
  const server = http.createServer((_, res) => res.end(page));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());

  const cg = await startSidecar(
    { vizrs: { address: "127.0.0.1:9", token: "" }, output_url: `http://127.0.0.1:${server.address().port}/`, lead_seconds: 0 },
    { VIZRS_BROWSER: browser, ...(noSandbox ? { VIZRS_BROWSER_NO_SANDBOX: "1" } : {}) },
  );
  t.after(cg.stop);
  const [width, height] = [320, 180];
  const data = {
    Resolution: { Width: width, Height: height },
    FrameRate: 25,
    MediaItem_Path: "/m/News.mp4",
    MediaItem_Start: "2026-10-24T20:00:00+02:00",
    MediaItem_Stop: "2026-10-24T20:30:00+02:00",
  };

  // ffmpeg overlay path: take the session of channel 1 over, read raw frames
  // until a cold browser has drawn the page.
  const script = runClient(`${cg.url}/etv?channel=1`, data, 90000);
  const claim = await fetch(`${cg.url}/ffmpeg?channel=1&wait=5000`);
  assert.equal(claim.status, 200);
  const { session, width: w, height: h } = await claim.json();
  assert.deepEqual([w, h], [width, height]);

  const size = yuvaSize(width, height);
  const center = 110 * width + 100;
  const alpha = size - width * height;
  const abort = new AbortController();
  const frames = await fetch(`${cg.url}/ffmpeg/frames?session=${session}`, { signal: abort.signal });
  let buffer = Buffer.alloc(0);
  let drawn = null;
  const deadline = Date.now() + 60000;
  for await (const chunk of frames.body) {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= size) {
      const frame = buffer.subarray(0, size);
      buffer = buffer.subarray(size);
      if (frame[alpha + center] === 255) drawn = frame;
    }
    if (drawn || Date.now() > deadline) break;
  }
  abort.abort();
  assert.ok(drawn, "the page reaches the raw frames");
  // Opaque red, BT.601 below 720 lines; outside the box transparent.
  assert.equal(drawn[center], 81);
  assert.equal(drawn[alpha], 0);
  await script;

  // Script element path on another channel: the same capture, as ETV packets.
  const stream = await runClient(`${cg.url}/etv?channel=2`, data, 1500);
  const last = readFrames(stream, width, height).at(-1);
  assert.deepEqual([...last.subarray(center * 4, center * 4 + 4)], [255, 0, 0, 255]);
  assert.equal(last[3], 0);
});
