// Clip context from the ErsatzTV template data (stdin JSON of the script
// element) and rendering of the VizRS command templates.

import path from "node:path";

/** Seconds of a .NET TimeSpan in JSON ("[d.]hh:mm:ss[.fffffff]"). */
export function timeSpanSeconds(value) {
  if (typeof value === "number") return value;
  if (typeof value !== "string") return 0;

  const match = /^(-)?(?:(\d+)\.)?(\d+):(\d+):(\d+(?:\.\d+)?)$/.exec(value.trim());
  if (!match) return 0;

  const [, minus, days = "0", h, m, s] = match;
  const total = Number(days) * 86400 + Number(h) * 3600 + Number(m) * 60 + Number(s);
  return minus ? -total : total;
}

/** `hh:mm:ss` for durations, wrapping past 24 h. */
export function clock(seconds) {
  const total = Math.max(0, Math.round(seconds));
  const pad = (n) => String(n).padStart(2, "0");
  return `${pad(Math.floor(total / 3600) % 24)}:${pad(Math.floor(total / 60) % 60)}:${pad(total % 60)}`;
}

/** Wall clock `hh:mm:ss` in the offset ErsatzTV sent ("2026-10-24T20:15:00+02:00"). */
function localTime(iso) {
  const match = typeof iso === "string" ? /T(\d{2}:\d{2}:\d{2})/.exec(iso) : null;
  return match ? match[1] : "";
}

/** Values must not break `key=value|key=value` or the line protocol. */
export function cleanValue(value) {
  return String(value ?? "").replace(/[\r\n]/g, "").replace(/\|/g, "/");
}

function stem(file) {
  return file ? path.parse(String(file)).name : "";
}

/** Context of the clip described by the ErsatzTV template data. */
export function clipContext(data) {
  const source = data.MediaItem_Path ?? "";
  const start = Date.parse(data.MediaItem_Start ?? "");
  const stop = Date.parse(data.MediaItem_Stop ?? "");
  const title = [data.MediaItem_Title, data.MediaItem_ShowTitle].find((t) => t && String(t).trim()) ?? stem(source);

  // EPG entries start at the current programme; the next one starts after this clip.
  const epg = Array.isArray(data.Epg) ? data.Epg : [];
  const next = epg.find((entry) => Date.parse(entry.Start) > (Number.isNaN(start) ? Date.now() : start) + 1000);
  const nextStart = next ? Date.parse(next.Start) : NaN;
  const nextStop = next ? Date.parse(next.Stop) : NaN;

  return {
    title: String(title),
    show_title: String(data.MediaItem_ShowTitle ?? ""),
    source: String(source),
    start: data.MediaItem_Start ?? "",
    stop: data.MediaItem_Stop ?? "",
    duration: Number.isNaN(start) || Number.isNaN(stop) ? timeSpanSeconds(data.MediaItem_Duration) : (stop - start) / 1000,
    seek: timeSpanSeconds(data.MediaItem_StreamSeek),
    next_title: next ? String(next.Title ?? "") : "",
    next_start_full: next ? localTime(next.Start) : "",
    next_duration: Number.isNaN(nextStart) || Number.isNaN(nextStop) ? 0 : (nextStop - nextStart) / 1000,
    width: data.Resolution?.Width ?? 0,
    height: data.Resolution?.Height ?? 0,
    fps: Number(data.FrameRate) || 0,
  };
}

/** Commercial, by the configured regular expressions on path and title. */
export function isAd(ctx, adMatch) {
  const test = (pattern, value) => Boolean(pattern) && new RegExp(pattern, "i").test(value);
  return test(adMatch?.path, ctx.source) || test(adMatch?.title, ctx.title);
}

function renderLine(line, ctx) {
  const values = {
    "{title}": cleanValue(ctx.title),
    "{show_title}": cleanValue(ctx.show_title),
    "{source}": cleanValue(ctx.source),
    "{duration}": clock(ctx.duration),
    "{next_title}": cleanValue(ctx.next_title),
    "{next_start}": ctx.next_start_full.slice(0, 5),
    "{next_start_full}": ctx.next_start_full,
    "{next_duration}": clock(ctx.next_duration),
  };

  return Object.entries(values).reduce((acc, [key, value]) => acc.split(key).join(value), line);
}

/** One command per line; empty lines and `#` comments are skipped. */
export function renderCommands(template, ctx) {
  return String(template ?? "")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"))
    .map((line) => renderLine(line, ctx));
}
