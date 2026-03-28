const status = document.getElementById("status");
const detailEl = document.getElementById("detail");
const loginBtn = document.getElementById("login-btn");
const logoutBtn = document.getElementById("logout-btn");
const retryBtn = document.getElementById("retry-btn");
const authSection = document.getElementById("auth-section");
const loggedInSection = document.getElementById("logged-in-section");

const hintEl = document.querySelector(".hint");

function updateHint(res) {
  if (!hintEl) return;
  if (res && res.authenticated) {
    hintEl.style.display = "none";
  } else if (res && res.cliLoggedIn) {
    hintEl.innerHTML = "CLI ready. Log in above to connect.";
    hintEl.style.display = "block";
  } else {
    hintEl.innerHTML = "Run <code>sidekar login</code> in terminal first";
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
  detailEl.textContent = res && res.lastError ? res.lastError : "";
  retryBtn.style.display = "block";
}

function refreshStatus() {
  chrome.runtime.sendMessage({ type: "status" }, (res) => {
    if (chrome.runtime.lastError) {
      detailEl.textContent = chrome.runtime.lastError.message || "";
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
    } else {
      authSection.style.display = "block";
      loggedInSection.style.display = "none";
    }
  });
}

refreshStatus();
updateAuthUI();

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
