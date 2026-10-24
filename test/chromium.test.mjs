// Real headless Chromium on a local transparent page; skipped without a browser.

import assert from "node:assert/strict";
import http from "node:http";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

import { Capture, findBrowser } from "../src/capture.js";

const browser = findBrowser();
// Without the sandbox as root, and where the host forbids it (CI runners, containers).
const noSandbox = process.getuid?.() === 0 || process.env.VIZRS_BROWSER_NO_SANDBOX === "1";
const quiet = { debug() {}, info() {}, warn: (m) => process.env.TRACE && console.error(m), error: (m) => process.env.TRACE && console.error(m) };

const PAGE = `<!doctype html><html><body style="margin:0;background:transparent">
<div style="position:absolute;left:40px;top:300px;width:400px;height:60px;background:rgb(255,0,0)"></div>
</body></html>`;

test("Chromium capture gives the visible box of a transparent page", { skip: !browser && "no Chromium", timeout: 30000 }, async () => {
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
    const deadline = Date.now() + 20000;
    while (!capture.frame.rects.length && Date.now() < deadline) await sleep(100);

    const [{ pixels, ...rect }] = capture.frame.rects;
    assert.deepEqual(rect, { x: 40, y: 300, w: 400, h: 60 });
    const [r, g, b, a] = pixels.subarray(0, 4);
    assert.deepEqual([r, g, b, a], [255, 0, 0, 255]);
  } finally {
    capture.stop();
    server.close();
  }
});

test("Chromium capture pauses without sessions and resumes with a fresh frame", { skip: !browser && "no Chromium", timeout: 30000 }, async () => {
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
    const deadline = Date.now() + 20000;
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

test("Chromium capture renders smaller and scales to the channel", { skip: !browser && "no Chromium", timeout: 30000 }, async () => {
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
    const deadline = Date.now() + 20000;
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
