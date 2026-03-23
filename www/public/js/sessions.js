(function () {
  var content = document.getElementById("content");
  var userInfo = document.getElementById("user-info");
  var logoutBtn = document.getElementById("logout-btn");
  var refreshNote = document.getElementById("refresh-note");
  var refreshTimer = null;

  // --- Auth check ---
  function checkAuth() {
    return fetch("/api/auth/session")
      .then(function (res) {
        if (res.status === 401) {
          window.location.href = "/api/auth/github?redirect=/sessions";
          return null;
        }
        if (!res.ok) throw new Error("Auth check failed");
        return res.json();
      })
      .then(function (data) {
        if (!data) return null;
        var user = data.user;
        userInfo.innerHTML =
          '<span class="username">' + escapeHtml(user.login || user.name || "user") + "</span>";
        return user;
      });
  }

  // --- Logout ---
  logoutBtn.addEventListener("click", function () {
    fetch("/api/auth/session", { method: "POST" }).then(function () {
      window.location.href = "/";
    });
  });

  // --- Fetch and render sessions ---
  function fetchSessions() {
    return fetch("/api/sessions")
      .then(function (res) {
        if (res.status === 401) {
          window.location.href = "/api/auth/github?redirect=/sessions";
          return null;
        }
        if (!res.ok) throw new Error("Failed to fetch sessions");
        return res.json();
      })
      .then(function (data) {
        if (!data) return;
        renderSessions(data.sessions || []);
        refreshNote.textContent = "Auto-refreshing every 10s";
      })
      .catch(function () {
        content.innerHTML =
          '<div class="empty-state"><h2>Failed to load sessions</h2><p>Please try refreshing the page.</p></div>';
      });
  }

  function renderSessions(sessions) {
    if (sessions.length === 0) {
      content.innerHTML =
        '<div class="empty-state">' +
        "<h2>No active sessions</h2>" +
        "<p>Start a session to see it here.</p>" +
        "<code>sidekar &lt;agent&gt;</code>" +
        "</div>";
      return;
    }

    var html = '<div class="sessions-grid">';
    for (var i = 0; i < sessions.length; i++) {
      var s = sessions[i];
      html += renderSessionCard(s);
    }
    html += "</div>";
    content.innerHTML = html;

    // Attach click handlers
    var cards = content.querySelectorAll(".session-card");
    for (var j = 0; j < cards.length; j++) {
      cards[j].addEventListener("click", function (e) {
        var id = this.getAttribute("data-session-id");
        if (id) window.location.href = "/terminal/" + id;
      });
    }
  }

  function renderSessionCard(s) {
    var name = escapeHtml(s.name || s.session_name || "unnamed");
    var agentType = escapeHtml(s.agent_type || "unknown");
    var hostname = escapeHtml(s.hostname || "-");
    var cwd = escapeHtml(s.cwd || "-");
    var viewers = s.viewers != null ? s.viewers : 0;
    var connectedAt = s.connected_at ? relativeTime(new Date(s.connected_at)) : "-";

    return (
      '<div class="session-card" data-session-id="' + escapeHtml(s.id) + '">' +
      '<div class="session-name">' +
      name +
      ' <span class="agent-badge">' + agentType + "</span>" +
      "</div>" +
      '<div class="session-meta">' +
      '<div class="session-meta-row"><span class="label">host</span><span class="value">' + hostname + "</span></div>" +
      '<div class="session-meta-row"><span class="label">cwd</span><span class="value">' + cwd + "</span></div>" +
      "</div>" +
      '<div class="session-footer">' +
      "<span>" + connectedAt + "</span>" +
      '<span class="viewer-count"><span class="viewer-dot"></span> ' + viewers + " viewer" + (viewers !== 1 ? "s" : "") + "</span>" +
      "</div>" +
      "</div>"
    );
  }

  // --- Helpers ---
  function relativeTime(date) {
    var now = Date.now();
    var diff = now - date.getTime();
    if (diff < 0) return "just now";
    var seconds = Math.floor(diff / 1000);
    if (seconds < 60) return seconds + "s ago";
    var minutes = Math.floor(seconds / 60);
    if (minutes < 60) return minutes + "m ago";
    var hours = Math.floor(minutes / 60);
    if (hours < 24) return hours + "h ago";
    var days = Math.floor(hours / 24);
    return days + "d ago";
  }

  function escapeHtml(str) {
    if (!str) return "";
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  // --- Init ---
  checkAuth().then(function (user) {
    if (!user) return;
    fetchSessions();
    refreshTimer = setInterval(fetchSessions, 10000);
  });
})();
