(function () {
  var content = document.getElementById("content");
  var userInfo = document.getElementById("user-info");
  var logoutBtn = document.getElementById("logout-btn");

  function checkAuth() {
    return fetch("/api/auth/session")
      .then(function (res) {
        if (res.status === 401) {
          window.location.href = "/login?redirect=/devices";
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

  function fetchDevices() {
    return fetch("/api/auth/devices")
      .then(function (res) {
        if (res.status === 401) {
          window.location.href = "/login?redirect=/devices";
          return null;
        }
        if (!res.ok) throw new Error("Failed to fetch devices");
        return res.json();
      })
      .then(function (data) {
        if (!data) return;
        renderDevices(data.devices || []);
      })
      .catch(function () {
        content.innerHTML =
          '<div class="empty-state"><h2>Failed to load devices</h2><p>Please try refreshing the page.</p></div>';
      });
  }

  function renderDevices(devices) {
    if (devices.length === 0) {
      content.innerHTML =
        '<div class="empty-state">' +
        "<h2>No authorized devices</h2>" +
        "<p>Run <code>sidekar login</code> on a machine and approve it here to list CLI sessions and tunnels under your account.</p>" +
        "</div>";
      return;
    }

    var html = '<div class="devices-grid">';
    for (var i = 0; i < devices.length; i++) {
      html += renderDeviceCard(devices[i]);
    }
    html += "</div>";
    content.innerHTML = html;

    var buttons = content.querySelectorAll(".btn-revoke");
    for (var j = 0; j < buttons.length; j++) {
      buttons[j].addEventListener("click", function (e) {
        e.stopPropagation();
        e.preventDefault();
        var id = this.getAttribute("data-device-id");
        if (!id) return;
        var btn = this;
        sidekarConfirm({
          title: "Revoke device?",
          message:
            "The CLI on that machine will need to run sidekar login again.",
          confirmLabel: "Revoke",
          cancelLabel: "Cancel",
          danger: true,
        }).then(function (ok) {
          if (!ok) return;
          btn.disabled = true;
          fetch("/api/auth/devices?id=" + encodeURIComponent(id), { method: "DELETE" })
            .then(function (res) {
              if (!res.ok) throw new Error("revoke failed");
              fetchDevices();
            })
            .catch(function () {
              btn.disabled = false;
              sidekarAlert({
                title: "Could not revoke",
                message: "Try again in a moment.",
              });
            });
        });
      });
    }
  }

  function renderDeviceCard(d) {
    var host = escapeHtml(d.hostname || "unknown");
    var os = escapeHtml(d.os || "-");
    var arch = escapeHtml(d.arch || "-");
    var ver = escapeHtml(d.sidekar_version || "-");
    var last = d.last_seen_at ? formatDate(new Date(d.last_seen_at)) : "-";
    var lastAgo = d.last_seen_at ? relativeTime(new Date(d.last_seen_at)) : "";
    var lastDisplay = last;
    if (last !== "-" && lastAgo) {
      lastDisplay = last + " (" + lastAgo + ")";
    }
    var created = d.created_at ? formatDate(new Date(d.created_at)) : "-";

    return (
      '<div class="device-card">' +
      '<div class="device-card-top">' +
      '<div class="device-name">' + host + "</div>" +
      '<button type="button" class="btn-revoke" data-device-id="' +
      escapeHtml(d.id) +
      '">Revoke</button>' +
      "</div>" +
      '<div class="device-meta">' +
      '<div class="device-meta-row"><span class="label">OS</span><span class="value">' + os + "</span></div>" +
      '<div class="device-meta-row"><span class="label">Arch</span><span class="value">' + arch + "</span></div>" +
      '<div class="device-meta-row"><span class="label">sidekar</span><span class="value">' + ver + "</span></div>" +
      '<div class="device-meta-row"><span class="label">Last seen</span><span class="value">' + escapeHtml(lastDisplay) + "</span></div>" +
      '<div class="device-meta-row"><span class="label">Authorized</span><span class="value">' + created + "</span></div>" +
      "</div>" +
      "</div>"
    );
  }

  function formatDate(d) {
    if (isNaN(d.getTime())) return "-";
    return d.toLocaleString(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    });
  }

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

  checkAuth().then(function (user) {
    if (!user) return;
    fetchDevices();
  });
})();
