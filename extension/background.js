// Sidekar Chrome Extension — Service Worker
// Connects to local sidekar WebSocket server and executes commands in the user's browser.

const NATIVE_HOST_NAME = "dev.sidekar";
const RECONNECT_DELAY_MS = 3000;
const KEEPALIVE_INTERVAL_MS = 20000;

let ws = null;
let keepaliveTimer = null;
let authenticated = false;
let lastConnectError = null;
let sawAuthFail = false;
let currentPort = null;
let cliLoggedIn = false;

function wsUrl(port) {
  return `ws://127.0.0.1:${port}`;
}

function getExtToken() {
  return new Promise((resolve) => {
    chrome.storage.local.get(["extToken"], (data) => {
      resolve(String(data.extToken || "").trim());
    });
  });
}

// Use native messaging to ensure ext-server is running and get the port
function ensureServerViaNative() {
  return new Promise((resolve) => {
    let port;
    try {
      port = chrome.runtime.connectNative(NATIVE_HOST_NAME);
    } catch (e) {
      console.error("[sidekar] Failed to connect to native host:", e);
      resolve({ error: "Native host not installed. Run: sidekar ext install-host" });
      return;
    }

    let resolved = false;

    port.onMessage.addListener((msg) => {
      if (!resolved) {
        resolved = true;
        port.disconnect();
        resolve(msg);
      }
    });

    port.onDisconnect.addListener(() => {
      if (!resolved) {
        resolved = true;
        const err = chrome.runtime.lastError?.message || "Native host disconnected";
        resolve({ error: err });
      }
    });

    port.postMessage({ type: "ensure_server" });

    // Timeout after 5 seconds
    setTimeout(() => {
      if (!resolved) {
        resolved = true;
        port.disconnect();
        resolve({ error: "Native host timeout" });
      }
    }, 5000);
  });
}

async function connect() {
  if (ws && ws.readyState <= 1) return;

  const extToken = await getExtToken();
  if (!extToken) {
    console.log("[sidekar] no token configured — click extension icon to log in");
    lastConnectError = "Click the Sidekar icon to log in with GitHub";
    return;
  }

  // Use native messaging to ensure server is running and get port
  const nativeResult = await ensureServerViaNative();
  if (nativeResult.error) {
    console.error("[sidekar] Native host error:", nativeResult.error);
    lastConnectError = nativeResult.error;
    scheduleReconnect();
    return;
  }

  const port = nativeResult.port;
  if (!port) {
    lastConnectError = "Native host did not return a port";
    scheduleReconnect();
    return;
  }

  // Track CLI login status from native host
  cliLoggedIn = nativeResult.cli_logged_in === true;

  currentPort = port;
  const url = wsUrl(port);
  sawAuthFail = false;
  // Don't clear lastConnectError here - preserve auth_fail reason across reconnects
  // Only clear it on successful auth_ok

  try {
    ws = new WebSocket(url);
  } catch (e) {
    lastConnectError = "Could not create WebSocket";
    console.error("[sidekar] WebSocket constructor failed", e);
    scheduleReconnect();
    return;
  }

  ws.onopen = () => {
    console.log("[sidekar] connected to", url);
    send({
      type: "hello",
      version: chrome.runtime.getManifest().version,
      token: extToken,
    });
    startKeepalive();
  };

  ws.onmessage = async (event) => {
    let msg;
    try {
      msg = JSON.parse(event.data);
    } catch {
      return;
    }

    if (msg.type === "auth_ok") {
      authenticated = true;
      lastConnectError = null;
      console.log("[sidekar] authenticated");
      return;
    }
    if (msg.type === "auth_fail") {
      authenticated = false;
      sawAuthFail = true;
      lastConnectError = msg.reason ||
        "Authentication failed — try logging in again from the extension popup.";
      console.log("[sidekar] auth failed:", msg.reason || "check credentials");
      // Don't call ws.close() - let the server's close frame arrive naturally
      // This avoids race conditions with onerror/onclose handlers
      return;
    }

    if (!authenticated) return;

    const result = await handleCommand(msg);
    send({ id: msg.id, ...result });
  };

  ws.onclose = (ev) => {
    console.log("[sidekar] disconnected", ev.code, ev.reason, "sawAuthFail:", sawAuthFail);
    authenticated = false;
    stopKeepalive();
    if (!sawAuthFail && !lastConnectError) {
      if (ev.code === 1006 || ev.code === 1000) {
        // Connection closed right after hello - likely auth failed
        // Check if reason contains useful info
        if (ev.reason && ev.reason.length > 0) {
          lastConnectError = ev.reason;
        } else {
          lastConnectError = "Run 'sidekar login' in terminal to complete the authentication.";
        }
      } else {
        lastConnectError = `Disconnected (code ${ev.code})`;
      }
    }
    scheduleReconnect();
  };

  ws.onerror = () => {
    // Don't overwrite if we already have a real error from auth_fail
    if (!sawAuthFail && !lastConnectError) {
      lastConnectError = "WebSocket connection error";
    }
    console.error("[sidekar] WebSocket error to", url);
  };
}

function send(obj) {
  if (ws && ws.readyState === 1) {
    ws.send(JSON.stringify(obj));
  }
}

function scheduleReconnect() {
  ws = null;
  setTimeout(connect, RECONNECT_DELAY_MS);
}

function startKeepalive() {
  stopKeepalive();
  keepaliveTimer = setInterval(() => {
    send({ type: "ping" });
  }, KEEPALIVE_INTERVAL_MS);
}

function stopKeepalive() {
  if (keepaliveTimer) {
    clearInterval(keepaliveTimer);
    keepaliveTimer = null;
  }
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

// Ref map: tab_id -> { ref_num -> css_path }
const refMaps = new Map();

/** Safe unwrap of chrome.scripting.executeScript results (empty array / missing result). */
function firstInjectionResult(exec) {
  if (!exec || exec.length === 0) {
    return { error: "No result from executeScript" };
  }
  const { result, error } = exec[0];
  if (error != null && error !== "") {
    return { error: typeof error === "string" ? error : JSON.stringify(error) };
  }
  if (result === undefined) {
    return { error: "No result from page" };
  }
  return result;
}

async function handleCommand(msg) {
  try {
    switch (msg.command) {
      case "tabs":
        return await cmdTabs();
      case "read":
        return await cmdRead(msg);
      case "screenshot":
        return await cmdScreenshot(msg);
      case "click":
        return await cmdClick(msg);
      case "type":
        return await cmdType(msg);
      case "axtree":
        return await cmdAxtree(msg);
      case "eval":
        return await cmdEval(msg);
      case "navigate":
        return await cmdNavigate(msg);
      case "newtab":
        return await cmdNewTab(msg);
      case "close":
        return await cmdClose(msg);
      case "scroll":
        return await cmdScroll(msg);
      default:
        return { error: `Unknown command: ${msg.command}` };
    }
  } catch (e) {
    return { error: e.message };
  }
}

async function cmdTabs() {
  const tabs = await chrome.tabs.query({});
  return {
    tabs: tabs.map((t) => ({
      id: t.id,
      url: t.url,
      title: t.title,
      active: t.active,
      windowId: t.windowId,
    })),
  };
}

async function cmdRead(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      func: () => {
        const sel =
          document.querySelector("article") ||
          document.querySelector("main") ||
          document.body;
        return {
          url: location.href,
          title: document.title,
          text: sel.innerText.substring(0, 50000),
        };
      },
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  return firstInjectionResult(exec);
}

async function cmdScreenshot(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  try {
    const tab = await chrome.tabs.get(tabId);
    await chrome.tabs.update(tabId, { active: true });
    await sleep(200);
    const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, {
      format: "jpeg",
      quality: 80,
    });
    return { screenshot: dataUrl };
  } catch (e) {
    return { error: e.message || String(e) };
  }
}

async function cmdClick(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const target = msg.target;

  // Resolve ref number to data-sidekar-ref attribute
  const refNum = typeof target === "string" && /^\d+$/.test(target) ? parseInt(target) : null;

  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      func: (target, refNum) => {
      let el;
      if (refNum !== null) {
        el = document.querySelector(`[data-sidekar-ref="${refNum}"]`);
        if (!el) return { error: `Ref ${refNum} not found. Run axtree first.` };
      } else if (typeof target === "string") {
        if (target.startsWith("text:")) {
          const text = target.slice(5);
          const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT);
          let best = null;
          while (walker.nextNode()) {
            const node = walker.currentNode;
            const nodeText = node.innerText?.trim();
            if (nodeText && nodeText === text && node.offsetParent !== null) {
              // Prefer smallest (most specific) match
              if (!best || node.innerText.length <= best.innerText.length) best = node;
            }
          }
          el = best;
        } else {
          el = document.querySelector(target);
        }
      }
      if (!el) return { error: `Element not found: ${target}` };
      el.scrollIntoView({ block: "center" });
      el.click();
      return { clicked: true, tag: el.tagName, text: el.innerText?.substring(0, 100) };
    },
    args: [target, refNum],
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  return firstInjectionResult(exec);
}

async function cmdType(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector;
  const text = msg.text;

  // Resolve ref number
  const refNum = /^\d+$/.test(selector) ? parseInt(selector) : null;

  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      func: (selector, text, refNum) => {
      let el;
      if (refNum !== null) {
        el = document.querySelector(`[data-sidekar-ref="${refNum}"]`);
        if (!el) return { error: `Ref ${refNum} not found. Run axtree first.` };
      } else {
        el = document.querySelector(selector);
        if (!el) return { error: `Element not found: ${selector}` };
      }
      el.focus();
      // For contenteditable
      if (el.getAttribute("contenteditable") === "true") {
        el.textContent = text;
        el.dispatchEvent(new InputEvent("input", { bubbles: true, data: text }));
        return { typed: true, length: text.length };
      }
      el.value = text;
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
      return { typed: true, selector, length: text.length };
    },
    args: [selector, text, refNum],
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  return firstInjectionResult(exec);
}

async function cmdAxtree(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      func: () => {
      // Clean up old refs
      document.querySelectorAll("[data-sidekar-ref]").forEach((el) => {
        el.removeAttribute("data-sidekar-ref");
      });

      const elements = [];
      let refCounter = 1;
      const interactiveTags = new Set([
        "A", "BUTTON", "INPUT", "SELECT", "TEXTAREA", "DETAILS", "SUMMARY",
      ]);
      const interactiveRoles = new Set([
        "button", "link", "textbox", "checkbox", "radio", "combobox",
        "menuitem", "tab", "switch", "slider", "searchbox", "option",
      ]);

      function walk(node) {
        if (node.nodeType !== 1) return;
        if (node.offsetParent === null && node.tagName !== "BODY") return;

        const role = node.getAttribute("role") || "";
        const tag = node.tagName;
        const isInteractive =
          interactiveRoles.has(role) ||
          interactiveTags.has(tag) ||
          node.getAttribute("tabindex") !== null ||
          node.getAttribute("contenteditable") === "true";

        if (isInteractive) {
          const ref = refCounter++;
          node.setAttribute("data-sidekar-ref", ref);
          const name =
            node.getAttribute("aria-label") ||
            node.getAttribute("placeholder") ||
            node.getAttribute("title") ||
            node.innerText?.substring(0, 80)?.trim() ||
            "";
          elements.push({
            ref,
            tag,
            role: role || tag.toLowerCase(),
            name,
            type: node.getAttribute("type") || "",
            value: node.value || "",
          });
        }
        for (const child of node.children) walk(child);
      }

      walk(document.body);
      return { url: location.href, title: document.title, elements };
    },
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  const out = firstInjectionResult(exec);
  if (out && out.elements) {
    refMaps.set(tabId, out.elements.length);
  }
  return out;
}

async function cmdNavigate(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const url = msg.url;
  if (!url) return { error: "No URL provided" };

  await chrome.tabs.update(tabId, { url });
  // Wait for page load
  await new Promise((resolve) => {
    function listener(tid, info) {
      if (tid === tabId && info.status === "complete") {
        chrome.tabs.onUpdated.removeListener(listener);
        resolve();
      }
    }
    chrome.tabs.onUpdated.addListener(listener);
    // Timeout fallback
    setTimeout(() => {
      chrome.tabs.onUpdated.removeListener(listener);
      resolve();
    }, 15000);
  });
  await sleep(500);

  const tab = await chrome.tabs.get(tabId);
  return { url: tab.url, title: tab.title };
}

async function cmdNewTab(msg) {
  const url = msg.url || "about:blank";
  const tab = await chrome.tabs.create({ url, active: true });
  // Wait for load
  if (url !== "about:blank") {
    await new Promise((resolve) => {
      function listener(tid, info) {
        if (tid === tab.id && info.status === "complete") {
          chrome.tabs.onUpdated.removeListener(listener);
          resolve();
        }
      }
      chrome.tabs.onUpdated.addListener(listener);
      setTimeout(() => {
        chrome.tabs.onUpdated.removeListener(listener);
        resolve();
      }, 15000);
    });
    await sleep(500);
  }
  const updated = await chrome.tabs.get(tab.id);
  return { id: updated.id, url: updated.url, title: updated.title };
}

async function cmdClose(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  await chrome.tabs.remove(tabId);
  return { closed: true, tabId };
}

async function cmdScroll(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const direction = msg.direction || "down";
  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      func: (direction) => {
      const amount = Math.round(window.innerHeight * 0.8);
      switch (direction) {
        case "up": window.scrollBy(0, -amount); break;
        case "down": window.scrollBy(0, amount); break;
        case "top": window.scrollTo(0, 0); break;
        case "bottom": window.scrollTo(0, document.body.scrollHeight); break;
      }
      return { scrolled: direction, y: window.scrollY };
    },
    args: [direction],
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  return firstInjectionResult(exec);
}

async function cmdEval(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      func: (code) => {
        try {
          return { result: String(eval(code)) };
        } catch (e) {
          return { error: e.message };
        }
      },
      args: [msg.code],
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  return firstInjectionResult(exec);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async function getActiveTabId() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab) throw new Error("No active tab");
  return tab.id;
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// ---------------------------------------------------------------------------
// Internal message handling (from popup)
// ---------------------------------------------------------------------------

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg.type === "colorScheme") {
    setIconForScheme(msg.dark);
    return false;
  }
  if (msg.type === "status") {
    // Query native host for fresh CLI login status
    ensureServerViaNative().then((nativeResult) => {
      if (!nativeResult.error) {
        cliLoggedIn = nativeResult.cli_logged_in === true;
      }
      sendResponse({
        connected: ws !== null && ws.readyState === 1,
        authenticated,
        wsUrl: currentPort ? wsUrl(currentPort) : null,
        lastError: lastConnectError,
        cliLoggedIn,
      });
    });
    return true;  // async response
  }
  if (msg.type === "reconnect") {
    if (ws) {
      try { ws.close(); } catch {}
    }
    ws = null;
    authenticated = false;
    lastConnectError = null;  // Clear error for fresh retry
    connect();
    sendResponse({ ok: true });
    return false;
  }
  if (msg.type === "setToken") {
    const extToken = String(msg.extToken || "").trim();
    chrome.storage.local.set({ extToken }, () => {
      if (ws) {
        try { ws.close(); } catch {}
      }
      ws = null;
      authenticated = false;
      connect();
      sendResponse({ ok: true });
    });
    return true;
  }
  if (msg.type === "startOAuth") {
    startOAuthFlow();
    sendResponse({ ok: true });
    return false;
  }
  return false;
});

// ---------------------------------------------------------------------------
// OAuth flow (runs in background so it survives popup closing)
// ---------------------------------------------------------------------------

const CALLBACK_URL = "https://sidekar.dev/ext-callback";

function startOAuthFlow() {
  const authUrl = "https://sidekar.dev/api/auth/github?redirect=/ext-callback";

  chrome.tabs.create({ url: authUrl, active: true }, (tab) => {
    const authTabId = tab.id;

    function onUpdated(tabId, changeInfo, updatedTab) {
      if (tabId !== authTabId) return;
      if (changeInfo.status !== "complete") return;
      if (!updatedTab.url || !updatedTab.url.startsWith(CALLBACK_URL)) return;

      chrome.tabs.onUpdated.removeListener(onUpdated);
      chrome.tabs.onRemoved.removeListener(onRemoved);

      // Poll for the token (page fetches it async)
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
              console.error("[sidekar] Could not read token:", chrome.runtime.lastError);
              return;
            }

            const token =
              results && results[0] && results[0].result
                ? results[0].result
                : null;

            if (token && token.length > 0) {
              console.log("[sidekar] Got token from callback page");
              chrome.storage.local.set({ extToken: token }, () => {
                if (ws) {
                  try { ws.close(); } catch {}
                }
                ws = null;
                authenticated = false;
                connect();
              });

              // Close the callback tab
              chrome.tabs.remove(authTabId).catch(() => {});
            } else if (++attempts < maxAttempts) {
              setTimeout(tryReadToken, 250);
            } else {
              console.error("[sidekar] Token not found after polling");
            }
          }
        );
      }

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
}

// ---------------------------------------------------------------------------
// Theme-adaptive icon (Chrome has no manifest-level theme_icons)
// ---------------------------------------------------------------------------

function setIconForScheme(dark) {
  const variant = dark ? "dark" : "light";
  chrome.action.setIcon({
    path: {
      16: `icons/icon-${variant}-16.png`,
      48: `icons/icon-${variant}-48.png`,
      128: `icons/icon-${variant}-128.png`,
    },
  });
}

async function ensureOffscreen() {
  const contexts = await chrome.runtime.getContexts({
    contextTypes: ["OFFSCREEN_DOCUMENT"],
  });
  if (contexts.length === 0) {
    await chrome.offscreen.createDocument({
      url: "offscreen.html",
      reasons: ["MATCH_MEDIA"],
      justification: "Detect light/dark color scheme for toolbar icon",
    });
  }
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

chrome.alarms.create("keepalive", { periodInMinutes: 0.4 });

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === "keepalive") {
    if (!ws || ws.readyState !== 1) {
      connect();
    }
  }
});

connect();
ensureOffscreen().catch(() => {});
console.log("[sidekar] service worker started");

chrome.runtime.onStartup.addListener(() => {
  connect();
  ensureOffscreen().catch(() => {});
});
chrome.runtime.onInstalled.addListener(() => {
  console.log("[sidekar] extension installed/updated");
  connect();
  ensureOffscreen().catch(() => {});
});
