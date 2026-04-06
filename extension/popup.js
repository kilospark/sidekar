const status = document.getElementById("status");
const extStatus = document.getElementById("ext-status");
const detailEl = document.getElementById("detail");
const loginBtn = document.getElementById("login-btn");
const logoutBtn = document.getElementById("logout-btn");
const retryTimer = document.getElementById("retry-timer");
const retryCountdown = document.getElementById("retry-countdown");
let countdownInterval = null;
const authSection = document.getElementById("auth-section");
const loggedInSection = document.getElementById("logged-in-section");

function loginCommandMarkup(copied = false) {
  if (copied) {
    return `sidekar device login <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"/></svg>`;
  }
  return `sidekar device login <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
}

function createCopyCommand() {
  const cmdCopy = document.createElement("span");
  cmdCopy.className = "cmd-copy";
  cmdCopy.innerHTML = loginCommandMarkup(false);
  cmdCopy.onclick = () => {
    navigator.clipboard.writeText("sidekar device login").then(() => {
      cmdCopy.classList.add("copied");
      cmdCopy.innerHTML = loginCommandMarkup(true);
      setTimeout(() => {
        cmdCopy.classList.remove("copied");
        cmdCopy.innerHTML = loginCommandMarkup(false);
      }, 2000);
    });
  };
  return cmdCopy;
}

function renderCliLoginAction() {
  status.className = "action-status";
  status.textContent = "";

  const wrapper = document.createElement("span");
  wrapper.className = "status-copy";

  const prefix = document.createElement("span");
  prefix.className = "status-prefix";
  prefix.textContent = "Run";

  wrapper.appendChild(prefix);
  wrapper.appendChild(createCopyCommand());
  status.appendChild(wrapper);
}

function stopCountdown() {
  if (countdownInterval) {
    clearInterval(countdownInterval);
    countdownInterval = null;
  }
  retryTimer.style.display = "none";
}

function startCountdown() {
  stopCountdown();
  let secs = 3;
  retryCountdown.textContent = secs;
  retryTimer.style.display = "block";
  countdownInterval = setInterval(() => {
    secs--;
    if (secs <= 0) {
      stopCountdown();
    } else {
      retryCountdown.textContent = secs;
    }
  }, 1000);
}

function applyStatus(res) {
  if (res && res.authenticated) {
    status.textContent = "Connected & authenticated";
    status.className = "connected";
    detailEl.textContent = "";
    stopCountdown();
    return;
  }
  if (res && res.connected) {
    status.textContent = "Bridge connected, waiting for extension auth...";
    status.className = "pending";
    detailEl.textContent = "";
    stopCountdown();
    return;
  }
  if (res && res.cliLoggedIn) {
    status.textContent = "CLI authenticated, connecting...";
    status.className = "pending";
  } else {
    renderCliLoginAction();
  }

  // Check if error is about needing to run sidekar device login
  const needsLogin = res && res.lastError && (
    res.lastError.includes("login") ||
    res.lastError.includes("token")
  );

  if (needsLogin) {
    detailEl.style.color = "#666";
    detailEl.textContent = "";
    stopCountdown();
  } else {
    detailEl.style.color = "#991b1b";
    detailEl.textContent = res && res.lastError ? res.lastError : "";
    // Show retry countdown for connection errors (not auth issues)
    const isAuthIssue = res && res.lastError && (
      res.lastError.includes("login") ||
      res.lastError.includes("token") ||
      res.lastError.includes("Auth")
    );
    if (res && res.lastError && !isAuthIssue && !countdownInterval) {
      startCountdown();
    } else if (!res || !res.lastError || isAuthIssue) {
      stopCountdown();
    }
  }
}

function refreshStatus() {
  chrome.runtime.sendMessage({ type: "status" }, (res) => {
    if (chrome.runtime.lastError) {
      detailEl.textContent = chrome.runtime.lastError.message || "";
      renderCliLoginAction();
      return;
    }
    applyStatus(res);
  });
}

function updateAuthUI() {
  chrome.storage.local.get(["extToken", "extProfile"], (data) => {
    if (data.extToken) {
      authSection.style.display = "none";
      loggedInSection.style.display = "block";
      const p = data.extProfile;
      const label = p && (p.login || p.email) ? `Signed in as ${p.login || p.email}` : "Signed in";
      extStatus.textContent = label;
      extStatus.className = "connected";
    } else {
      authSection.style.display = "block";
      loggedInSection.style.display = "none";
      extStatus.textContent = "Not signed in";
      extStatus.className = "disconnected";
    }
  });
}

refreshStatus();
updateAuthUI();

// Show extension version
document.getElementById("version").textContent = chrome.runtime.getManifest().version;

// Auto-refresh status every 2 seconds while popup is open
setInterval(refreshStatus, 2000);

// Listen for storage changes to update UI when token is set by background
chrome.storage.onChanged.addListener((changes) => {
  if (changes.extToken) {
    updateAuthUI();
    refreshStatus();
  }
});

// --- OAuth Login ---

loginBtn.addEventListener("click", () => {
  // Delegate to background script (survives popup closing)
  chrome.runtime.sendMessage({ type: "startOAuth" });
});

// --- Logout ---

logoutBtn.addEventListener("click", () => {
  chrome.storage.local.remove(["extToken", "extProfile"], () => {
    chrome.runtime.sendMessage({ type: "reconnect" }, () => {
      updateAuthUI();
      refreshStatus();
    });
  });
});
