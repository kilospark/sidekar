// Sidekar Chrome Extension — Service Worker
// Connects to local sidekar WebSocket server and executes commands in the user's browser.

const DEFAULT_EXT_PORT = 9876;
const RECONNECT_DELAY_MS = 3000;
const KEEPALIVE_INTERVAL_MS = 20000;

let ws = null;
let keepaliveTimer = null;
let authenticated = false;
let lastConnectError = null;
let sawAuthFail = false;

function normalizePort(raw) {
  if (raw === undefined || raw === null || raw === "") return DEFAULT_EXT_PORT;
  const n = typeof raw === "string" ? parseInt(raw, 10) : Number(raw);
  if (!Number.isFinite(n) || n < 1 || n > 65535) return DEFAULT_EXT_PORT;
  return n;
}

function wsUrl(port) {
  return `ws://127.0.0.1:${port}`;
}

// Secret and extPort (must match sidekar / SIDEKAR_EXT_PORT) live in chrome.storage.local
function getBridgeConfig() {
  return new Promise((resolve) => {
    chrome.storage.local.get(["secret", "extPort"], (data) => {
      resolve({
        secret: String(data.secret || "").trim(),
        extPort: normalizePort(data.extPort),
      });
    });
  });
}

async function connect() {
  if (ws && ws.readyState <= 1) return;

  const { secret, extPort } = await getBridgeConfig();
  if (!secret) {
    console.log("[sidekar] no secret configured — click extension icon to set it");
    lastConnectError = "Set a secret from: sidekar ext secret";
    return;
  }

  const url = wsUrl(extPort);
  sawAuthFail = false;
  lastConnectError = null;

  try {
    ws = new WebSocket(url);
  } catch (e) {
    lastConnectError = "Could not create WebSocket — is the bridge running? (sidekar ext tabs)";
    console.error("[sidekar] WebSocket constructor failed", e);
    scheduleReconnect();
    return;
  }

  ws.onopen = () => {
    console.log("[sidekar] connected to", url);
    send({ type: "hello", version: chrome.runtime.getManifest().version, secret });
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
      lastConnectError =
        "Secret rejected — run sidekar ext secret and paste the value again (no spaces).";
      console.log("[sidekar] auth failed — check secret");
      ws.close();
      return;
    }

    if (!authenticated) return;

    const result = await handleCommand(msg);
    send({ id: msg.id, ...result });
  };

  ws.onclose = (ev) => {
    console.log("[sidekar] disconnected", ev.code, ev.reason);
    authenticated = false;
    stopKeepalive();
    if (!sawAuthFail && !lastConnectError) {
      if (ev.code === 1006) {
        lastConnectError =
          "Cannot reach the bridge on this port. In a terminal run: sidekar ext tabs (or sidekar ext-server).";
      } else if (ev.code !== 1000) {
        lastConnectError = `Disconnected (code ${ev.code}). Is sidekar ext-server running on port ${extPort}?`;
      }
    }
    scheduleReconnect();
  };

  ws.onerror = () => {
    lastConnectError =
      "Connection error — start the bridge: sidekar ext tabs (uses port " + extPort + ")";
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
  const [result] = await chrome.scripting.executeScript({
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
  return result.result;
}

async function cmdScreenshot(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const tab = await chrome.tabs.get(tabId);
  await chrome.tabs.update(tabId, { active: true });
  await sleep(200);
  const dataUrl = await chrome.tabs.captureVisibleTab(tab.windowId, {
    format: "jpeg",
    quality: 80,
  });
  return { screenshot: dataUrl };
}

async function cmdClick(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const target = msg.target;

  // Resolve ref number to data-sidekar-ref attribute
  const refNum = typeof target === "string" && /^\d+$/.test(target) ? parseInt(target) : null;

  const [result] = await chrome.scripting.executeScript({
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
  return result.result;
}

async function cmdType(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector;
  const text = msg.text;

  // Resolve ref number
  const refNum = /^\d+$/.test(selector) ? parseInt(selector) : null;

  const [result] = await chrome.scripting.executeScript({
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
  return result.result;
}

async function cmdAxtree(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const [result] = await chrome.scripting.executeScript({
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
  // Store ref count for this tab (for validation)
  if (result.result && result.result.elements) {
    refMaps.set(tabId, result.result.elements.length);
  }
  return result.result;
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
  const [result] = await chrome.scripting.executeScript({
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
  return result.result;
}

async function cmdEval(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const [result] = await chrome.scripting.executeScript({
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
  return result.result;
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
  if (msg.type === "status") {
    (async () => {
      const { extPort } = await getBridgeConfig();
      sendResponse({
        connected: ws !== null && ws.readyState === 1,
        authenticated,
        wsUrl: wsUrl(extPort),
        lastError: lastConnectError,
      });
    })();
    return true;
  }
  if (msg.type === "reconnect") {
    if (ws) {
      try { ws.close(); } catch {}
    }
    ws = null;
    authenticated = false;
    connect();
    sendResponse({ ok: true });
    return false;
  }
  if (msg.type === "setSecret") {
    const extPort = normalizePort(msg.extPort);
    const secret = String(msg.secret || "").trim();
    chrome.storage.local.set({ secret, extPort }, () => {
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
  return false;
});

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
console.log("[sidekar] service worker started");

chrome.runtime.onStartup.addListener(connect);
chrome.runtime.onInstalled.addListener(() => {
  console.log("[sidekar] extension installed/updated");
  connect();
});
