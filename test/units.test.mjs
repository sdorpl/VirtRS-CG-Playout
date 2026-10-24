import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import { test } from "node:test";
import { setTimeout as sleep } from "node:timers/promises";

import { Capture, drawnAt, everyNthFrame } from "../src/capture.js";
import { loadConfig } from "../src/config.js";
import { clearPacket, crop, framePacket, rectanglesPacket, repeatPacket, visibleRects } from "../src/packets.js";
import { decodePng } from "../src/png.js";
import { renderSize, scaleRects } from "../src/scale.js";
import { SAMPLE_DELAY_MS, streamFrames } from "../src/stream.js";
import { clipContext, isAd, renderCommands, timeSpanSeconds } from "../src/template.js";
import { onAir } from "../src/vizrs.js";

import { encodePng, readFrames } from "./helpers.mjs";

function picture(width, height) {
  const rgba = Buffer.alloc(width * height * 4);
  for (let i = 0; i < rgba.length; i++) rgba[i] = (i * 37 + (i >> 5)) & 0xff;
  return rgba;
}

test("PNG decodes every filter type, RGBA and RGB", () => {
  const rgba = picture(7, 5);
  for (const filter of [0, 1, 2, 3, 4]) {
    assert.deepEqual(decodePng(encodePng(7, 5, rgba, { filter })).rgba, rgba);
  }

  const opaque = Buffer.from(rgba);
  for (let i = 3; i < opaque.length; i += 4) opaque[i] = 255;
  assert.deepEqual(decodePng(encodePng(7, 5, rgba, { alpha: false, filter: 4 })).rgba, opaque);
});

test("visible rectangles and crop", () => {
  const width = 10;
  const rgba = Buffer.alloc(width * 6 * 4);
  rgba[(2 * width + 3) * 4 + 3] = 255;
  rgba[(4 * width + 7) * 4 + 3] = 10;
  rgba[(0 * width + 1) * 4] = 200; // colour without alpha stays invisible

  const rects = visibleRects(rgba, width, 6);
  assert.deepEqual(rects, [{ x: 3, y: 2, w: 5, h: 3 }]);
  assert.equal(crop(rgba, width, rects[0]).length, 5 * 3 * 4);
  assert.deepEqual(visibleRects(Buffer.alloc(40), 5, 2), []);
});

test("distant graphics become separate rectangles", () => {
  const [width, height] = [320, 180];
  const rgba = Buffer.alloc(width * height * 4);
  const fill = (x0, y0, w, h) => {
    for (let y = y0; y < y0 + h; y++) for (let x = x0; x < x0 + w; x++) rgba[(y * width + x) * 4 + 3] = 255;
  };
  fill(10, 10, 40, 20); // logo
  fill(260, 12, 50, 16); // clock, same rows
  fill(0, 150, 320, 20); // ticker
  fill(100, 160, 3, 3); // inside the ticker

  assert.deepEqual(visibleRects(rgba, width, height), [
    { x: 10, y: 10, w: 40, h: 20 },
    { x: 260, y: 12, w: 50, h: 16 },
    { x: 0, y: 150, w: 320, h: 20 },
  ]);

  // Gaps under 32 px stay in one rectangle.
  fill(70, 10, 10, 10);
  assert.deepEqual(visibleRects(rgba, width, height)[0], { x: 10, y: 10, w: 70, h: 20 });
});

test("packets are read the way ErsatzTV draws them", () => {
  const [width, height] = [4, 3];
  const rects = [
    { x: 1, y: 1, w: 2, h: 1, pixels: Buffer.from([1, 2, 3, 4, 5, 6, 7, 8]) },
    { x: 0, y: 2, w: 1, h: 1, pixels: Buffer.from([9, 10, 11, 12]) },
  ];
  const stream = Buffer.concat([clearPacket(), rectanglesPacket(rects), repeatPacket(3), clearPacket()]);

  const frames = readFrames(stream, width, height);
  assert.equal(frames.length, 1 + 1 + 3 + 1);
  assert.ok(frames[0].every((b) => b === 0));
  assert.deepEqual(frames[1].subarray((1 * width + 1) * 4, (1 * width + 3) * 4), rects[0].pixels);
  assert.deepEqual(frames[1].subarray(2 * width * 4, (2 * width + 1) * 4), rects[1].pixels);
  assert.deepEqual(frames[4], frames[1]);
  assert.ok(frames[5].every((b) => b === 0));
});

test("a frame's packet is built once for all sessions", () => {
  const frame = { seq: 1, rects: [{ x: 0, y: 0, w: 1, h: 1, pixels: Buffer.alloc(4, 7) }] };
  assert.equal(framePacket(frame), framePacket(frame));
  assert.deepEqual(framePacket({ seq: 0, rects: [] }), clearPacket());
});

test("screencast step keeps the channel rate", () => {
  assert.equal(everyNthFrame(25), 2);
  assert.equal(everyNthFrame(30), 2);
  assert.equal(everyNthFrame(50), 1);
  assert.equal(everyNthFrame(60), 1);
  assert.equal(everyNthFrame(0), 60);
});

test("capture history gives the picture of a past moment", () => {
  const capture = new Capture({ url: "", width: 2, height: 1, fps: 25 });
  const first = capture.frame;
  capture.setFrame([{ x: 0, y: 0, w: 1, h: 1, pixels: Buffer.alloc(4, 1) }]);
  const second = capture.frame;
  assert.equal(capture.frameAt(first.at - 1), first, "older than the history: the oldest frame");
  assert.equal(capture.frameAt(second.at), second);
  assert.equal(capture.frameAt(second.at + 1000), second);
});

test("screencast timestamp on the performance clock", () => {
  const now = performance.now();
  const at = drawnAt((performance.timeOrigin + now - 30) / 1000, now);
  assert.ok(Math.abs(at - (now - 30)) < 1, `${at} vs ${now - 30}`);
  assert.equal(drawnAt(undefined, now), now, "missing: arrival time");
  assert.equal(drawnAt((performance.timeOrigin + now + 500) / 1000, now), now, "in the future: arrival time");
  assert.equal(drawnAt((performance.timeOrigin + now - 5000) / 1000, now), now, "too old: arrival time");
});

test("a picture dated in the past keeps the history in order", () => {
  const capture = new Capture({ url: "", width: 1, height: 1, fps: 25 });
  const rect = () => [{ x: 0, y: 0, w: 1, h: 1, pixels: Buffer.alloc(4, 1) }];
  capture.setFrame(rect(), performance.now());
  const first = capture.frame;
  capture.setFrame(rect(), first.at - 50);
  assert.equal(capture.frame.at, first.at);
});

test("pictures decoded with varying delay each land in their own output frame", async () => {
  const capture = new Capture({ url: "", width: 1, height: 1, fps: 25 });
  // Chromium draws mid-slot every 40 ms; each picture is decoded 0–85 ms later.
  const started = performance.now();
  const pending = [];
  let drawn = 0;
  const deliver = setInterval(() => {
    const now = performance.now();
    while (started + 10 + drawn * 40 <= now) {
      const at = started + 10 + drawn * 40;
      pending.push({ at, value: (drawn % 250) + 1, ready: at + Math.random() * 85 });
      drawn += 1;
    }
    while (pending.length && pending[0].ready <= now) {
      const { at, value } = pending.shift();
      capture.setFrame([{ x: 0, y: 0, w: 1, h: 1, pixels: Buffer.alloc(4, value) }], at);
    }
  }, 5);
  const response = new EventEmitter();
  const chunks = [];
  response.write = (chunk) => chunks.push(chunk) > 0;

  const stream = streamFrames(capture, response, { fps: 25, leadSeconds: 0 });
  await sleep(1500);
  stream.stop();
  clearInterval(deliver);

  const values = readFrames(Buffer.concat(chunks), 1, 1).map((frame) => frame[0]).filter((v) => v > 0);
  const steps = values.slice(1).map((v, i) => v - values[i]);
  const exact = steps.filter((step) => step === 1).length;
  assert.ok(exact >= steps.length - 1, `steps: ${steps}`);
});

test("frames written late still show motion, not one picture repeated", async () => {
  const capture = new Capture({ url: "", width: 1, height: 1, fps: 25 });
  const animate = setInterval(() => {
    capture.setFrame([{ x: 0, y: 0, w: 1, h: 1, pixels: Buffer.alloc(4, (capture.frame.seq % 250) + 1) }]);
  }, 20);
  // Output frames look SAMPLE_DELAY_MS back: the animation runs that long first.
  await sleep(SAMPLE_DELAY_MS + 40);
  // ErsatzTV stops reading for 400 ms after the first packet, then catches up.
  const response = new EventEmitter();
  const chunks = [];
  let blocked = true;
  response.write = (chunk) => {
    chunks.push(chunk);
    if (!blocked) return true;
    blocked = false;
    setTimeout(() => response.emit("drain"), 400);
    return false;
  };

  const stream = streamFrames(capture, response, { fps: 25, leadSeconds: 0 });
  await sleep(600);
  stream.stop();
  clearInterval(animate);

  const values = readFrames(Buffer.concat(chunks), 1, 1).map((frame) => frame[0]);
  const late = values.slice(1, 10);
  assert.ok(new Set(late).size >= 8, `frames after the stall: ${late}`);
});

test("render size from the option", () => {
  assert.deepEqual(renderSize("", 1920, 1080), { width: 1920, height: 1080 });
  assert.deepEqual(renderSize("auto", 1280, 720), { width: 1280, height: 720 });
  assert.deepEqual(renderSize("720p", 1920, 1080), { width: 1280, height: 720 });
  assert.deepEqual(renderSize("540", 1920, 1080), { width: 960, height: 540 });
  assert.deepEqual(renderSize("1080P", 1280, 720), { width: 1920, height: 1080 });
  assert.deepEqual(renderSize("1024x576", 1920, 1080), { width: 1024, height: 576 });
  assert.throws(() => renderSize("hd", 1920, 1080), /bad render size/);
  assert.throws(() => renderSize("8p", 1920, 1080), /out of range/);
});

test("rectangles scale to the channel with soft, unshifted edges", async () => {
  const [width, height] = [40, 20];
  const rgba = Buffer.alloc(width * height * 4);
  for (let y = 5; y < 15; y++) for (let x = 10; x < 30; x++) rgba.set([255, 0, 0, 255], (y * width + x) * 4);

  const [r] = await scaleRects(rgba, width, height, [{ x: 10, y: 5, w: 20, h: 10 }], 60, 30);
  // 4 px of transparent margin, scaled 1.5×: inside the 60x30 canvas.
  assert.deepEqual([r.x, r.y, r.w, r.h], [9, 1, 42, 28]);
  assert.equal(r.pixels.length, r.w * r.h * 4);

  const at = (x, y) => [...r.pixels.subarray(((y - r.y) * r.w + (x - r.x)) * 4, ((y - r.y) * r.w + (x - r.x)) * 4 + 4)];
  assert.deepEqual(at(30, 15), [255, 0, 0, 255], "inside stays solid");
  assert.equal(at(9, 1)[3], 0, "margin stays transparent");
  const edge = at(15, 15);
  assert.ok(edge[3] > 0 && edge[0] === 255, `edge keeps its colour (premultiplied): ${edge}`);
});

test("ErsatzTV template data becomes the clip context", () => {
  const ctx = clipContext({
    Resolution: { Width: 1280, Height: 720, Name: "720p" },
    FrameRate: 25,
    MediaItem_Path: "/media/news/Wiadomosci.mp4",
    MediaItem_Title: "",
    MediaItem_ShowTitle: "Wiadomości",
    MediaItem_Start: "2026-10-24T20:00:00+02:00",
    MediaItem_Stop: "2026-10-24T20:30:00+02:00",
    MediaItem_StreamSeek: "00:01:30.5000000",
    Epg: [
      { Title: "Wiadomości", Start: "2026-10-24T20:00:00+02:00", Stop: "2026-10-24T20:30:00+02:00" },
      { Title: "Film | premiera", Start: "2026-10-24T20:30:00+02:00", Stop: "2026-10-24T22:10:00+02:00" },
    ],
  });

  assert.equal(ctx.title, "Wiadomości");
  assert.equal(ctx.duration, 1800);
  assert.equal(ctx.seek, 90.5);
  assert.equal(ctx.next_title, "Film | premiera");
  assert.equal(ctx.next_start_full, "20:30:00");
  assert.equal(ctx.next_duration, 6000);
  assert.deepEqual([ctx.width, ctx.height, ctx.fps], [1280, 720, 25]);

  assert.deepEqual(
    renderCommands("# strap\nTAKE 4001 headline={title}|sub=Potem: {next_title} o {next_start}\n\nUPDATE 2001 d={duration}", ctx),
    ["TAKE 4001 headline=Wiadomości|sub=Potem: Film / premiera o 20:30", "UPDATE 2001 d=00:30:00"],
  );
  assert.equal(clipContext({ MediaItem_Path: "/m/plansza.mp4" }).title, "plansza");
  assert.equal(timeSpanSeconds("1.02:00:00"), 93600);
});

test("ads by path or title", () => {
  const match = { path: "/reklamy/", title: "^REKLAMA" };
  assert.ok(isAd({ source: "/media/Reklamy/a.mp4", title: "x" }, match));
  assert.ok(isAd({ source: "/m/a.mp4", title: "Reklama – bank" }, match));
  assert.ok(!isAd({ source: "/m/a.mp4", title: "Film" }, match));
  assert.ok(!isAd({ source: "/reklamy/a.mp4", title: "x" }, {}));
});

test("STATE reply gives OUT and pages", () => {
  assert.deepEqual(onAir('OK {"layers":[{"layer":"main","page":"1001"},{"layer":"logo","page":""}]}'), {
    out: "OUT main,logo",
    pages: ["1001"],
  });
  assert.deepEqual(onAir('OK {"layers":[{"layer":"main","page":{"id":"p1","number":1001,"name":"Belka"}}]}'), {
    out: "OUT main",
    pages: ["1001"],
  });
  assert.equal(onAir('OK {"layers":[]}'), null);
  assert.equal(onAir("ERR"), null);
});

test("config overrides", () => {
  const config = loadConfig(null, { VIZRS_ADDRESS: "vizrs:6100", VIZRS_ON_CLIP: "TAKE 1\\nTAKE 2", LEAD_SECONDS: "1" });
  assert.equal(config.vizrs.address, "vizrs:6100");
  assert.equal(config.on_clip, "TAKE 1\nTAKE 2");
  assert.equal(config.lead_seconds, 1);
  assert.equal(config.ad_without_element, true);
  assert.equal(loadConfig(null, { VIZRS_AD_WITHOUT_ELEMENT: "0" }).ad_without_element, false);
  assert.equal(config.render_size, "");
  assert.equal(loadConfig(null, { VIZRS_RENDER_SIZE: "720p" }).render_size, "720p");
});
