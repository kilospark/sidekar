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

  // Create terminal
  var term = new Terminal({
    cursorBlink: true,
    fontSize: 14,
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
  term.loadAddon(new WebLinksAddon.WebLinksAddon());
  term.open(document.getElementById("terminal"));

  // Account for status bar height (32px)
  function fitTerminal() {
    var container = document.getElementById("terminal");
    container.style.height = "calc(100vh - 32px)";
    container.style.marginTop = "32px";
    fitAddon.fit();
  }

  fitTerminal();

  function setStatus(state, text) {
    statusDot.className = state;
    statusText.textContent = text;
  }

  function connect() {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }

    setStatus("", "connecting...");

    // Fetch /api/auth/session to verify we are authenticated before connecting
    fetch("/api/auth/session")
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

        var token = getCookie("sidekar_session");
        var wsUrl = "wss://" + relayHost + "/session/" + sessionId;
        if (token) {
          wsUrl += "?token=" + encodeURIComponent(token);
        }

        ws = new WebSocket(wsUrl);
        ws.binaryType = "arraybuffer";

        ws.onopen = function () {
          setStatus("connected", "connected");
          // Send initial terminal size
          ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
        };

        ws.onmessage = function (event) {
          if (typeof event.data === "string") {
            // Control message — ignore for now
            return;
          }
          term.write(new Uint8Array(event.data));
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

  // Send resize events on window resize
  window.addEventListener("resize", function () {
    fitTerminal();
    if (ws && ws.readyState === WebSocket.OPEN) {
      ws.send(JSON.stringify({ type: "resize", cols: term.cols, rows: term.rows }));
    }
  });

  connect();
})();
