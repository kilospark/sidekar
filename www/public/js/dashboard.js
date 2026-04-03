(function () {
  var content = document.getElementById("content");
  var userInfo = document.getElementById("user-info");
  var logoutBtn = document.getElementById("logout-btn");

  function checkAuth() {
    return fetch("/api/auth/session")
      .then(function (res) {
        if (res.status === 401) {
          window.location.href = "/login?redirect=/dashboard";
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

  logoutBtn.addEventListener("click", function () {
    fetch("/api/auth/session", { method: "POST" }).then(function () {
      window.location.href = "/";
    });
  });

  function loadDashboard() {
    return Promise.all([
      fetch("/api/auth/devices").then(function (res) {
        if (res.status === 401) {
          window.location.href = "/login?redirect=/dashboard";
          return null;
        }
        if (!res.ok) throw new Error("devices");
        return res.json();
      }),
      fetch("/api/sessions").then(function (res) {
        if (res.status === 401) {
          window.location.href = "/login?redirect=/dashboard";
          return null;
        }
        if (!res.ok) throw new Error("sessions");
        return res.json();
      }),
    ])
      .then(function (results) {
        if (!results[0] || !results[1]) return;
        var devices = results[0].devices || [];
        var sessions = results[1].sessions || [];
        renderDashboard(devices.length, sessions.length);
      })
      .catch(function () {
        content.innerHTML =
          '<div class="empty-state"><h2>Could not load dashboard</h2><p>Try refreshing the page.</p></div>';
      });
  }

  function renderDashboard(deviceCount, sessionCount) {
    var dLabel = deviceCount === 1 ? "device" : "devices";
    var sLabel = sessionCount === 1 ? "session" : "sessions";

    var html = '<div class="dash-summary">';
    html += '<p class="dash-lead">You have <strong>' + deviceCount + "</strong> " + dLabel + " and <strong>" + sessionCount + "</strong> " + sLabel + ".</p>";
    html += '<div class="dash-grid">';

    html += '<div class="dash-card">';
    html += '<div class="dash-card-top"><span class="dash-card-title">Devices</span><span class="dash-card-num">' + deviceCount + "</span></div>";
    html += '<a class="dash-card-cta" href="/devices">View devices</a>';
    if (deviceCount === 0) {
      html +=
        '<div class="dash-hint">' +
        "<p><strong>Authorize a new device:</strong> on a machine with the sidekar CLI, run <code>sidekar login</code>, then approve the code in the browser (or open <a href=\"/approve\">sidekar.dev/approve</a> while signed in and enter the user code).</p>" +
        "</div>";
    }
    html += "</div>";

    html += '<div class="dash-card">';
    html += '<div class="dash-card-top"><span class="dash-card-title">Sessions</span><span class="dash-card-num">' + sessionCount + "</span></div>";
    html += '<a class="dash-card-cta" href="/sessions">View sessions</a>';
    if (sessionCount === 0) {
      html +=
        '<div class="dash-hint">' +
        "<p><strong>Start a session:</strong> run an agent through sidekar, for example <code>sidekar claude</code> or <code>sidekar codex</code>, with any extra arguments you need. With the relay connected, active sessions show up here.</p>" +
        "</div>";
    }
    html += "</div>";

    html += "</div></div>";
    content.innerHTML = html;
  }

  function escapeHtml(str) {
    if (!str) return "";
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  checkAuth().then(function (user) {
    if (!user) return;
    loadDashboard();
  });
})();
