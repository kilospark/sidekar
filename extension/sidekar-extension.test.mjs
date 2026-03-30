/**
 * Node smoke tests for extension clipboard / paste helpers (no Chrome APIs).
 * Run: node --test extension/sidekar-extension.test.mjs
 */
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/** Keep behavior aligned with offscreen.js withTimeout */
function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms)
    ),
  ]);
}

test("withTimeout rejects when inner promise never settles", async () => {
  await assert.rejects(
    async () => {
      await withTimeout(new Promise(() => {}), 40, "hang");
    },
    (e) => /hang timed out after 40ms/.test(String(e.message))
  );
});

test("withTimeout resolves when inner promise is fast", async () => {
  const v = await withTimeout(Promise.resolve("ok"), 500, "fast");
  assert.equal(v, "ok");
});

test("background.js: Google Docs plain-text skips offscreen clipboard", () => {
  const bg = fs.readFileSync(path.join(__dirname, "background.js"), "utf8");
  assert.match(bg, /skipped-google-docs-plain/);
  assert.match(bg, /offscreenMessageTimeoutMs = 12000/);
  assert.ok(bg.includes("html || !isGoogleDocs"));
  assert.ok(bg.includes("await writeClipboardContents(tabId, html, plainText)"));
});

test("background.js: ensureOffscreenReady pings offscreen before clipboard", () => {
  const bg = fs.readFileSync(path.join(__dirname, "background.js"), "utf8");
  assert.ok(bg.includes("function ensureOffscreenReady"));
  assert.ok(bg.includes("offscreenPing"));
  assert.ok(bg.includes("await ensureOffscreenReady()"));
});

test("offscreen.js: navigator clipboard calls use bounded withTimeout", () => {
  const src = fs.readFileSync(path.join(__dirname, "offscreen.js"), "utf8");
  assert.match(src, /NAV_CLIPBOARD_TIMEOUT_MS/);
  assert.match(src, /withTimeout\(\s*navigator\.clipboard\.write/);
  assert.match(src, /withTimeout\(\s*navigator\.clipboard\.writeText/);
  assert.match(src, /offscreenPing/);
  assert.match(src, /offscreen-deadline/);
  assert.match(src, /setTimeout\(resolve, 32\)/);
});
