// VizRS TCP client (line protocol, default port 6100) and the per-channel
// logic of clip and ad events: ads are played without graphics, the pages
// taken out come back with the first regular clip.

import net from "node:net";

import { isAd, renderCommands } from "./template.js";

const CONNECT_TIMEOUT = 2000;
const REPLY_TIMEOUT = 2000;
// After an item's stop: no new session by then = the next item has no element.
const GAP_GRACE = 2000;

/** OUT for all layers on air in a `STATE` reply, and their pages. */
export function onAir(stateReply) {
  if (!stateReply.startsWith("OK")) return null;

  let state;
  try {
    state = JSON.parse(stateReply.slice(2).trim());
  } catch {
    return null;
  }

  const layers = Array.isArray(state?.layers) ? state.layers : [];
  const names = layers.map((l) => l?.layer).filter((n) => typeof n === "string");
  // VizRS reports the page as an object ({id, number, name}); older builds as a string.
  const pages = layers
    .map((l) => (l?.page && typeof l.page === "object" ? l.page.number : l?.page))
    .filter((p) => (typeof p === "string" || typeof p === "number") && p !== "")
    .map(String);

  return names.length ? { out: `OUT ${names.join(",")}`, pages } : null;
}

class Connection {
  constructor(socket) {
    this.socket = socket;
    this.buffer = "";
    this.waiting = null;
    this.closed = false;

    socket.setEncoding("utf8");
    socket.on("data", (chunk) => {
      this.buffer += chunk;
      this.flush();
    });
    socket.on("close", () => {
      this.closed = true;
      this.flush();
    });
    socket.on("error", () => {});
  }

  flush() {
    if (!this.waiting) return;

    const index = this.buffer.indexOf("\n");
    if (index >= 0) {
      const line = this.buffer.slice(0, index).trim();
      this.buffer = this.buffer.slice(index + 1);
      const { resolve, timer } = this.waiting;
      this.waiting = null;
      clearTimeout(timer);
      resolve(line);
    } else if (this.closed) {
      const { reject, timer } = this.waiting;
      this.waiting = null;
      clearTimeout(timer);
      reject(new Error("connection closed"));
    }
  }

  command(line) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting = null;
        reject(new Error(`${line.split(" ")[0]}: no reply`));
      }, REPLY_TIMEOUT);

      this.waiting = { resolve, reject, timer };
      this.socket.write(`${line}\n`);
      this.flush();
    });
  }

  close() {
    this.socket.destroy();
  }
}

function connect(address) {
  const index = address.lastIndexOf(":");
  const host = address.slice(0, index) || "127.0.0.1";
  const port = Number(address.slice(index + 1));

  return new Promise((resolve, reject) => {
    const socket = net.connect({ host, port, noDelay: true });
    const timer = setTimeout(() => {
      socket.destroy();
      reject(new Error(`connect to ${address}: timeout`));
    }, CONNECT_TIMEOUT);

    socket.once("connect", () => {
      clearTimeout(timer);
      resolve(new Connection(socket));
    });
    socket.once("error", (error) => {
      clearTimeout(timer);
      reject(new Error(`connect to ${address}: ${error.message}`));
    });
  });
}

/**
 * Sends commands over one connection: replies per command and the pages taken
 * out by `clearFirst` (all layers on air, from `STATE`).
 */
export async function send(address, token, commands, clearFirst) {
  const connection = await connect(address);
  const sent = { replies: [], clearedPages: [] };

  try {
    if (token) {
      const reply = await connection.command(`AUTH ${token}`);
      if (!reply.startsWith("OK")) throw new Error(`AUTH: ${reply}`);
    }

    if (clearFirst) {
      const air = onAir(await connection.command("STATE"));

      if (air) {
        const reply = await connection.command(air.out);
        if (reply.startsWith("OK")) sent.clearedPages = air.pages;
        sent.replies.push([air.out, reply]);
      }
    }

    for (const command of commands) {
      sent.replies.push([command, await connection.command(command)]);
    }
  } finally {
    connection.close();
  }

  return sent;
}

/** Clip events of one channel, sent in order. */
export class ChannelEvents {
  constructor(config, log = console) {
    this.config = config;
    this.log = log;
    this.queue = Promise.resolve();
    this.cleared = null;
    this.lastKey = null;
    this.active = 0;
    this.gapTimer = null;
  }

  /** A script session of the channel starts; returns its end callback. */
  session(ctx) {
    this.active += 1;
    clearTimeout(this.gapTimer);
    this.clip(ctx);
    return () => this.ended(ctx);
  }

  /**
   * A session ended. When no session follows by the item's stop (ErsatzTV runs
   * ahead of the clock), the next item has no VizRS element: it is an ad.
   */
  ended(ctx) {
    this.active = Math.max(0, this.active - 1);
    if (!this.config.ad_without_element || this.active > 0) return;

    const key = `${ctx.source}|${ctx.start}`;
    const stop = Date.parse(ctx.stop ?? "");
    const delay = (Number.isNaN(stop) ? 0 : Math.max(0, stop - Date.now())) + (this.config.gap_grace_ms ?? GAP_GRACE);

    clearTimeout(this.gapTimer);
    this.gapTimer = setTimeout(() => {
      if (this.active === 0 && this.lastKey === key) this.ad(ctx);
    }, delay);
    this.gapTimer.unref?.();
  }

  /** An item without the element: graphics out and `on_ad`, as for a matched ad. */
  ad(ctx) {
    this.lastKey = `ad|${ctx.stop ?? ""}`;
    const adCtx = { ...ctx, title: "", show_title: "", source: "", duration: 0 };
    const commands = renderCommands(this.config.on_ad, adCtx);

    this.queue = this.queue.then(() => this.sendClip(true, commands)).catch(() => {});
    return this.queue;
  }

  /**
   * Events for a clip that starts (or is joined) now. ErsatzTV starts the
   * script again when a new session joins the same item; that does not send
   * the commands twice (and does not restart the animations).
   */
  clip(ctx) {
    const key = `${ctx.source}|${ctx.start}`;
    if (key === this.lastKey) return this.queue;
    this.lastKey = key;

    const ad = isAd(ctx, this.config.ad_match);
    const commands = renderCommands(ad ? this.config.on_ad : this.config.on_clip, ctx);

    this.queue = this.queue.then(() => this.sendClip(ad, commands)).catch(() => {});
    return this.queue;
  }

  async sendClip(ad, commands) {
    const { address, token } = this.config.vizrs;
    // Graphics taken out for the break come back before the clip's own commands.
    const restore = ad ? null : this.cleared;
    if (restore) {
      this.cleared = null;
      commands.unshift(`TAKE ${restore.join(",")}`);
    }

    if (!commands.length && !ad) return;

    try {
      const sent = await send(address, token, commands, ad);
      // Later ads of the same break only find `on_ad` graphics on air.
      if (sent.clearedPages.length && !this.cleared) this.cleared = sent.clearedPages;

      for (const [command, reply] of sent.replies) {
        if (reply.startsWith("OK")) this.log.debug?.(`VizRS ${command}: ${reply}`);
        else this.log.warn(`VizRS ${command}: ${reply}`);
      }
    } catch (error) {
      // Try again with the next regular clip.
      if (restore) this.cleared = restore;
      this.log.error(`VizRS: ${error.message}`);
    }
  }
}
