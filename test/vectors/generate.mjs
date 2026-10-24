// Reference vectors from the Node.js build, for the Rust tests (rust/tests/,
// docs/plany/PLAN-rust.md): inputs and the exact outputs of the pure parts.
// `node test/vectors/generate.mjs` writes test/vectors/*.json; vectors.test.mjs
// fails when they differ from what the Node.js build gives now.
//
// Byte outputs are `{ b64 }`, large ones `{ sha256 }`.
// Pictures are described as fills on a transparent canvas, so both sides can
// build the same RGBA.

import { createHash } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { everyNthFrame } from "../../src/capture.js";
import { DEFAULTS, loadConfig } from "../../src/config.js";
import { clearPacket, crop, rectanglesPacket, repeatPacket, visibleRects } from "../../src/packets.js";
import { decodePng } from "../../src/png.js";
import { renderSize, scaleRects } from "../../src/scale.js";
import { parseRate } from "../../src/server.js";
import { clipContext, clock, cleanValue, isAd, renderCommands, timeSpanSeconds } from "../../src/template.js";
import { onAir } from "../../src/vizrs.js";
import { yuvaFrame, yuvaSize } from "../../src/yuva.js";

import { encodePng } from "../helpers.mjs";

const DIR = path.dirname(fileURLToPath(import.meta.url));
const b64 = (buffer) => Buffer.from(buffer).toString("base64");
const sha256 = (buffer) => createHash("sha256").update(buffer).digest("hex");
/** Small outputs in full, large ones as a hash. */
const bytes = (buffer) => (buffer.length <= 4096 ? { b64: b64(buffer) } : { sha256: sha256(buffer) });

/** RGBA canvas from `{ width, height, fills: [{ x, y, w, h, rgba }] }`, later fills on top. */
export function paint({ width, height, fills }) {
  const rgba = Buffer.alloc(width * height * 4);
  for (const { x, y, w, h, rgba: color } of fills) {
    for (let row = y; row < y + h; row++) {
      for (let col = x; col < x + w; col++) rgba.set(color, (row * width + col) * 4);
    }
  }
  return rgba;
}

const RED = [255, 0, 0, 255];
const WHITE = [255, 255, 255, 255];
const HALF_BLUE = [0, 0, 255, 128];

const PICTURES = {
  empty: { width: 16, height: 8, fills: [] },
  colour_without_alpha: { width: 10, height: 6, fills: [{ x: 1, y: 0, w: 1, h: 1, rgba: [200, 0, 0, 0] }] },
  one_pixel: { width: 10, height: 6, fills: [{ x: 3, y: 2, w: 1, h: 1, rgba: WHITE }, { x: 7, y: 4, w: 1, h: 1, rgba: [9, 9, 9, 10] }] },
  channel_branding: {
    width: 320,
    height: 180,
    fills: [
      { x: 10, y: 10, w: 40, h: 20, rgba: WHITE },
      { x: 260, y: 12, w: 50, h: 16, rgba: RED },
      { x: 0, y: 150, w: 320, h: 20, rgba: HALF_BLUE },
      { x: 100, y: 160, w: 3, h: 3, rgba: WHITE },
    ],
  },
  gap_under_32: {
    width: 320,
    height: 180,
    fills: [{ x: 10, y: 10, w: 40, h: 20, rgba: WHITE }, { x: 70, y: 10, w: 10, h: 10, rgba: RED }],
  },
  rows_gap_32: {
    width: 64,
    height: 100,
    fills: [{ x: 0, y: 0, w: 4, h: 2, rgba: WHITE }, { x: 0, y: 34, w: 4, h: 2, rgba: WHITE }, { x: 0, y: 66, w: 4, h: 2, rgba: WHITE }],
  },
  scattered: {
    width: 400,
    height: 200,
    fills: Array.from({ length: 40 }, (_, i) => ({ x: (i % 10) * 40, y: Math.floor(i / 10) * 50, w: 1, h: 1, rgba: WHITE })),
  },
  edges: {
    width: 33,
    height: 17,
    fills: [{ x: 0, y: 0, w: 1, h: 1, rgba: RED }, { x: 32, y: 16, w: 1, h: 1, rgba: HALF_BLUE }],
  },
};

function templateVectors() {
  const news = {
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
  };
  const contexts = [
    news,
    // .NET DateTimeOffset as ErsatzTV writes it: seven fractional digits.
    {
      Resolution: { Width: 1920, Height: 1080 },
      FrameRate: "50",
      MediaItem_Path: "/media/filmy/Noc w muzeum (2006).mkv",
      MediaItem_Title: "Noc w muzeum\nreżyserska",
      MediaItem_ShowTitle: "Kino",
      MediaItem_Start: "2026-10-24T21:59:59.5000000+02:00",
      MediaItem_Stop: "2026-10-24T23:47:12.2500000+02:00",
      MediaItem_StreamSeek: "01:02:03.0040000",
      Epg: [
        { Title: "Teraz", Start: "2026-10-24T22:00:00.0000000+02:00", Stop: "2026-10-24T23:47:12.0000000+02:00" },
        { Title: "Potem", Start: "2026-10-24T23:47:12.0000000+02:00", Stop: "2026-10-25T00:17:12.0000000+02:00" },
      ],
    },
    // UTC and an EPG entry starting within a second of the clip: not "next".
    {
      Resolution: { Width: 720, Height: 576 },
      FrameRate: 25,
      MediaItem_Path: "/m/a.mp4",
      MediaItem_Title: "A",
      MediaItem_Start: "2026-10-24T18:00:00Z",
      MediaItem_Stop: "2026-10-24T18:00:30Z",
      Epg: [
        { Title: "Ten sam", Start: "2026-10-24T18:00:01Z", Stop: "2026-10-24T18:00:30Z" },
        { Title: "Następny", Start: "2026-10-24T18:00:30Z", Stop: "2026-10-24T18:30:00Z" },
      ],
    },
    // No times: the duration from the TimeSpan, the title from the file name.
    { MediaItem_Path: "/m/plansza.mp4", MediaItem_Duration: "1.02:00:00" },
    { MediaItem_Path: "/m/x.mp4", MediaItem_Title: "  ", MediaItem_ShowTitle: "Seria", MediaItem_Duration: 12.5, FrameRate: "x" },
    {},
  ].map((data) => ({ data, ctx: clipContext(data) }));

  const ctx = contexts[0].ctx;
  const templates = [
    "# strap\nTAKE 4001 headline={title}|sub=Potem: {next_title} o {next_start}\n\nUPDATE 2001 d={duration}",
    "  TAKE 1 {show_title} {source}  \n#x\nUPDATE 2 n={next_start_full} nd={next_duration} {title}{title}",
    "TAKE 5 {unknown} {TITLE}",
    "",
  ];
  const commands = [...contexts.map(({ ctx: c }) => ({ template: templates[0], ctx: c })), ...templates.map((template) => ({ template, ctx }))]
    .map(({ template, ctx: c }) => ({ template, ctx: c, commands: renderCommands(template, c) }));

  const spans = ["00:01:30.5000000", "1.02:00:00", "-00:00:05", " 01:00:00 ", "10:00", "garbage", "", 12.5, null, "00:00:00.1"];
  const clocks = [0, 59.4, 59.5, 3600, 90061, -5, 86461, 1.49];
  const clean = ["a|b", "line\r\nbreak", "", null, 42, "Zażółć | gęślą"];

  const match = { path: "/reklamy/", title: "^REKLAMA" };
  const ads = [
    { ctx: { source: "/media/Reklamy/a.mp4", title: "x" }, ad_match: match },
    { ctx: { source: "/m/a.mp4", title: "Reklama – bank" }, ad_match: match },
    { ctx: { source: "/m/a.mp4", title: "Film" }, ad_match: match },
    { ctx: { source: "/reklamy/a.mp4", title: "x" }, ad_match: {} },
    { ctx: { source: "/m/spot_01.mp4", title: "x" }, ad_match: { path: "spot_\\d+\\.mp4$", title: "" } },
    { ctx: { source: "/m/a.mp4", title: "BLOK REKLAMOWY" }, ad_match: { path: "", title: "blok (reklamowy|sponsorski)" } },
  ].map((c) => ({ ...c, ad: isAd(c.ctx, c.ad_match) }));

  return {
    contexts,
    commands,
    time_spans: spans.map((input) => ({ input, seconds: timeSpanSeconds(input) })),
    clocks: clocks.map((seconds) => ({ seconds, clock: clock(seconds) })),
    clean: clean.map((input) => ({ input, value: cleanValue(input) })),
    ads,
  };
}

function vizrsVectors() {
  const replies = [
    'OK {"layers":[{"layer":"main","page":"1001"},{"layer":"logo","page":""}]}',
    'OK {"layers":[{"layer":"main","page":{"id":"p1","number":1001,"name":"Belka"}}]}',
    'OK {"layers":[{"layer":"a","page":7},{"layer":"b"},{"page":"9"},{"layer":"c","page":null}]}',
    'OK {"layers":[]}',
    "OK {}",
    "OK not json",
    "ERR unauthorized",
    "",
  ];
  return { on_air: replies.map((reply) => ({ reply, result: onAir(reply) })) };
}

function configVectors() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "virtrs-vectors-"));
  const file = path.join(dir, "config.json");
  const cases = [
    { file: null, env: {} },
    { file: null, env: { VIZRS_ADDRESS: "vizrs:6100", VIZRS_ON_CLIP: "TAKE 1\\nTAKE 2", LEAD_SECONDS: "1" } },
    { file: null, env: { VIZRS_AD_WITHOUT_ELEMENT: "0", VIZRS_RENDER_SIZE: "720p", VIZRS_BROWSER_NO_SANDBOX: "1", VIZRS_BROWSER_ARGS: "  --a  --b=1 " } },
    { file: null, env: { VIZRS_AD_WITHOUT_ELEMENT: "no", VIZRS_TOKEN: "", VIZRS_OUTPUT_URL: "", VIZRS_BROWSER_NO_SANDBOX: "true", LEAD_SECONDS: "" } },
    {
      file: { listen: "127.0.0.1:7701", vizrs: { token: "t" }, ad_match: { title: "^R" }, lead_seconds: 2.5, on_ad: "OUT x", gap_grace_ms: 50 },
      env: {},
    },
    {
      file: { vizrs: { address: "h:1", token: "file" }, ad_match: { path: "/a/", title: "b" }, browser_args: ["--x"] },
      env: { VIZRS_TOKEN: "env", VIZRS_AD_PATH: "/env/", VIZRS_ON_AD: "OUT a\\nOUT b", LISTEN: "0.0.0.0:1", VIZRS_BROWSER: "/usr/bin/chromium" },
    },
  ];
  try {
    return {
      defaults: DEFAULTS,
      cases: cases.map((c) => {
        if (c.file) writeFileSync(file, JSON.stringify(c.file));
        return { ...c, config: loadConfig(c.file ? file : null, c.env) };
      }),
    };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function serverVectors() {
  const rates = ["25", "25/1", "30000/1001", "50", "", "x/0", "0", "-25", "25/0", null, "24000/1001"];
  const sizes = [
    ["", 1920, 1080], ["auto", 1280, 720], [" AUTO ", 1280, 720], ["720p", 1920, 1080], ["540", 1920, 1080],
    ["1080P", 1280, 720], ["1024x576", 1920, 1080], ["720p", 720, 576], ["hd", 1920, 1080], ["8p", 1920, 1080],
    ["10000x10", 1920, 1080], ["1280X720", 1920, 1080], [null, 640, 360],
  ];
  return {
    rates: rates.map((input) => ({ input, fps: parseRate(input) })),
    render_sizes: sizes.map(([spec, width, height]) => {
      try {
        return { spec, width, height, size: renderSize(spec, width, height) };
      } catch (error) {
        return { spec, width, height, error: error.message };
      }
    }),
    every_nth_frame: [0, 1, 24, 25, 29.97, 30, 50, 59.94, 60, 100].map((fps) => ({ fps, n: everyNthFrame(fps) })),
  };
}

function frameVectors() {
  const rects = Object.entries(PICTURES).map(([name, picture]) => ({
    name,
    picture,
    rects: visibleRects(paint(picture), picture.width, picture.height),
  }));

  // Packets of those pictures, as ErsatzTV receives them.
  const packets = rects.map(({ name, picture, rects: boxes }) => {
    const rgba = paint(picture);
    const packet = boxes.length ? rectanglesPacket(boxes.map((r) => ({ ...r, pixels: crop(rgba, picture.width, r) }))) : clearPacket();
    return { name, packet: bytes(packet) };
  });
  const repeats = [0, 1, 2, 25, 70000].map((frames) => ({ frames, packet: bytes(repeatPacket(frames)) }));

  // yuva420p: BT.601 below 720 lines, BT.709 from 720; odd sizes; alpha-weighted chroma.
  const own = [
    ["empty", 4, 2, []],
    ["white_and_half_red", 4, 2, [{ x: 0, y: 0, w: 2, h: 1, pixels: [...WHITE, 255, 0, 0, 128] }]],
    ["odd_size", 5, 3, [{ x: 3, y: 1, w: 2, h: 2, pixels: [...RED, ...HALF_BLUE, 0, 255, 0, 255, 0, 0, 0, 0] }]],
    ["hd_red", 1280, 720, [{ x: 0, y: 0, w: 1, h: 1, pixels: RED }]],
  ].map(([name, width, height, boxes]) => ({
    name,
    width,
    height,
    rects: boxes.map(({ pixels, ...b }) => ({ ...b, pixels_b64: b64(pixels) })),
    size: yuvaSize(width, height),
    frame: bytes(yuvaFrame(boxes.map((b) => ({ ...b, pixels: Buffer.from(b.pixels) })), width, height)),
  }));
  // The pictures above, through their visible rectangles (cropped from `paint`).
  const fromPictures = rects.filter(({ rects: r }) => r.length).map(({ name, picture, rects: r }) => {
    const rgba = paint(picture);
    const boxes = r.map((box) => ({ ...box, pixels: crop(rgba, picture.width, box) }));
    return { name: `picture_${name}`, picture: name, size: yuvaSize(picture.width, picture.height), frame: bytes(yuvaFrame(boxes, picture.width, picture.height)) };
  });
  const yuva = [...own, ...fromPictures];

  return { rects, packets, repeats, yuva };
}

function pngVectors() {
  const rgba = Buffer.alloc(7 * 5 * 4);
  for (let i = 0; i < rgba.length; i++) rgba[i] = (i * 37 + (i >> 5)) & 0xff;
  const cases = [];
  for (const alpha of [true, false]) {
    for (const filter of [0, 1, 2, 3, 4]) {
      const png = encodePng(7, 5, rgba, { filter, alpha });
      const decoded = decodePng(png);
      cases.push({ filter, alpha, png_b64: b64(png), width: decoded.width, height: decoded.height, rgba_b64: b64(decoded.rgba) });
    }
  }
  return { cases };
}

async function scaleVectors() {
  // Lanczos through libvips: the Rust side compares with a small tolerance.
  const cases = [
    { name: "box_1_5x", picture: { width: 40, height: 20, fills: [{ x: 10, y: 5, w: 20, h: 10, rgba: RED }] }, out: [60, 30] },
    { name: "720p_to_1080p", picture: { width: 128, height: 72, fills: [{ x: 8, y: 50, w: 64, h: 8, rgba: WHITE }, { x: 100, y: 4, w: 20, h: 10, rgba: HALF_BLUE }] }, out: [192, 108] },
    { name: "down_half", picture: { width: 64, height: 36, fills: [{ x: 0, y: 0, w: 64, h: 6, rgba: RED }, { x: 20, y: 20, w: 9, h: 9, rgba: WHITE }] }, out: [32, 18] },
  ];
  const results = [];
  for (const { name, picture, out } of cases) {
    const rgba = paint(picture);
    const rects = visibleRects(rgba, picture.width, picture.height);
    const scaled = await scaleRects(rgba, picture.width, picture.height, rects, out[0], out[1]);
    results.push({
      name,
      picture,
      out_width: out[0],
      out_height: out[1],
      rects,
      scaled: scaled.map(({ pixels, ...r }) => ({ ...r, pixels_b64: b64(pixels) })),
    });
  }
  return { tolerance: 2, cases: results };
}

/** All vector files by name. */
export async function vectors() {
  return {
    template: templateVectors(),
    vizrs: vizrsVectors(),
    config: configVectors(),
    server: serverVectors(),
    frames: frameVectors(),
    png: pngVectors(),
    scale: await scaleVectors(),
  };
}

export const serialize = (data) => `${JSON.stringify(data, null, 1)}\n`;

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  for (const [name, data] of Object.entries(await vectors())) {
    writeFileSync(path.join(DIR, `${name}.json`), serialize(data));
    console.log(`test/vectors/${name}.json`);
  }
}
