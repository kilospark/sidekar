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
    screen.addEventListener("touchstart", function (e) {
      if (e.touches.length === 1) touchY = e.touches[0].clientY;
    }, { capture: true, passive: true });
    screen.addEventListener("touchmove", function (e) {
      if (touchY !== null && e.touches.length === 1) {
        var dy = touchY - e.touches[0].clientY;
        touchY = e.touches[0].clientY;
        var lines = Math.round((dy * MULTIPLIER) / LINE_PX);
        if (lines !== 0) term.scrollLines(lines);
        e.preventDefault();
        e.stopPropagation();
      }
    }, { capture: true, passive: false });
    screen.addEventListener("touchend", function () {
      touchY = null;
    }, { capture: true, passive: true });
  })();

  var terminalWrap = document.getElementById("terminal-wrap");
  var jumpBtn = document.getElementById("jump-bottom");
  var remoteCols = 80;
  var remoteRows = 24;
  var layoutTimer = null;

  // Scroll both xterm's scrollback AND the outer wrap container to bottom.
  function jumpToBottom() {
    term.scrollToBottom();
    terminalWrap.scrollTop = terminalWrap.scrollHeight;
  }

  jumpBtn.addEventListener("click", function () {
    jumpToBottom();
    jumpBtn.style.display = "none";
  });

  // Show/hide the jump button based on xterm scroll position.
  function updateJumpButton() {
    var vp = term.element && term.element.querySelector(".xterm-viewport");
    if (!vp) return;
    var nearBottom = vp.scrollHeight - vp.scrollTop - vp.clientHeight <= 48;
    jumpBtn.style.display = nearBottom ? "none" : "block";
  }

  // Listen for xterm scroll events to toggle the button.
  term.onScroll(updateJumpButton);

  function syncTerminalFrame() {
    term.resize(remoteCols, remoteRows);
  }

  function setRemoteGeometry(cols, rows) {
    if (cols > 0) remoteCols = cols;
    if (rows > 0) remoteRows = rows;
    syncTerminalFrame();
  }

  syncTerminalFrame();

  function isNearBottom() {
    var vp = term.element && term.element.querySelector(".xterm-viewport");
    if (!vp) return true;
    return vp.scrollHeight - vp.scrollTop - vp.clientHeight <= 48;
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
            var stick = isNearBottom();

            term.write(u8, function () {
              if (stick) jumpToBottom();
              updateJumpButton();
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

  // iOS keyboard: visualViewport shrinks but fixed elements don't.
  // Resize terminal-wrap to fit above the keyboard and jump to bottom.
  if (window.visualViewport) {
    window.visualViewport.addEventListener("resize", function () {
      var vv = window.visualViewport;
      terminalWrap.style.height = (vv.height - 32) + "px";
      jumpToBottom();
    });
    window.visualViewport.addEventListener("scroll", function () {
      var vv = window.visualViewport;
      terminalWrap.style.height = (vv.height - 32) + "px";
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
