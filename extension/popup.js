const status = document.getElementById("status");
const detailEl = document.getElementById("detail");
const secretInput = document.getElementById("secret");
const portInput = document.getElementById("port");
const saveBtn = document.getElementById("save");
const endpointEl = document.getElementById("endpoint");

const DEFAULT_EXT_PORT = 9876;

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

chrome.storage.local.get(["secret", "extPort"], (data) => {
  if (data.secret) {
    secretInput.value = data.secret;
  }
  const p = data.extPort;
  if (typeof p === "number" && p >= 1 && p <= 65535) {
    portInput.value = String(p);
  } else if (typeof p === "string" && /^\d+$/.test(p)) {
    portInput.value = p;
  }
});

refreshStatus();

saveBtn.addEventListener("click", () => {
  const secret = secretInput.value.trim();
  if (!secret) return;

  const rawPort = portInput.value.trim();
  const parsed = parseInt(rawPort, 10);
  const extPort = rawPort === "" || !Number.isFinite(parsed) ? DEFAULT_EXT_PORT : parsed;

  status.textContent = "Connecting...";
  status.className = "pending";
  detailEl.textContent = "";

  chrome.runtime.sendMessage({ type: "setSecret", secret, extPort }, () => {
    if (chrome.runtime.lastError) {
      detailEl.textContent = chrome.runtime.lastError.message || "";
      status.textContent = "Not connected";
      status.className = "disconnected";
      return;
    }
    const delays = [400, 1200, 2500, 5000];
    delays.forEach((ms) => setTimeout(refreshStatus, ms));
  });
});
