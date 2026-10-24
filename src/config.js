// Configuration: JSON file (optional) with environment overrides.

import { readFileSync } from "node:fs";

export const DEFAULTS = {
  listen: "0.0.0.0:7700",
  vizrs: { address: "127.0.0.1:6100", token: "" },
  // VizRS /output page burned into the video; empty = commands only.
  output_url: "",
  on_clip: "",
  on_ad: "",
  // Regular expressions (case-insensitive) for commercials, on path and title.
  ad_match: { path: "", title: "" },
  // An item without the VizRS graphics element (no session after the previous
  // item ends) is treated as a commercial, like an unchecked watermark.
  ad_without_element: true,
  // How far the packet stream may run ahead of real time (ErsatzTV burst).
  // Graphics reach the picture this much later than they change in VizRS.
  lead_seconds: 4,
  browser: "",
  no_sandbox: false,
  // Extra Chromium flags, e.g. fewer raster threads on small ARM hosts.
  browser_args: [],
};

const unescape = (value) => value.replace(/\\n/g, "\n");

export function loadConfig(file, env = process.env) {
  let fromFile = {};
  if (file) fromFile = JSON.parse(readFileSync(file, "utf8"));

  const config = {
    ...DEFAULTS,
    ...fromFile,
    vizrs: { ...DEFAULTS.vizrs, ...fromFile.vizrs },
    ad_match: { ...DEFAULTS.ad_match, ...fromFile.ad_match },
  };

  if (env.LISTEN) config.listen = env.LISTEN;
  if (env.VIZRS_ADDRESS) config.vizrs.address = env.VIZRS_ADDRESS;
  if (env.VIZRS_TOKEN !== undefined) config.vizrs.token = env.VIZRS_TOKEN;
  if (env.VIZRS_OUTPUT_URL !== undefined) config.output_url = env.VIZRS_OUTPUT_URL;
  if (env.VIZRS_ON_CLIP !== undefined) config.on_clip = unescape(env.VIZRS_ON_CLIP);
  if (env.VIZRS_ON_AD !== undefined) config.on_ad = unescape(env.VIZRS_ON_AD);
  if (env.VIZRS_AD_PATH !== undefined) config.ad_match.path = env.VIZRS_AD_PATH;
  if (env.VIZRS_AD_TITLE !== undefined) config.ad_match.title = env.VIZRS_AD_TITLE;
  if (env.VIZRS_AD_WITHOUT_ELEMENT !== undefined) config.ad_without_element = env.VIZRS_AD_WITHOUT_ELEMENT !== "0";
  if (env.LEAD_SECONDS) config.lead_seconds = Number(env.LEAD_SECONDS);
  if (env.VIZRS_BROWSER) config.browser = env.VIZRS_BROWSER;
  if (env.VIZRS_BROWSER_NO_SANDBOX === "1") config.no_sandbox = true;
  if (env.VIZRS_BROWSER_ARGS) config.browser_args = env.VIZRS_BROWSER_ARGS.split(/\s+/).filter(Boolean);

  return config;
}
