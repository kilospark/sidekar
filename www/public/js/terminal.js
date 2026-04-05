(function () {
  // Extract session ID from URL: /terminal/<id>
  var parts = window.location.pathname.split("/");
  var sessionId = parts[parts.length - 1];
  if (!sessionId) {
    window.location.href = "/sessions";
    return;
  }

  var relayOrigin =
    document.body.getAttribute("data-relay-origin") || "https://relay.sidekar.dev";
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

  term.loadAddon(new WebLinksAddon.WebLinksAddon());
  term.open(document.getElementById("terminal"));

  // Mobile touch scroll fix — xterm's built-in touch handler converts pixel
  // delta to lines at ~1 line per cell height, which feels frozen on phones.
  // Intercept in the capture phase (before xterm) and scroll manually.
  (function () {
    var screen = term.element && term.element.querySelector(".xterm-screen");
    if (!screen) return;
    var touchY = null;
    var LINE_PX = 18;
    var MULTIPLIER = 6;
    var lastTap = 0;
    var moved = false;
    screen.addEventListener("touchstart", function (e) {
      if (e.touches.length === 1) {
        touchY = e.touches[0].clientY;
        moved = false;
      }
    }, { capture: true, passive: true });
    screen.addEventListener("touchmove", function (e) {
      if (touchY !== null && e.touches.length === 1) {
        var dy = touchY - e.touches[0].clientY;
        touchY = e.touches[0].clientY;
        var lines = Math.round((dy * MULTIPLIER) / LINE_PX);
        if (lines !== 0) { term.scrollLines(lines); moved = true; }
        e.preventDefault();
        e.stopPropagation();
      }
    }, { capture: true, passive: false });
    screen.addEventListener("touchend", function () {
      // Double-tap to jump to bottom
      if (!moved) {
        var now = Date.now();
        if (now - lastTap < 350) { term.scrollToBottom(); lastTap = 0; }
        else { lastTap = now; }
      }
      touchY = null;
    }, { capture: true, passive: true });
  })();

  var terminalWrap = document.getElementById("terminal-wrap");
  var remoteCols = 80;
  var remoteRows = 24;
  var layoutTimer = null;

  function syncTerminalFrame() {
    var container = document.getElementById("terminal");
    term.resize(remoteCols, remoteRows);
    requestAnimationFrame(function () {
      requestAnimationFrame(function () {
        var el = term.element;
        if (!el) return;
        var w = el.offsetWidth;
        var h = el.offsetHeight;
        if (w > 0) container.style.width = w + "px";
        if (h > 0) container.style.height = h + "px";
      });
    });
  }

  function setRemoteGeometry(cols, rows) {
    if (cols > 0) remoteCols = cols;
    if (rows > 0) remoteRows = rows;
    syncTerminalFrame();
  }

  syncTerminalFrame();

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

  // --- Initial remote scrollback + live PTY stream ---
  var sessionProtocolReady = false;
  var legacyRelay = false;
  var expectScrollbackBytes = 0;

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
          window.location.href = "/login?redirect=/terminal/" + sessionId;
          return null;
        }
        if (!res.ok) throw new Error("Auth check failed");
        return res.json();
      })
      .then(function (data) {
        if (!data) return; // redirecting

        return resolveOwnerOrigin(data.token).then(function (ownerOrigin) {
          var wsUrl = toWebSocketOrigin(ownerOrigin) + "/session/" + sessionId;
          if (data.token) {
            wsUrl += "?token=" + encodeURIComponent(data.token);
          }

          ws = new WebSocket(wsUrl);
          ws.binaryType = "arraybuffer";

          ws.onopen = function () {
            setStatus("connected", "connected");
            sessionProtocolReady = false;
            legacyRelay = false;
            expectScrollbackBytes = 0;
          };

          ws.onmessage = function (event) {
            if (typeof event.data === "string") {
              try {
                var j = JSON.parse(event.data);
                if (j.type === "session" && j.v === 1) {
                  sessionProtocolReady = true;
                  expectScrollbackBytes = j.scrollback_bytes | 0;
                  setRemoteGeometry(j.cols | 0, j.rows | 0);
                  return;
                }
                if (j.type === "pty" && j.v === 1 && j.event === "resize") {
                  setRemoteGeometry(j.cols | 0, j.rows | 0);
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

            if (expectScrollbackBytes > 0) {
              expectScrollbackBytes = 0;
              var stickScrollback = isViewportNearBottom();
              term.write(u8, function () {
                if (stickScrollback) term.scrollToBottom();
              });
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
        });
      })
      .catch(function () {
        setStatus("error", "session unavailable — retrying...");
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
    if (layoutTimer) clearTimeout(layoutTimer);
    layoutTimer = setTimeout(function () {
      layoutTimer = null;
      syncTerminalFrame();
    }, 80);
  });

  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", function () {
      if (layoutTimer) clearTimeout(layoutTimer);
      layoutTimer = setTimeout(function () {
        layoutTimer = null;
        syncTerminalFrame();
      }, 80);
    });
  }

  function toWebSocketOrigin(origin) {
    return origin.replace(/^https:\/\//, "wss://").replace(/^http:\/\//, "ws://");
  }

  function resolveOwnerOrigin(token) {
    var url =
      relayOrigin +
      "/session/" +
      encodeURIComponent(sessionId) +
      "/resolve?token=" +
      encodeURIComponent(token);
    return fetch(url)
      .then(function (res) {
        if (res.status === 401) throw new Error("unauthorized");
        if (res.status === 404) throw new Error("not found");
        if (!res.ok) throw new Error("resolve failed");
        return res.json();
      })
      .then(function (data) {
        return data.owner_origin || relayOrigin;
      });
  }

  connect();
})();
