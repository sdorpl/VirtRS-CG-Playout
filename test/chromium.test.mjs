// Real headless Chromium on a local transparent page; skipped without a browser.

import assert from "node:assert/strict";
import http from "node:http";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

import { Capture, findBrowser } from "../src/capture.js";

const browser = findBrowser();
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
    noSandbox: process.getuid?.() === 0,
    log: quiet,
  }).start();

  try {
    const deadline = Date.now() + 20000;
    while (!capture.frame.rect && Date.now() < deadline) await sleep(100);

    assert.deepEqual(capture.frame.rect, { x: 40, y: 300, w: 400, h: 60 });
    const [r, g, b, a] = capture.frame.pixels.subarray(0, 4);
    assert.deepEqual([r, g, b, a], [255, 0, 0, 255]);
  } finally {
    capture.stop();
    server.close();
  }
});
