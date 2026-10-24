import assert from "node:assert/strict";
import { test } from "node:test";

import { loadConfig } from "../src/config.js";
import { clearPacket, crop, rectanglesPacket, repeatPacket, visibleRects } from "../src/packets.js";
import { decodePng } from "../src/png.js";
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
});
