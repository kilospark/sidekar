const status = document.getElementById("status");
const extStatus = document.getElementById("ext-status");
const detailEl = document.getElementById("detail");
const loginBtn = document.getElementById("login-btn");
const logoutBtn = document.getElementById("logout-btn");
const retryBtn = document.getElementById("retry-btn");
const authSection = document.getElementById("auth-section");
const loggedInSection = document.getElementById("logged-in-section");

const hintEl = document.querySelector(".hint");

function updateHint(res) {
  if (!hintEl) return;
  // Hide hint if authenticated or if error message already explains the issue
  if (res && res.authenticated) {
    hintEl.style.display = "none";
  } else if (res && res.lastError) {
    // Error message already shown in detail area - don't duplicate
    hintEl.style.display = "none";
  } else {
    hintEl.innerHTML = "Run <code>sidekar login</code>";
    hintEl.style.display = "block";
  }
}

function applyStatus(res) {
  updateHint(res);
  if (res && res.authenticated) {
    status.textContent = "Connected & authenticated";
    status.className = "connected";
    detailEl.textContent = "";
    retryBtn.style.display = "none";
    return;
  }
  if (res && res.connected) {
    status.textContent = "Connected, waiting for auth...";
    status.className = "pending";
    detailEl.textContent = "";
    retryBtn.style.display = "none";
    return;
  }
  status.textContent = "Not connected";
  status.className = "disconnected";
  
  // Check if error is about needing to run sidekar login
  const needsLogin = res && res.lastError && (
    res.lastError.includes("login") || 
    res.lastError.includes("token")
  );
  
  if (needsLogin) {
    detailEl.style.color = "#666";
    detailEl.innerHTML = "Run ";
    const cmdCopy = document.createElement("span");
    cmdCopy.className = "cmd-copy";
    cmdCopy.innerHTML = `sidekar login <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
    cmdCopy.onclick = () => {
      navigator.clipboard.writeText("sidekar login").then(() => {
        cmdCopy.classList.add("copied");
        cmdCopy.innerHTML = `sidekar login <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="20 6 9 17 4 12"/></svg>`;
        setTimeout(() => {
          cmdCopy.classList.remove("copied");
          cmdCopy.innerHTML = `sidekar login <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>`;
        }, 2000);
      });
    };
    detailEl.appendChild(cmdCopy);
  } else {
    detailEl.style.color = "#991b1b";
    detailEl.textContent = res && res.lastError ? res.lastError : "";
  }
  
  // Only show retry for connection errors, not auth/login issues
  const isAuthIssue = res && res.lastError && (
    res.lastError.includes("login") || 
    res.lastError.includes("token") ||
    res.lastError.includes("Auth")
  );
  retryBtn.style.display = (res && res.lastError && !isAuthIssue) ? "block" : "none";
}

function refreshStatus() {
  chrome.runtime.sendMessage({ type: "status" }, (res) => {
    if (chrome.runtime.lastError) {
      detailEl.textContent = chrome.runtime.lastError.message || "";
      updateHint({ cliLoggedIn: false, authenticated: false });
      return;
    }
    applyStatus(res);
  });
}

function updateAuthUI() {
  chrome.storage.local.get(["extToken"], (data) => {
    if (data.extToken) {
      authSection.style.display = "none";
      loggedInSection.style.display = "block";
      extStatus.textContent = "Signed in";
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

// --- Retry connection ---

retryBtn.addEventListener("click", () => {
  retryBtn.disabled = true;
  retryBtn.textContent = "Retrying...";
  chrome.runtime.sendMessage({ type: "reconnect" }, () => {
    setTimeout(() => {
      retryBtn.disabled = false;
      retryBtn.textContent = "Retry connection";
      refreshStatus();
    }, 1000);
  });
});

// --- GitHub OAuth Login ---

loginBtn.addEventListener("click", () => {
  // Delegate to background script (survives popup closing)
  chrome.runtime.sendMessage({ type: "startOAuth" });
});

// --- Logout ---

logoutBtn.addEventListener("click", () => {
  chrome.storage.local.remove(["extToken"], () => {
    chrome.runtime.sendMessage({ type: "reconnect" }, () => {
      updateAuthUI();
      refreshStatus();
    });
  });
});
