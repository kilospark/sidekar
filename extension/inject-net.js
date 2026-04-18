// sidekar inject-net — MAIN-world passive network capture.
//
// Patches: window.fetch, XMLHttpRequest, EventSource.
// Emits CustomEvents on document: __sidekar_net, __sidekar_sse, __sidekar_sse_done,
// __sidekar_sse_error, __sidekar_headers.
// ISOLATED-world content.js listens, buffers, flushes to background.
//
// Design rules (per review):
// - Tiny, deterministic, fail-open. Any internal error must return control to
//   the page's original code, never throw into app code.
// - Patch fetch/XHR/EventSource only. No wider prototype walking.
// - Preserve toString / name / length on wrappers so naive anti-bot checks pass.
// - Cap early, redact late. Never stringify/redact before queueing if body is
//   large — push raw (capped) bytes to content.js, let it redact on idle flush.
// - Kill switch: window.__sidekar_passive_off === true disables all patching.
//   Scheme denylist applied before install.

(() => {
  "use strict";

  // --- Scheme denylist --------------------------------------------------------
  try {
    const proto = (location && location.protocol) || "";
    if (
      proto === "chrome:" ||
      proto === "chrome-extension:" ||
      proto === "devtools:" ||
      proto === "about:" ||
      proto === "file:"
    ) {
      return;
    }
    // PDF viewer check is best-effort; the extension won't be injected there by
    // default on most Chromes, but guard for embedded viewers.
    if (document.contentType && document.contentType === "application/pdf") {
      return;
    }
  } catch {
    // If location is inaccessible, bail — we're in a sandbox we shouldn't touch.
    return;
  }

  // --- Kill switch ------------------------------------------------------------
  try {
    if (window.__sidekar_passive_off === true) return;
  } catch {
    return;
  }

  // Re-entry guard (same window, multiple frame injections).
  if (window.__sidekar_net_patched === true) return;
  try {
    Object.defineProperty(window, "__sidekar_net_patched", {
      value: true,
      configurable: false,
      enumerable: false,
      writable: false,
    });
  } catch {
    return;
  }

  const SCHEMA_V = 2;

  // --- Safe dispatcher --------------------------------------------------------
  // The content.js side can toggle emit_off via document event to quiet
  // capture without unpatching. Patched wrappers stay in place (we can't
  // truly un-inject a document), but emit() becomes a no-op and the tee
  // bookkeeping skips chunk accumulation.
  let emit_off = false;
  try {
    document.addEventListener("__sidekar_emit_off", (e) => {
      try {
        emit_off = !!(e && e.detail && e.detail.off);
      } catch {}
    });
  } catch {}
  function emit(name, detail) {
    if (emit_off) return;
    try {
      document.dispatchEvent(new CustomEvent(name, { detail }));
    } catch {
      // Swallow — never throw into app code.
    }
  }
  function capture_off() {
    return emit_off;
  }

  // --- Wrapper-descriptor helper ---------------------------------------------
  // Copies toString/name/length onto a wrapper so `String(fetch)` and
  // `fetch.toString()` do not betray the patch.
  function mirror(target, original) {
    try {
      const origString = Function.prototype.toString.call(original);
      const ts = function toString() {
        return origString;
      };
      Object.defineProperty(ts, "toString", {
        value: ts,
        configurable: true,
        writable: true,
      });
      Object.defineProperty(target, "toString", {
        value: ts,
        configurable: true,
        writable: true,
      });
      Object.defineProperty(target, "name", {
        value: original.name,
        configurable: true,
      });
      Object.defineProperty(target, "length", {
        value: original.length,
        configurable: true,
      });
    } catch {
      // non-fatal
    }
    return target;
  }

  // --- Header extraction ------------------------------------------------------
  function headersToObject(h) {
    if (!h) return undefined;
    try {
      if (h instanceof Headers) {
        const out = {};
        h.forEach((v, k) => {
          out[k] = v;
        });
        return out;
      }
      if (Array.isArray(h)) {
        const out = {};
        for (const [k, v] of h) out[k] = v;
        return out;
      }
      if (typeof h === "object") {
        return Object.assign({}, h);
      }
    } catch {}
    return undefined;
  }

  // --- Body classification ----------------------------------------------------
  // Return { kind: "text"|"binary"|"form"|"other", sizeHint?: number }.
  // Bodies get textified only when kind === "text". Otherwise a marker lands.
  function classifyInit(init) {
    try {
      if (!init || init.body == null) return { kind: "none" };
      const b = init.body;
      if (typeof b === "string") return { kind: "text", sizeHint: b.length };
      if (b instanceof Blob) return { kind: "binary", sizeHint: b.size };
      if (b instanceof ArrayBuffer) return { kind: "binary", sizeHint: b.byteLength };
      if (ArrayBuffer.isView(b)) return { kind: "binary", sizeHint: b.byteLength };
      if (typeof FormData !== "undefined" && b instanceof FormData) return { kind: "form" };
      if (typeof URLSearchParams !== "undefined" && b instanceof URLSearchParams) {
        const s = b.toString();
        return { kind: "text", sizeHint: s.length, _cached: s };
      }
      if (typeof ReadableStream !== "undefined" && b instanceof ReadableStream) {
        return { kind: "stream" };
      }
      return { kind: "other" };
    } catch {
      return { kind: "other" };
    }
  }

  // --- fetch patch ------------------------------------------------------------
  const originalFetch = window.fetch;
  if (typeof originalFetch === "function") {
    const patched = function fetch(input, init) {
      let url;
      try {
        url =
          typeof input === "string"
            ? input
            : input && input.url
              ? input.url
              : String(input);
      } catch {
        return originalFetch.apply(this, arguments);
      }

      const method =
        (init && init.method) ||
        (input && input.method) ||
        "GET";
      const reqHeaders = headersToObject(init && init.headers);
      const reqBody = classifyInit(init);
      const t0 = Date.now();

      let promise;
      try {
        promise = originalFetch.apply(this, arguments);
      } catch (err) {
        // Sync failure — still emit, then rethrow.
        emit("__sidekar_net", {
          v: SCHEMA_V,
          kind: "fetch",
          phase: "error",
          transport: "fetch",
          captureState: "metadata_only",
          url,
          method,
          reqHeaders,
          reqBody,
          t0,
          err: String(err && err.message ? err.message : err),
        });
        throw err;
      }

      return promise.then(
        (response) => {
          try {
            const status = response.status;
            const ct = (response.headers.get && response.headers.get("content-type")) || "";
            const acceptHeader =
              (reqHeaders && (reqHeaders["accept"] || reqHeaders["Accept"])) || "";
            const isSse =
              String(ct).toLowerCase().includes("text/event-stream") ||
              String(acceptHeader).toLowerCase().includes("text/event-stream");

            if (isSse && response.body && !response.bodyUsed) {
              // Tee the stream — page gets real bytes, we accumulate a copy.
              try {
                const reader = response.body.getReader();
                const decoder = new TextDecoder("utf-8");
                let chunkSeq = 0;
                let totalBytes = 0;
                // Rolling per-stream cap: keep most-recent N bytes in memory
                // rather than accumulating from the start. Long-lived streams
                // stay bounded even if sse_done never fires.
                const ROLLING_CAP = 2 * 1024 * 1024; // 2 MiB
                const IDLE_FINALIZE_MS = 60 * 1000;
                let droppedChunks = 0;
                let droppedBytes = 0;
                const chunks = [];
                let chunkBytes = 0;
                let lastChunkAt = Date.now();
                let idleTimer = null;
                let finalized = false;

                function finalizeStream(reason) {
                  if (finalized) return;
                  finalized = true;
                  if (idleTimer) {
                    try {
                      clearTimeout(idleTimer);
                    } catch {}
                    idleTimer = null;
                  }
                  const body = chunks.join("");
                  const captureState =
                    droppedChunks > 0 ? "truncated" : "full";
                  emit("__sidekar_net", {
                    v: SCHEMA_V,
                    kind: "fetch",
                    phase: reason === "error" ? "error" : "done",
                    transport: "fetch_stream",
                    captureState,
                    url,
                    method,
                    status,
                    ct,
                    reqHeaders,
                    reqBody,
                    body,
                    bodyKind: "text",
                    t0,
                    t1: Date.now(),
                    sse: true,
                  });
                  emit("__sidekar_sse_done", {
                    v: SCHEMA_V,
                    url,
                    method,
                    status,
                    totalChunks: chunkSeq,
                    totalBytes,
                    droppedChunks,
                    droppedBytes,
                    captureState,
                    reason,
                    duration: Date.now() - t0,
                  });
                }

                function armIdle() {
                  if (idleTimer) {
                    try {
                      clearTimeout(idleTimer);
                    } catch {}
                  }
                  idleTimer = setTimeout(
                    () => finalizeStream("idle"),
                    IDLE_FINALIZE_MS
                  );
                }

                const pass = new ReadableStream({
                  start(controller) {
                    function pump() {
                      reader.read().then(
                        ({ done, value }) => {
                          if (done) {
                            finalizeStream("eof");
                            controller.close();
                            return;
                          }
                          try {
                            const text = decoder.decode(value, { stream: true });
                            totalBytes += value.byteLength;
                            if (!capture_off()) {
                              // Rolling truncation — drop oldest to keep
                              // chunkBytes <= ROLLING_CAP.
                              chunks.push(text);
                              chunkBytes += text.length;
                              while (
                                chunkBytes > ROLLING_CAP &&
                                chunks.length > 1
                              ) {
                                const removed = chunks.shift();
                                chunkBytes -= removed.length;
                                droppedChunks++;
                                droppedBytes += removed.length;
                              }
                              emit("__sidekar_sse", {
                                v: SCHEMA_V,
                                transport: "fetch_stream",
                                url,
                                method,
                                status,
                                chunk: text,
                                seq: chunkSeq++,
                                t: Date.now(),
                              });
                            } else {
                              chunkSeq++;
                            }
                            lastChunkAt = Date.now();
                            armIdle();
                          } catch {}
                          controller.enqueue(value);
                          pump();
                        },
                        (err) => {
                          emit("__sidekar_sse_error", {
                            v: SCHEMA_V,
                            url,
                            method,
                            err: String(err && err.message ? err.message : err),
                          });
                          finalizeStream("error");
                          controller.error(err);
                        }
                      );
                    }
                    armIdle();
                    pump();
                  },
                  cancel(reason) {
                    finalizeStream("cancel");
                    try {
                      reader.cancel(reason);
                    } catch {}
                  },
                });

                return new Response(pass, {
                  status: response.status,
                  statusText: response.statusText,
                  headers: response.headers,
                });
              } catch {
                // If anything in the tee setup fails, return original response.
                return response;
              }
            }

            // Non-SSE: clone + async text capture. Do NOT await this — the
            // page gets the original response immediately.
            try {
              const clone = response.clone();
              clone.text().then(
                (body) => {
                  emit("__sidekar_net", {
                    v: SCHEMA_V,
                    kind: "fetch",
                    phase: "done",
                    transport: "fetch",
                    captureState: "full",
                    url,
                    method,
                    status,
                    ct,
                    reqHeaders,
                    reqBody,
                    body,
                    bodyKind: "text",
                    t0,
                    t1: Date.now(),
                  });
                },
                () => {
                  emit("__sidekar_net", {
                    v: SCHEMA_V,
                    kind: "fetch",
                    phase: "done",
                    transport: "fetch",
                    captureState: "metadata_only",
                    url,
                    method,
                    status,
                    ct,
                    reqHeaders,
                    reqBody,
                    bodyKind: "opaque",
                    t0,
                    t1: Date.now(),
                  });
                }
              );
            } catch {
              emit("__sidekar_net", {
                v: SCHEMA_V,
                kind: "fetch",
                phase: "done",
                transport: "fetch",
                captureState: "metadata_only",
                url,
                method,
                status,
                ct,
                reqHeaders,
                reqBody,
                bodyKind: "opaque",
                t0,
                t1: Date.now(),
              });
            }
          } catch {}
          return response;
        },
        (err) => {
          emit("__sidekar_net", {
            v: SCHEMA_V,
            kind: "fetch",
            phase: "error",
            transport: "fetch",
            captureState: "metadata_only",
            url,
            method,
            reqHeaders,
            reqBody,
            t0,
            t1: Date.now(),
            err: String(err && err.message ? err.message : err),
          });
          throw err;
        }
      );
    };
    mirror(patched, originalFetch);
    try {
      window.fetch = patched;
    } catch {}
  }

  // --- XMLHttpRequest patch ---------------------------------------------------
  try {
    const XHR = window.XMLHttpRequest && window.XMLHttpRequest.prototype;
    if (XHR && typeof XHR.open === "function") {
      const origOpen = XHR.open;
      const origSend = XHR.send;
      const origSetHeader = XHR.setRequestHeader;

      XHR.open = function sidekar_xhr_open(method, url) {
        try {
          this.__sk_url = String(url);
          this.__sk_method = String(method || "GET");
          this.__sk_headers = {};
          this.__sk_t0 = Date.now();
        } catch {}
        return origOpen.apply(this, arguments);
      };

      XHR.setRequestHeader = function sidekar_xhr_setHeader(name, value) {
        try {
          if (this.__sk_headers) this.__sk_headers[name] = value;
        } catch {}
        return origSetHeader.apply(this, arguments);
      };

      XHR.send = function sidekar_xhr_send(body) {
        let bodyKind = "none";
        let sizeHint;
        try {
          if (body != null) {
            if (typeof body === "string") {
              bodyKind = "text";
              sizeHint = body.length;
            } else if (body instanceof Blob) {
              bodyKind = "binary";
              sizeHint = body.size;
            } else if (body instanceof ArrayBuffer || ArrayBuffer.isView(body)) {
              bodyKind = "binary";
              sizeHint = body.byteLength || undefined;
            } else if (typeof FormData !== "undefined" && body instanceof FormData) {
              bodyKind = "form";
            } else if (
              typeof URLSearchParams !== "undefined" &&
              body instanceof URLSearchParams
            ) {
              bodyKind = "text";
            } else if (body instanceof Document) {
              bodyKind = "doc";
            } else {
              bodyKind = "other";
            }
          }
        } catch {}

        const self = this;
        try {
          self.addEventListener("load", function sidekar_xhr_load() {
            try {
              let respText;
              let captured = "text";
              let captureState = "full";
              try {
                respText = self.responseText;
              } catch {
                respText = undefined;
                captured = "opaque";
                captureState = "metadata_only";
              }
              emit("__sidekar_net", {
                v: SCHEMA_V,
                kind: "xhr",
                phase: "done",
                transport: "xhr",
                captureState,
                url: self.__sk_url,
                method: self.__sk_method,
                status: self.status,
                ct: (self.getResponseHeader && self.getResponseHeader("content-type")) || "",
                reqHeaders: self.__sk_headers,
                reqBody: { kind: bodyKind, sizeHint },
                body: respText,
                bodyKind: captured,
                t0: self.__sk_t0,
                t1: Date.now(),
              });
            } catch {}
          });
          self.addEventListener("error", function sidekar_xhr_error() {
            emit("__sidekar_net", {
              v: SCHEMA_V,
              kind: "xhr",
              phase: "error",
              transport: "xhr",
              captureState: "metadata_only",
              url: self.__sk_url,
              method: self.__sk_method,
              status: self.status,
              reqHeaders: self.__sk_headers,
              reqBody: { kind: bodyKind, sizeHint },
              t0: self.__sk_t0,
              t1: Date.now(),
            });
          });
        } catch {}
        return origSend.apply(this, arguments);
      };

      mirror(XHR.open, origOpen);
      mirror(XHR.send, origSend);
      mirror(XHR.setRequestHeader, origSetHeader);
    }
  } catch {}

  // --- EventSource patch ------------------------------------------------------
  // Preserve every observable: constructor, readyState, onmessage, onerror,
  // onopen, addEventListener semantics.
  try {
    const OriginalES = window.EventSource;
    if (typeof OriginalES === "function") {
      function Wrapped(url, init) {
        const real = new OriginalES(url, init);
        const resolvedUrl = String(url);
        try {
          emit("__sidekar_sse_open", {
            v: SCHEMA_V,
            url: resolvedUrl,
            withCredentials: !!(init && init.withCredentials),
            t: Date.now(),
          });
        } catch {}

        const origAdd = real.addEventListener.bind(real);
        let seq = 0;
        real.addEventListener = function (type, listener, options) {
          if (type === "message" && typeof listener === "function") {
            const wrapped = function (ev) {
              try {
                emit("__sidekar_sse", {
                  v: SCHEMA_V,
                  transport: "eventsource",
                  url: resolvedUrl,
                  chunk: String(ev.data),
                  seq: seq++,
                  event: ev.type,
                  lastEventId: ev.lastEventId,
                  t: Date.now(),
                });
              } catch {}
              return listener.apply(this, arguments);
            };
            return origAdd(type, wrapped, options);
          }
          return origAdd(type, listener, options);
        };

        // Proxy `onmessage` setter.
        let currentOnMessage = null;
        try {
          Object.defineProperty(real, "onmessage", {
            configurable: true,
            get() {
              return currentOnMessage;
            },
            set(fn) {
              currentOnMessage = fn;
              real.addEventListener("message", fn);
            },
          });
        } catch {}

        return real;
      }
      Wrapped.CONNECTING = OriginalES.CONNECTING;
      Wrapped.OPEN = OriginalES.OPEN;
      Wrapped.CLOSED = OriginalES.CLOSED;
      Wrapped.prototype = OriginalES.prototype;
      mirror(Wrapped, OriginalES);
      try {
        window.EventSource = Wrapped;
      } catch {}
    }
  } catch {}
})();
