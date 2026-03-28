const status = document.getElementById("status");
const detailEl = document.getElementById("detail");
const portInput = document.getElementById("port");
const endpointEl = document.getElementById("endpoint");
const loginBtn = document.getElementById("login-btn");
const logoutBtn = document.getElementById("logout-btn");
const authSection = document.getElementById("auth-section");
const loggedInSection = document.getElementById("logged-in-section");
const userInfoEl = document.getElementById("user-info");
const secretInput = document.getElementById("secret");
const saveSecretBtn = document.getElementById("save-secret");
const savePortBtn = document.getElementById("save-port");

const DEFAULT_EXT_PORT = 9876;
const CALLBACK_URL = "https://sidekar.dev/ext-callback";

function applyStatus(res) {
  if (res && res.wsUrl) {
    endpointEl.textContent = `Endpoint: ${res.wsUrl}`;
  }
  if (res && res.authenticated) {
    status.textContent = "Connected & authenticated";
    status.className = "connected";
    detailEl.textContent = "";
    return;
  }
  if (res && res.connected) {
    status.textContent = "Connected, waiting for auth...";
    status.className = "pending";
    detailEl.textContent = "";
    return;
  }
  status.textContent = "Not connected";
  status.className = "disconnected";
  detailEl.textContent = res && res.lastError ? res.lastError : "";
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
  chrome.storage.local.get(["extToken", "secret"], (data) => {
    if (data.extToken) {
      authSection.style.display = "none";
      loggedInSection.style.display = "block";
      userInfoEl.textContent = "Authenticated via GitHub";
    } else if (data.secret) {
      authSection.style.display = "none";
      loggedInSection.style.display = "block";
      userInfoEl.textContent = "Authenticated via shared secret";
    } else {
      authSection.style.display = "block";
      loggedInSection.style.display = "none";
    }
  });
}

// Load saved port
chrome.storage.local.get(["extPort"], (data) => {
  const p = data.extPort;
  if (typeof p === "number" && p >= 1 && p <= 65535) {
    portInput.value = String(p);
  } else if (typeof p === "string" && /^\d+$/.test(p)) {
    portInput.value = p;
  }
});

refreshStatus();
updateAuthUI();

// --- GitHub OAuth Login ---

loginBtn.addEventListener("click", () => {
  const authUrl = "https://sidekar.dev/api/auth/github?redirect=/ext-callback";

  // Open the OAuth flow in a new tab
  chrome.tabs.create({ url: authUrl, active: true }, (tab) => {
    const authTabId = tab.id;

    // Listen for the callback tab to finish loading
    function onUpdated(tabId, changeInfo, updatedTab) {
      if (tabId !== authTabId) return;
      if (changeInfo.status !== "complete") return;
      if (!updatedTab.url || !updatedTab.url.startsWith(CALLBACK_URL)) return;

      // The ext-callback page has loaded — inject a script to read the token
      chrome.scripting.executeScript(
        {
          target: { tabId: authTabId },
          func: () => {
            const el = document.getElementById("ext-token");
            return el ? el.getAttribute("data-token") : null;
          },
        },
        (results) => {
          chrome.tabs.onUpdated.removeListener(onUpdated);
          chrome.tabs.onRemoved.removeListener(onRemoved);

          if (chrome.runtime.lastError) {
            detailEl.textContent = "Could not read token from callback page.";
            return;
          }

          const token =
            results && results[0] && results[0].result
              ? results[0].result
              : null;

          if (token) {
            // Store the token and reconnect
            chrome.runtime.sendMessage(
              { type: "setToken", extToken: token, extPort: getPort() },
              () => {
                if (chrome.runtime.lastError) {
                  detailEl.textContent =
                    chrome.runtime.lastError.message || "";
                  return;
                }
                updateAuthUI();
                const delays = [400, 1200, 2500, 5000];
                delays.forEach((ms) => setTimeout(refreshStatus, ms));
              }
            );

            // Close the callback tab
            chrome.tabs.remove(authTabId).catch(() => {});
          } else {
            detailEl.textContent =
              "Token not found on callback page. Check if you are logged in to sidekar.dev.";
          }
        }
      );
    }

    function onRemoved(tabId) {
      if (tabId !== authTabId) return;
      chrome.tabs.onUpdated.removeListener(onUpdated);
      chrome.tabs.onRemoved.removeListener(onRemoved);
    }

    chrome.tabs.onUpdated.addListener(onUpdated);
    chrome.tabs.onRemoved.addListener(onRemoved);
  });
});

// --- Logout ---

logoutBtn.addEventListener("click", () => {
  chrome.storage.local.remove(["extToken", "secret"], () => {
    chrome.runtime.sendMessage({ type: "reconnect" }, () => {
      updateAuthUI();
      refreshStatus();
    });
  });
});

// --- Manual secret (backwards compat) ---

saveSecretBtn.addEventListener("click", () => {
  const secret = secretInput.value.trim();
  if (!secret) return;

  status.textContent = "Connecting...";
  status.className = "pending";
  detailEl.textContent = "";

  chrome.runtime.sendMessage(
    { type: "setSecret", secret, extPort: getPort() },
    () => {
      if (chrome.runtime.lastError) {
        detailEl.textContent = chrome.runtime.lastError.message || "";
        status.textContent = "Not connected";
        status.className = "disconnected";
        return;
      }
      updateAuthUI();
      const delays = [400, 1200, 2500, 5000];
      delays.forEach((ms) => setTimeout(refreshStatus, ms));
    }
  );
});

// --- Port update ---

savePortBtn.addEventListener("click", () => {
  const extPort = getPort();
  chrome.storage.local.set({ extPort }, () => {
    chrome.runtime.sendMessage({ type: "reconnect" }, () => {
      const delays = [400, 1200, 2500];
      delays.forEach((ms) => setTimeout(refreshStatus, ms));
    });
  });
});

function getPort() {
  const rawPort = portInput.value.trim();
  const parsed = parseInt(rawPort, 10);
  return rawPort === "" || !Number.isFinite(parsed) ? DEFAULT_EXT_PORT : parsed;
}
