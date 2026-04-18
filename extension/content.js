// sidekar content.js — ISOLATED-world bridge.
//
// Listens for CustomEvents dispatched by inject-net.js on document, owns a
// bounded ring buffer, and pushes events opportunistically to background.js
// via chrome.runtime.sendMessage({type: "sk_net_firehose", ...}).
//
// Background relays to the daemon as {type: "net_passive_event", ...} frames.

(() => {
  "use strict";

  // Re-entry guard.
  if (window.__sidekar_content_active === true) return;
  try {
    Object.defineProperty(window, "__sidekar_content_active", {
      value: true,
      configurable: false,
      enumerable: false,
      writable: false,
    });
  } catch {
    return;
  }

  const BUFFER_CAP = 500;
  const BODY_PREVIEW_CAP = 64 * 1024; // 64 KiB
  const FLUSH_INTERVAL_MS = 250;
  const FLUSH_BATCH_MAX = 50;

  const buffer = []; // ring of events awaiting flush
  let dropped = 0;
  let emit_off = false; // when true, skip buffer push; inject-net.js also muted

  // --- Redaction (runs on flush path, not hot path) --------------------------
  const SENSITIVE_HEADER_KEYS = new Set([
    "authorization",
    "cookie",
    "set-cookie",
    "x-auth-token",
    "x-access-token",
    "x-refresh-token",
    "x-csrf-token",
    "x-xsrf-token",
  ]);

  function redactHeaders(h) {
    if (!h || typeof h !== "object") return h;
    const out = {};
    for (const k of Object.keys(h)) {
      if (SENSITIVE_HEADER_KEYS.has(String(k).toLowerCase())) {
        out[k] = "[REDACTED]";
      } else {
        out[k] = h[k];
      }
    }
    return out;
  }

  const JWT_RE = /\beyJ[A-Za-z0-9._-]{20,}\b/g;
  const BEARER_RE = /Bearer\s+[A-Za-z0-9._~+/=-]{10,}/gi;
  const TOKEN_KV_RE =
    /("?(authorization|cookie|set-cookie|access[_-]?token|refresh[_-]?token|csrf|session(id)?|jwt)"?\s*[:=]\s*"?)([^"\s,&}]+)/gi;

  function redactBody(s) {
    if (typeof s !== "string") return s;
    try {
      return s
        .replace(TOKEN_KV_RE, "$1[REDACTED]")
        .replace(JWT_RE, "[REDACTED_JWT]")
        .replace(BEARER_RE, "Bearer [REDACTED]");
    } catch {
      return s;
    }
  }

  function capBody(s) {
    if (typeof s !== "string") return { body: s, truncated: false };
    if (s.length <= BODY_PREVIEW_CAP) return { body: s, truncated: false };
    return {
      body: s.slice(0, BODY_PREVIEW_CAP),
      truncated: true,
      fullLength: s.length,
    };
  }

  // --- Buffer push (hot path — minimal work) --------------------------------
  function push(kind, detail) {
    if (emit_off) return;
    if (buffer.length >= BUFFER_CAP) {
      buffer.shift();
      dropped++;
    }
    buffer.push({ kind, detail });
  }

  function setEmitOff(off) {
    emit_off = !!off;
    try {
      document.dispatchEvent(
        new CustomEvent("__sidekar_emit_off", { detail: { off: emit_off } })
      );
    } catch {}
    if (emit_off) {
      buffer.length = 0;
      dropped = 0;
    }
  }

  // --- Listeners --------------------------------------------------------------
  document.addEventListener("__sidekar_net", (e) => push("net", e.detail));
  document.addEventListener("__sidekar_sse", (e) => push("sse", e.detail));
  document.addEventListener("__sidekar_sse_done", (e) => push("sse_done", e.detail));
  document.addEventListener("__sidekar_sse_open", (e) => push("sse_open", e.detail));
  document.addEventListener("__sidekar_sse_error", (e) => push("sse_error", e.detail));

  // --- Flush loop (idle, batched) -------------------------------------------
  let flushing = false;
  async function flush() {
    if (flushing || buffer.length === 0) return;
    flushing = true;
    try {
      const batch = buffer.splice(0, FLUSH_BATCH_MAX);
      const droppedNow = dropped;
      dropped = 0;

      // Redact + cap on flush path, not hot path.
      const prepared = batch.map((item) => {
        const d = item.detail || {};
        if (item.kind === "net") {
          const out = Object.assign({}, d);
          if (out.reqHeaders) out.reqHeaders = redactHeaders(out.reqHeaders);
          if (typeof out.body === "string") {
            const capped = capBody(out.body);
            out.body = redactBody(capped.body);
            if (capped.truncated) {
              out.bodyTruncated = true;
              out.bodyFullLength = capped.fullLength;
            }
          }
          return { kind: item.kind, detail: out };
        }
        if (item.kind === "sse") {
          // Per-chunk SSE is bounded by the natural chunk size — don't redact
          // by default; agents consuming SSE want the raw event frame.
          return { kind: item.kind, detail: d };
        }
        return { kind: item.kind, detail: d };
      });

      try {
        chrome.runtime.sendMessage({
          type: "sk_net_firehose",
          v: 1,
          tabUrl: location.href,
          frameUrl: location.href,
          events: prepared,
          dropped: droppedNow,
          t: Date.now(),
        });
      } catch {
        // If background is asleep or disconnected, push events back and retry
        // on next tick. Bounded by BUFFER_CAP so we don't loop indefinitely.
        for (let i = prepared.length - 1; i >= 0; i--) {
          if (buffer.length >= BUFFER_CAP) break;
          buffer.unshift(prepared[i]);
        }
      }
    } finally {
      flushing = false;
    }
  }

  setInterval(flush, FLUSH_INTERVAL_MS);

  // --- Kill switch + control channel ----------------------------------------
  chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
    if (!msg || typeof msg !== "object") return false;
    if (msg.type === "sk_passive_emit_off") {
      setEmitOff(!!msg.off);
      sendResponse({ ok: true, emit_off });
      return false;
    }
    if (msg.type === "sk_passive_flush") {
      flush();
      sendResponse({ ok: true, pending: buffer.length });
      return false;
    }
    if (msg.type === "sk_passive_query") {
      sendResponse({
        ok: true,
        emit_off,
        pending: buffer.length,
        dropped,
      });
      return false;
    }
    return false;
  });
})();
