// The reference vectors (test/vectors/*.json) still match the Node.js build:
// after a deliberate change, `node test/vectors/generate.mjs` rewrites them,
// and the Rust build must follow. Scaling (libvips, SIMD per CPU) is compared
// with the tolerance stored in the file, everything else exactly.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { serialize, vectors } from "./vectors/generate.mjs";

const STALE = (name) => `test/vectors/${name}.json is stale: node test/vectors/generate.mjs`;

function assertScale(stored, fresh) {
  assert.equal(stored.cases.length, fresh.cases.length, STALE("scale"));
  stored.cases.forEach((expected, i) => {
    const actual = fresh.cases[i];
    assert.deepEqual(actual.scaled.map(({ pixels_b64, ...r }) => r), expected.scaled.map(({ pixels_b64, ...r }) => r), STALE("scale"));
    actual.scaled.forEach((rect, j) => {
      const a = Buffer.from(rect.pixels_b64, "base64");
      const b = Buffer.from(expected.scaled[j].pixels_b64, "base64");
      const off = a.findIndex((value, k) => Math.abs(value - b[k]) > stored.tolerance);
      assert.equal(off, -1, `${expected.name}: byte ${off} off by more than ${stored.tolerance}`);
    });
  });
}

test("reference vectors match the Node.js build", async () => {
  for (const [name, data] of Object.entries(await vectors())) {
    const stored = readFileSync(new URL(`./vectors/${name}.json`, import.meta.url), "utf8");
    if (name === "scale") assertScale(JSON.parse(stored), JSON.parse(serialize(data)));
    else assert.equal(stored, serialize(data), STALE(name));
  }
});
