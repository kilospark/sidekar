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
  term.loadAddon(new WebLinksAddon.WebLinksAddon());
  term.open(document.getElementById("terminal"));

  var LAYOUT_KEY = "terminalLayoutAdaptive";
  var FIXED_COLS = 80;
  var FIXED_ROWS = 24;

  var layoutAdaptiveCheckbox = document.getElementById("layout-adaptive");
  var terminalWrap = document.getElementById("terminal-wrap");

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
    fitAddon.fit();
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
      fitTerminalAdaptive();
    } else {
      document.body.classList.add("terminal-layout-fixed");
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
        };

        ws.onmessage = function (event) {
          if (typeof event.data === "string") return;
          // Only follow new output if the user was already at the bottom; otherwise
          // scrolling up to read history would be undone on every PTY chunk.
          var stickToBottom = isViewportNearBottom();
          term.write(new Uint8Array(event.data), function () {
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
      fitTerminalAdaptive();
    } else {
      applyFixedLayout();
    }
  });

  connect();
})();
