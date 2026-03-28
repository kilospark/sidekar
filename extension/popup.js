const status = document.getElementById("status");
const detailEl = document.getElementById("detail");
const loginBtn = document.getElementById("login-btn");
const logoutBtn = document.getElementById("logout-btn");
const authSection = document.getElementById("auth-section");
const loggedInSection = document.getElementById("logged-in-section");

const CALLBACK_URL = "https://sidekar.dev/ext-callback";

function applyStatus(res) {
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

      chrome.tabs.onUpdated.removeListener(onUpdated);
      chrome.tabs.onRemoved.removeListener(onRemoved);

      // The page loads async, so poll for the token
      let attempts = 0;
      const maxAttempts = 20;

      function tryReadToken() {
        chrome.scripting.executeScript(
          {
            target: { tabId: authTabId },
            func: () => {
              const el = document.getElementById("ext-token");
              return el ? el.getAttribute("data-token") : null;
            },
          },
          (results) => {
            if (chrome.runtime.lastError) {
              detailEl.textContent = "Could not read token from callback page.";
              return;
            }

            const token =
              results && results[0] && results[0].result
                ? results[0].result
                : null;

            if (token && token.length > 0) {
              // Store the token and reconnect
              chrome.runtime.sendMessage(
                { type: "setToken", extToken: token },
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
            } else if (++attempts < maxAttempts) {
              // Token not ready yet, retry
              setTimeout(tryReadToken, 250);
            } else {
              detailEl.textContent =
                "Token not found on callback page. Check if you are logged in to sidekar.dev.";
            }
          }
        );
      }

      // Start polling after a brief delay
      setTimeout(tryReadToken, 300);
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
  chrome.storage.local.remove(["extToken"], () => {
    chrome.runtime.sendMessage({ type: "reconnect" }, () => {
      updateAuthUI();
      refreshStatus();
    });
  });
});
