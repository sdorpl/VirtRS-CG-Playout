// HTTP side of the sidecar.
//
//   POST /etv?channel=1[&render=720p]
//                         body: ErsatzTV template data (JSON from the script
//                         element's stdin); reply: endless ETV packet stream
//   GET  /ffmpeg?channel=1[&wait=2000]
//                         the ffmpeg overlay (client/vizrs-ffmpeg.py) takes over
//                         a script session of the channel: its packets
//                         stop, the reply gives the session, size and rate
//   GET  /ffmpeg/frames?session=N[&fps=25/1]
//                         raw yuva420p frames of that session for ffmpeg
//   GET  /health          captures and sessions

import http from "node:http";

import { Capture, findBrowser } from "./capture.js";
import { canScale, renderSize } from "./scale.js";
import { clipContext } from "./template.js";
import { streamFrames, yuvaFormat } from "./stream.js";
import { ChannelEvents } from "./vizrs.js";

const MAX_BODY = 4 * 1024 * 1024;
// A channel served by the ffmpeg overlay holds its script packets this long
// for the overlay to take the session over; then ErsatzTV draws them itself.
const HOLD_MS = 3000;
// A session taken over waits this long for the overlay's frame stream.
const FRAMES_WAIT_MS = 10000;
const MAX_WAIT_MS = 10000;

/** "25", "25/1" or "30000/1001" as frames per second; 0 if not a rate. */
export function parseRate(value) {
  const [num, den = "1"] = String(value ?? "").split("/");
  const rate = Number(num) / Number(den);
  return Number.isFinite(rate) && rate > 0 ? rate : 0;
}

/** Transparent frames when burn-in is off or the size is unknown. */
const EMPTY = { frame: { seq: 0, rects: [] }, status: "off" };

export function createServer(config, { log = console, captureFactory } = {}) {
  const browser = config.browser || findBrowser();
  const makeCapture = captureFactory ?? ((size) =>
    new Capture({ url: config.output_url, browser, noSandbox: config.no_sandbox, extraArgs: config.browser_args, log, ...size }).start());
  const captures = new Map();
  const channels = new Map();
  let sessions = 0;
  // Script sessions by id, for the ffmpeg overlay to take over.
  const scripts = new Map();
  const waiters = new Map();
  // Channels whose ffmpeg runs through the overlay: their sessions start held.
  const overlayChannels = new Set();
  let nextId = 0;

  /** One capture per channel size and render size, shared by the channels that have both. */
  const captureFor = (width, height, fps, spec) => {
    if (!config.output_url || !width || !height) return EMPTY;

    let render = { width, height };
    try {
      render = renderSize(spec, width, height);
    } catch (error) {
      log.warn(`render size: ${error.message}; using ${width}x${height}`);
    }
    if ((render.width !== width || render.height !== height) && !canScale) {
      log.warn(`render size ${render.width}x${render.height} needs sharp; using ${width}x${height}`);
      render = { width, height };
    }

    const key = `${width}x${height}/${render.width}x${render.height}`;
    if (!captures.has(key)) {
      captures.set(key, makeCapture({ width: render.width, height: render.height, outWidth: width, outHeight: height, fps }));
    }
    return captures.get(key);
  };

  const eventsFor = (channel) => {
    if (!channels.has(channel)) channels.set(channel, new ChannelEvents(config, log));
    return channels.get(channel);
  };

  const free = (channel) => [...scripts.values()].find((s) => s.channel === channel && !s.claimed && s.capture !== EMPTY);

  /** The overlay takes the session: ErsatzTV gets no more packets from it. */
  const claim = (script) => {
    script.claimed = true;
    clearTimeout(script.hold);
    script.stream.pause();
    overlayChannels.add(script.channel);
    // No frame stream after all (the overlay gave up): ErsatzTV draws again.
    script.claimTimer = setTimeout(() => {
      script.claimed = false;
      script.stream.resume();
    }, FRAMES_WAIT_MS);
    script.claimTimer.unref?.();
    return script;
  };

  const waitForScript = (channel, ms) => new Promise((resolve) => {
    const found = free(channel);
    if (found) return resolve(claim(found));
    if (!waiters.has(channel)) waiters.set(channel, []);
    const list = waiters.get(channel);
    const done = (script) => {
      clearTimeout(timer);
      list.splice(list.indexOf(done), 1);
      resolve(script);
    };
    const timer = setTimeout(() => done(null), ms);
    list.push(done);
  });

  const register = (script) => {
    scripts.set(script.id, script);
    const list = waiters.get(script.channel) ?? [];
    if (list.length) list[0](claim(script));
  };

  const claimRequest = async (url, response) => {
    const channel = url.searchParams.get("channel") ?? "1";
    const wait = Math.min(MAX_WAIT_MS, Math.max(0, Number(url.searchParams.get("wait") ?? 2000) || 0));
    const script = await waitForScript(channel, wait);
    if (!script) {
      response.statusCode = 404;
      response.end();
      return;
    }

    log.info(`channel ${channel}: ffmpeg overlay took over the graphics of ${script.ctx.title}`);
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ session: script.id, width: script.ctx.width, height: script.ctx.height, fps: script.fps }));
  };

  const framesRequest = (url, response) => {
    const script = scripts.get(Number(url.searchParams.get("session")));
    if (!script?.claimed || script.frames) {
      response.statusCode = 404;
      response.end();
      return;
    }

    clearTimeout(script.claimTimer);
    script.frames = response;
    const { capture } = script;
    const fps = parseRate(url.searchParams.get("fps")) || script.fps;
    capture.raiseFps?.(fps);
    capture.acquire?.();
    response.writeHead(200, { "Content-Type": "application/octet-stream", "Cache-Control": "no-store" });
    response.flushHeaders();
    response.on("close", () => {
      capture.release?.();
      // The transcode is over: the script ends with it, as without the overlay.
      script.response.end();
    });
    streamFrames(capture, response, {
      fps,
      leadSeconds: config.lead_seconds,
      format: yuvaFormat(script.ctx.width, script.ctx.height),
    });
  };

  const server = http.createServer((request, response) => {
    const url = new URL(request.url, "http://localhost");

    if (request.method === "GET" && url.pathname === "/health") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({
        sessions,
        captures: [...captures.values()].map((c) => ({ size: c.size ?? c.key, render: c.render ?? c.key, fps: c.fps, status: c.status, seq: c.frame.seq })),
        overlay: [...overlayChannels],
      }));
      return;
    }

    if (request.method === "GET" && url.pathname === "/ffmpeg") {
      claimRequest(url, response);
      return;
    }

    if (request.method === "GET" && url.pathname === "/ffmpeg/frames") {
      framesRequest(url, response);
      return;
    }

    if (request.method !== "POST" || url.pathname !== "/etv") {
      response.statusCode = 404;
      response.end();
      return;
    }

    const chunks = [];
    let size = 0;
    request.on("data", (chunk) => {
      size += chunk.length;
      if (size > MAX_BODY) request.destroy();
      else chunks.push(chunk);
    });
    request.on("end", () => {
      let ctx;
      try {
        ctx = clipContext(JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}"));
      } catch (error) {
        response.statusCode = 400;
        response.end(`bad template data: ${error.message}`);
        return;
      }

      const channel = url.searchParams.get("channel") ?? "1";
      const fps = ctx.fps > 0 ? ctx.fps : 25;
      log.info(`channel ${channel}: ${ctx.title} (${ctx.width}x${ctx.height}@${fps}, seek ${ctx.seek.toFixed(1)} s)`);

      // Commands go out in the background; frames start at once.
      const ended = eventsFor(channel).session(ctx);

      const capture = captureFor(ctx.width, ctx.height, fps, url.searchParams.get("render") ?? config.render_size);
      capture.acquire?.();
      response.writeHead(200, { "Content-Type": "application/octet-stream", "Cache-Control": "no-store" });
      // A held session writes nothing at first; the client must see the reply at once.
      response.flushHeaders();
      sessions += 1;
      const held = overlayChannels.has(channel) && capture !== EMPTY;
      const script = {
        id: ++nextId, channel, ctx, fps, capture, response, claimed: false, frames: null, hold: null, claimTimer: null,
        stream: streamFrames(capture, response, { fps, leadSeconds: config.lead_seconds, paused: held }),
      };
      if (held) {
        script.hold = setTimeout(() => {
          overlayChannels.delete(channel);
          log.warn(`channel ${channel}: no ffmpeg overlay; ErsatzTV draws the graphics`);
          script.stream.resume();
        }, HOLD_MS);
        script.hold.unref?.();
      }
      response.on("close", () => {
        sessions -= 1;
        scripts.delete(script.id);
        clearTimeout(script.hold);
        clearTimeout(script.claimTimer);
        capture.release?.();
        ended();
      });
      register(script);
    });
  });

  server.stopCaptures = () => {
    for (const capture of captures.values()) capture.stop?.();
  };

  return server;
}
