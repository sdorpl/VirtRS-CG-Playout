#!/usr/bin/env node
// VirtRS CG Playout sidecar: `virtrs-cg [config.json]`.

import { loadConfig } from "../src/config.js";
import { createServer } from "../src/server.js";

const debug = process.env.DEBUG === "1";
const stamp = () => new Date().toISOString();
const log = {
  debug: (...a) => debug && console.log(stamp(), "DEBUG", ...a),
  info: (...a) => console.log(stamp(), "INFO", ...a),
  warn: (...a) => console.warn(stamp(), "WARN", ...a),
  error: (...a) => console.error(stamp(), "ERROR", ...a),
};

const config = loadConfig(process.argv[2] ?? process.env.CONFIG);
const index = config.listen.lastIndexOf(":");
const host = config.listen.slice(0, index);
const port = Number(config.listen.slice(index + 1));

const server = createServer(config, { log });
server.listen(port, host, () => {
  log.info(`listening on ${config.listen}; VizRS ${config.vizrs.address}; burn-in ${config.output_url || "off"}`);
});

const shutdown = () => {
  server.stopCaptures();
  server.close();
  process.exit(0);
};
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
