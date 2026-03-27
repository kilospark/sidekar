(function () {
  // Extract session ID from URL: /terminal/<id>
  var parts = window.location.pathname.split("/");
  var sessionId = parts[parts.length - 1];
  if (!sessionId) {
    window.location.href = "/sessions";
    return;
  }

  var relayHost = document.body.getAttribute("data-relay-host") || "relay.sidekar.dev";
  var statusDot = document.getElementById("status-dot");
  var statusText = document.getElementById("status-text");
  var ws = null;
  var reconnectTimer = null;

  // Read sidekar_session cookie value for cross-origin auth
  function getCookie(name) {
    var match = document.cookie.match(new RegExp("(?:^|; )" + name + "=([^;]*)"));
    return match ? match[1] : null;
  }

  // Create terminal (scrollback: lines kept above viewport; sticky scroll in onmessage)
  var term = new Terminal({
    cursorBlink: true,
    fontSize: 14,
    scrollback: 10000,
    fontFamily: "'SF Mono', Menlo, Consolas, monospace",
    theme: {
      background: "#09090b",
      foreground: "#fafafa",
      cursor: "#fafafa",
      selectionBackground: "#3f3f46",
    },
  });

  var fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  // UMD build exports the class as SerializeAddon.SerializeAddon (same pattern as WebLinksAddon).
  var serializeAddon = new SerializeAddon.SerializeAddon();
  term.loadAddon(serializeAddon);
  term.loadAddon(new WebLinksAddon.WebLinksAddon());
  term.open(document.getElementById("terminal"));

  var LAYOUT_KEY = "terminalLayoutAdaptive";
  var FIXED_COLS = 80;
  var FIXED_ROWS = 24;

  var layoutAdaptiveCheckbox = document.getElementById("layout-adaptive");
  var terminalWrap = document.getElementById("terminal-wrap");
  /** Debounce viewport-driven fits; rapid resize + fit() caused flicker / duplicated lines. */
  var fitDebounceTimer = null;
  /** Skip fit when container dimensions are unchanged (resize storms). */
  var lastAdaptiveFitKey = "";

  function loadLayoutPreference() {
    var stored = localStorage.getItem(LAYOUT_KEY);
    if (stored === "0") {
      layoutAdaptiveCheckbox.checked = false;
    } else if (stored === "1") {
      layoutAdaptiveCheckbox.checked = true;
    }
  }

  function fitTerminalAdaptive() {
    var container = document.getElementById("terminal");
    container.style.height = "calc(100vh - 32px)";
    container.style.width = "100%";
    var w = container.clientWidth;
    var h = container.clientHeight;
    if (w <= 0 || h <= 0) return;
    var key = w + "x" + h;
    if (key === lastAdaptiveFitKey) return;
    lastAdaptiveFitKey = key;
    fitAddon.fit();
  }

  function scheduleAdaptiveFit() {
    if (fitDebounceTimer) clearTimeout(fitDebounceTimer);
    fitDebounceTimer = setTimeout(function () {
      fitDebounceTimer = null;
      if (!layoutAdaptiveCheckbox.checked) return;
      lastAdaptiveFitKey = "";
      fitTerminalAdaptive();
    }, 120);
  }

  function applyFixedLayout() {
    var container = document.getElementById("terminal");
    terminalWrap.className = "layout-fixed";
    container.style.width = "";
    container.style.height = "";
    term.resize(FIXED_COLS, FIXED_ROWS);
    requestAnimationFrame(function () {
      requestAnimationFrame(function () {
        var el = term.element;
        if (!el) return;
        var w = el.offsetWidth;
        var h = el.offsetHeight;
        if (w > 0 && h > 0) {
          container.style.width = w + "px";
          container.style.height = h + "px";
        }
      });
    });
  }

  function applyLayout() {
    var adaptive = layoutAdaptiveCheckbox.checked;
    localStorage.setItem(LAYOUT_KEY, adaptive ? "1" : "0");
    var container = document.getElementById("terminal");
    if (adaptive) {
      document.body.classList.remove("terminal-layout-fixed");
      terminalWrap.className = "layout-adaptive";
      container.style.width = "";
      container.style.height = "";
      lastAdaptiveFitKey = "";
      // Let CSS settle before measuring; avoids fit/resize feedback with the checkbox toggle.
      requestAnimationFrame(function () {
        requestAnimationFrame(function () {
          fitTerminalAdaptive();
        });
      });
    } else {
      document.body.classList.add("terminal-layout-fixed");
      lastAdaptiveFitKey = "";
      applyFixedLayout();
    }
  }

  loadLayoutPreference();
  layoutAdaptiveCheckbox.addEventListener("change", applyLayout);
  applyLayout();

  function isViewportNearBottom() {
    var vp = term.element && term.element.querySelector(".xterm-viewport");
    if (!vp) return true;
    var threshold = 48;
    return vp.scrollHeight - vp.scrollTop - vp.clientHeight <= threshold;
  }

  function setStatus(state, text) {
    statusDot.className = state;
    statusText.textContent = text;
  }

  // --- Relay replay buffer: defer initial snapshot; merge on scroll-up or on-demand ---
  var sessionProtocolReady = false;
  var legacyRelay = false;
  var expectReplayBytes = 0;
  var expectHistoryBytes = 0;
  var pendingReplay = null;
  var initialReplayMerged = false;
  var historyFetchInFlight = false;
  var viewportScrollAttached = false;

  function injectReplayBeforeCurrentBuffer(replayBytes) {
    if (!replayBytes || replayBytes.length === 0) return;
    // Serialize entire buffer (active + scrollback); omit options = full buffer.
    var live = serializeAddon.serialize();
    term.reset();
    term.write(replayBytes);
    term.write(live);
  }

  function tryFetchHistoryOnScroll() {
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    if (historyFetchInFlight) return;

    if (pendingReplay && pendingReplay.length > 0 && !initialReplayMerged) {
      injectReplayBeforeCurrentBuffer(pendingReplay);
      pendingReplay = null;
      initialReplayMerged = true;
      return;
    }

    historyFetchInFlight = true;
    ws.send(JSON.stringify({ type: "history", v: 1 }));
  }

  function attachViewportScrollListener() {
    if (viewportScrollAttached) return;
    var vp = term.element && term.element.querySelector(".xterm-viewport");
    if (!vp) return;
    viewportScrollAttached = true;
    var scrollTimer = null;
    vp.addEventListener("scroll", function () {
      if (scrollTimer) clearTimeout(scrollTimer);
      scrollTimer = setTimeout(function () {
        if (vp.scrollTop < 48) tryFetchHistoryOnScroll();
      }, 100);
    });
  }

  function connect() {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }

    setStatus("", "connecting...");

    // Fetch JWT token for cross-origin WebSocket auth (cookie is HttpOnly)
    fetch("/api/auth/session?ws=1")
      .then(function (res) {
        if (res.status === 401) {
          window.location.href = "/api/auth/github?redirect=/terminal/" + sessionId;
          return null;
        }
        if (!res.ok) throw new Error("Auth check failed");
        return res.json();
      })
      .then(function (data) {
        if (!data) return; // redirecting

        var wsUrl = "wss://" + relayHost + "/session/" + sessionId;
        if (data.token) {
          wsUrl += "?token=" + encodeURIComponent(data.token);
        }

        ws = new WebSocket(wsUrl);
        ws.binaryType = "arraybuffer";

        ws.onopen = function () {
          setStatus("connected", "connected");
          sessionProtocolReady = false;
          legacyRelay = false;
          expectReplayBytes = 0;
          expectHistoryBytes = 0;
          pendingReplay = null;
          initialReplayMerged = false;
          historyFetchInFlight = false;
          attachViewportScrollListener();
        };

        ws.onmessage = function (event) {
          if (typeof event.data === "string") {
            try {
              var j = JSON.parse(event.data);
              if (j.type === "session" && j.v === 1) {
                sessionProtocolReady = true;
                expectReplayBytes = j.replay_len | 0;
                return;
              }
              if (j.type === "history" && j.v === 1) {
                if (j.empty) {
                  historyFetchInFlight = false;
                  return;
                }
                expectHistoryBytes = j.bytes | 0;
                return;
              }
            } catch (e) {}
            return;
          }

          var u8 = new Uint8Array(event.data);

          if (!sessionProtocolReady && !legacyRelay) {
            legacyRelay = true;
            sessionProtocolReady = true;
            var stickLegacy = isViewportNearBottom();
            term.write(u8, function () {
              if (stickLegacy) term.scrollToBottom();
            });
            return;
          }

          // Replay snapshot: show immediately. (Deferring only on scroll left a quiet PTY looking "blank".)
          if (expectReplayBytes > 0) {
            expectReplayBytes = 0;
            pendingReplay = null;
            initialReplayMerged = true;
            var stickReplay = isViewportNearBottom();
            term.write(u8, function () {
              if (stickReplay) term.scrollToBottom();
            });
            return;
          }

          if (expectHistoryBytes > 0) {
            expectHistoryBytes = 0;
            historyFetchInFlight = false;
            injectReplayBeforeCurrentBuffer(u8);
            return;
          }

          var stickToBottom = isViewportNearBottom();
          term.write(u8, function () {
            if (stickToBottom) term.scrollToBottom();
          });
        };

        ws.onclose = function () {
          setStatus("error", "disconnected — reconnecting...");
          scheduleReconnect();
        };

        ws.onerror = function () {
          if (ws) ws.close();
        };
      })
      .catch(function () {
        setStatus("error", "auth failed — retrying...");
        scheduleReconnect();
      });
  }

  function scheduleReconnect() {
    if (!reconnectTimer) {
      reconnectTimer = setTimeout(connect, 2000);
    }
  }

  // Send user input to relay
  term.onData(function (data) {
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(new TextEncoder().encode(data));
    }
  });

  window.addEventListener("resize", function () {
    if (layoutAdaptiveCheckbox.checked) {
      scheduleAdaptiveFit();
    } else {
      applyFixedLayout();
    }
  });

  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", function () {
      if (layoutAdaptiveCheckbox.checked) scheduleAdaptiveFit();
    });
  }

  connect();
})();
