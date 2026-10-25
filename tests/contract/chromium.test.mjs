// The sidecar burns a local transparent page in with a real headless Chromium
// (ETV packets and yuva420p frames); skipped without a browser.

import assert from "node:assert/strict";
import http from "node:http";
import { test } from "node:test";

import { readFrames, yuvaSize } from "./helpers.mjs";
import { findBrowser, runClient, startSidecar } from "./sidecar.mjs";

const browser = findBrowser();
// Without the sandbox as root, and where the host forbids it (CI runners, containers).
const noSandbox = process.getuid?.() === 0 || process.env.VIZRS_BROWSER_NO_SANDBOX === "1";
for (const mode of ["screencast", "frames"]) {
  // The frames mode needs chromium-headless-shell; another browser falls back to the screencast.
  test(`sidecar burns the page in (${mode}): ETV packets and yuva420p frames for ffmpeg`, { skip: !browser && "no Chromium", timeout: 120000 }, (t) => burnIn(t, mode));
}

async function burnIn(t, mode) {
  const page = `<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:40px;top:100px;width:120px;height:20px;background:rgb(255,0,0)"></div></body></html>`;
  const server = http.createServer((_, res) => res.end(page));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());

  const cg = await startSidecar(
    { vizrs: { address: "127.0.0.1:9", token: "" }, output_url: `http://127.0.0.1:${server.address().port}/`, lead_seconds: 0, capture_mode: mode },
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
}

test("graphics area: Chromium draws only the area, ffmpeg gets frames of its size", { skip: !browser && "no Chromium", timeout: 120000 }, async (t) => {
  const page = `<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:40px;top:100px;width:120px;height:20px;background:rgb(255,0,0)"></div>
<div style="position:absolute;left:40px;top:10px;width:120px;height:20px;background:rgb(0,0,255)"></div></body></html>`;
  const server = http.createServer((_, res) => res.end(page));
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  t.after(() => server.close());
  const cg = await startSidecar(
    { vizrs: { address: "127.0.0.1:9", token: "" }, output_url: `http://127.0.0.1:${server.address().port}/`, lead_seconds: 0, area: "200x40+20+90" },
    { VIZRS_BROWSER: browser, ...(noSandbox ? { VIZRS_BROWSER_NO_SANDBOX: "1" } : {}) },
  );
  t.after(cg.stop);
  const [width, height] = [320, 180];
  const data = { Resolution: { Width: width, Height: height }, FrameRate: 25, MediaItem_Path: "/m/News.mp4" };

  const script = runClient(`${cg.url}/etv?channel=1`, data, 90000);
  const { session, width: w, height: h, x, y } = await (await fetch(`${cg.url}/ffmpeg?channel=1&wait=5000&area=1`)).json();
  assert.deepEqual([w, h, x, y], [200, 40, 20, 90]);
  const size = yuvaSize(w, h);
  // The red box at 100,110 of the channel is 80,20 of the area.
  const center = 20 * w + 80;
  const alpha = size - w * h;
  const abort = new AbortController();
  const frames = await fetch(`${cg.url}/ffmpeg/frames?session=${session}&area=1`, { signal: abort.signal });
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
  assert.ok(drawn, "the area reaches the raw frames");
  assert.equal(drawn[center], 81);
  assert.equal(drawn[alpha], 0);
  await script;

  // ETV packets of the same capture: channel coordinates, the blue box (outside the area) cut off.
  const last = readFrames(await runClient(`${cg.url}/etv?channel=2&area=200x40%2B20%2B90`, data, 1500), width, height).at(-1);
  const at = (px, py) => [...last.subarray((py * width + px) * 4, (py * width + px) * 4 + 4)];
  assert.deepEqual(at(100, 110), [255, 0, 0, 255]);
  assert.deepEqual(at(100, 20), [0, 0, 0, 0]);
});
