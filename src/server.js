// HTTP side of the sidecar.
//
//   POST /etv?channel=1   body: ErsatzTV template data (JSON from the script
//                         element's stdin); reply: endless ETV packet stream
//   GET  /health          captures and sessions

import http from "node:http";

import { Capture, findBrowser } from "./capture.js";
import { clipContext } from "./template.js";
import { streamFrames } from "./stream.js";
import { ChannelEvents } from "./vizrs.js";

const MAX_BODY = 4 * 1024 * 1024;

/** Transparent frames when burn-in is off or the size is unknown. */
const EMPTY = { frame: { seq: 0, rects: [] }, status: "off" };

export function createServer(config, { log = console, captureFactory } = {}) {
  const browser = config.browser || findBrowser();
  const makeCapture = captureFactory ?? ((size) =>
    new Capture({ url: config.output_url, browser, noSandbox: config.no_sandbox, extraArgs: config.browser_args, log, ...size }).start());
  const captures = new Map();
  const channels = new Map();
  let sessions = 0;

  const captureFor = (width, height, fps) => {
    if (!config.output_url || !width || !height) return EMPTY;
    const key = `${width}x${height}`;
    if (!captures.has(key)) captures.set(key, makeCapture({ width, height, fps }));
    return captures.get(key);
  };

  const eventsFor = (channel) => {
    if (!channels.has(channel)) channels.set(channel, new ChannelEvents(config, log));
    return channels.get(channel);
  };

  const server = http.createServer((request, response) => {
    const url = new URL(request.url, "http://localhost");

    if (request.method === "GET" && url.pathname === "/health") {
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({
        sessions,
        captures: [...captures.values()].map((c) => ({ size: c.key, status: c.status, seq: c.frame.seq })),
      }));
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

      const capture = captureFor(ctx.width, ctx.height, fps);
      capture.acquire?.();
      response.writeHead(200, { "Content-Type": "application/octet-stream", "Cache-Control": "no-store" });
      sessions += 1;
      response.on("close", () => {
        sessions -= 1;
        capture.release?.();
        ended();
      });
      streamFrames(capture, response, { fps, leadSeconds: config.lead_seconds });
    });
  });

  server.stopCaptures = () => {
    for (const capture of captures.values()) capture.stop?.();
  };

  return server;
}
