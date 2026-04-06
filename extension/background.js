// Sidekar Chrome Extension — Service Worker
// Connects to sidekar daemon via localhost WebSocket (discovered through sidekar.dev API).

const API_BASE = "https://sidekar.dev";
const RECONNECT_DELAY_MS = 3000;

let ws = null;
let reconnectTimer = null;
let authenticated = false;
let lastConnectError = null;
let cliLoggedIn = false;
let creatingOffscreen = null;
let awaitListeners = new Map(); // id → resolve callback

function clearStoredExtToken() {
  return new Promise((resolve) => {
    chrome.storage.local.remove(["extToken", "extProfile"], () => resolve());
  });
}

function getExtToken() {
  return new Promise((resolve) => {
    chrome.storage.local.get(["extToken"], (data) => {
      resolve(String(data.extToken || "").trim());
    });
  });
}


function sendWs(obj) {
  if (!ws || ws.readyState !== WebSocket.OPEN) return false;
  try {
    ws.send(JSON.stringify(obj));
    return true;
  } catch (e) {
    console.error("[sidekar] WS send failed", e);
    return false;
  }
}

function sendNativeAwait(obj, timeoutMs = 10000) {
  return new Promise((resolve) => {
    if (!ws || ws.readyState !== WebSocket.OPEN || !authenticated) {
      resolve({ error: "Not connected to daemon" });
      return;
    }
    const id = "cli_" + Math.random().toString(36).slice(2, 10);
    awaitListeners.set(id, resolve);
    ws.send(JSON.stringify({ ...obj, id }));
    setTimeout(() => {
      if (awaitListeners.has(id)) {
        awaitListeners.delete(id);
        resolve({ error: "Command timed out" });
      }
    }, timeoutMs);
  });
}

function scheduleReconnect() {
  if (reconnectTimer) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    connect();
  }, RECONNECT_DELAY_MS);
}

/** Discover daemon ports via sidekar.dev API, then probe localhost. */
async function discoverDaemonPort(extToken) {
  const resp = await fetch(`${API_BASE}/api/sessions?discover`, {
    headers: { Authorization: `Bearer ${extToken}` },
  });
  if (!resp.ok) return null;
  const { ports } = await resp.json();
  if (!ports || ports.length === 0) return null;

  // Probe each port on localhost in parallel
  const results = await Promise.allSettled(
    ports.map(async (port) => {
      const r = await fetch(`http://127.0.0.1:${port}/health`, {
        signal: AbortSignal.timeout(1000),
      });
      if (r.ok) {
        const body = await r.json();
        if (body.sidekar) return port;
      }
      throw new Error("not sidekar");
    })
  );
  const found = results.find((r) => r.status === "fulfilled");
  return found ? found.value : null;
}

async function connect() {
  if (ws && ws.readyState === WebSocket.OPEN) return;

  // Close stale socket
  if (ws) {
    try { ws.close(); } catch {}
    ws = null;
  }

  const extToken = await getExtToken();
  if (!extToken) {
    lastConnectError = "Not signed in — sign in from the extension popup.";
    // Don't schedule reconnect for auth issues — wait for token change
    return;
  }

  let port;
  try {
    port = await discoverDaemonPort(extToken);
  } catch (e) {
    lastConnectError = "Cannot reach sidekar.dev — check internet connection.";
    scheduleReconnect();
    return;
  }

  if (!port) {
    lastConnectError = "No sidekar daemon found. Is sidekar running?";
    scheduleReconnect();
    return;
  }

  try {
    ws = new WebSocket(`ws://127.0.0.1:${port}/ext`);
  } catch (e) {
    lastConnectError = "WebSocket connection failed.";
    ws = null;
    scheduleReconnect();
    return;
  }

  ws.onopen = () => {
    console.log("[sidekar] WS connected to 127.0.0.1:" + port);
    // Wait for welcome, then register
  };

  ws.onmessage = async (event) => {
    let msg;
    try { msg = JSON.parse(event.data); } catch { return; }

    // Welcome message
    if (msg.type === "welcome") {
      const ua = navigator.userAgent || "";
      const brands = (navigator.userAgentData && navigator.userAgentData.brands || [])
        .map((b) => b.brand.toLowerCase());

      let browser = "Chrome";
      if (typeof navigator.brave !== "undefined") browser = "Brave";
      else if (ua.includes("Vivaldi/")) browser = "Vivaldi";
      else if (brands.some((b) => b.includes("opera")) || ua.includes("OPR/")) browser = "Opera";
      else if (ua.includes("Edg/") || brands.some((b) => b.includes("microsoft edge"))) browser = "Edge";
      else if (ua.includes("YaBrowser/") || brands.some((b) => b.includes("yandex"))) browser = "Yandex";
      else if (ua.includes("SamsungBrowser/")) browser = "Samsung";
      else if (ua.includes("Chromium/")) browser = "Chromium";
      sendWs({
        type: "bridge_register",
        version: chrome.runtime.getManifest().version,
        token: extToken,
        browser,
      });
      return;
    }

    if (msg.type === "auth_ok") {
      authenticated = true;
      cliLoggedIn = msg.cli_logged_in === true;
      lastConnectError = null;
      console.log("[sidekar] authenticated via WebSocket bridge");
      return;
    }

    if (msg.type === "auth_fail") {
      authenticated = false;
      cliLoggedIn = msg.cli_logged_in === true;
      const reason = msg.reason || "Authentication failed.";
      lastConnectError = reason;
      // Only clear token when server explicitly says it's invalid
      if (msg.clear_token === true) {
        await clearStoredExtToken();
        lastConnectError = "Extension session expired — sign in again from the extension popup.";
      }
      console.log("[sidekar] auth failed:", reason);
      return;
    }

    // Ping from daemon
    if (msg.type === "ping") {
      sendWs({ type: "pong" });
      return;
    }

    // Response to sendNativeAwait
    if (msg.id && awaitListeners.has(msg.id)) {
      const resolve = awaitListeners.get(msg.id);
      awaitListeners.delete(msg.id);
      resolve(msg);
      return;
    }

    // Command from daemon
    if (!authenticated || !msg || !msg.command) return;
    const result = await handleCommand(msg);
    sendWs({ id: msg.id, ...result });
  };

  ws.onclose = () => {
    console.log("[sidekar] WS disconnected");
    ws = null;
    authenticated = false;
    cliLoggedIn = false;
    if (!lastConnectError) {
      lastConnectError = "Daemon disconnected.";
    }
    // Reject pending awaits
    for (const [id, resolve] of awaitListeners) {
      resolve({ error: "Disconnected" });
    }
    awaitListeners.clear();
    scheduleReconnect();
  };

  ws.onerror = () => {
    // onclose fires after onerror, so reconnect is handled there
  };
}

// ---------------------------------------------------------------------------
// Command handlers
// ---------------------------------------------------------------------------

// Ref map: tab_id -> { ref_num -> css_path }
const refMaps = new Map();

// Watch state: watchId -> { tabId, selector } — used for re-injection on navigation
const activeWatchers = new Map();

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
      case "paste":
        return await cmdPaste(msg);
      case "setvalue":
        return await cmdSetValue(msg);
      case "axtree":
        return await cmdAxtree(msg);
      case "eval":
        return await cmdEval(msg);
      case "evalpage":
        return await cmdEvalPage(msg);
      case "navigate":
        return await cmdNavigate(msg);
      case "newtab":
        return await cmdNewTab(msg);
      case "close":
        return await cmdClose(msg);
      case "scroll":
        return await cmdScroll(msg);
      case "history":
        return await cmdHistory(msg);
      case "watch":
        return await cmdWatch(msg);
      case "unwatch":
        return await cmdUnwatch(msg);
      case "watchers":
        return await cmdWatchers();
      case "context":
        return await cmdContext();
      default:
        return { error: `Unknown command: ${msg.command}` };
    }
  } catch (e) {
    return { error: e.message };
  }
}

async function executeScriptResult(tabId, func, args = [], world = "ISOLATED") {
  let exec;
  try {
    exec = await chrome.scripting.executeScript({
      target: { tabId },
      world,
      func,
      args,
    });
  } catch (e) {
    return { error: e.message || String(e) };
  }
  return firstInjectionResult(exec);
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
  return await executeScriptResult(tabId, () => {
    const sel =
      document.querySelector("article") ||
      document.querySelector("main") ||
      document.body;
    return {
      url: location.href,
      title: document.title,
      text: sel.innerText.substring(0, 50000),
    };
  });
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

  return await executeScriptResult(tabId, (target, refNum) => {
      let el;
      if (refNum !== null) {
        el = document.querySelector(`[data-sidekar-ref="${refNum}"]`);
        if (!el) return { error: `Ref ${refNum} not found. Run ax-tree first.` };
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
    }, [target, refNum]);
}

async function cmdType(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector;
  const text = msg.text;

  // Resolve ref number
  const refNum = /^\d+$/.test(selector) ? parseInt(selector) : null;

  return await executeScriptResult(tabId, (selector, text, refNum) => {
      let el;
      if (refNum !== null) {
        el = document.querySelector(`[data-sidekar-ref="${refNum}"]`);
        if (!el) return { error: `Ref ${refNum} not found. Run ax-tree first.` };
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
    }, [selector, text, refNum]);
}

async function focusTabWindow(tabId) {
  try {
    const tab = await chrome.tabs.get(tabId);
    if (tab.windowId != null) {
      try {
        await chrome.windows.update(tab.windowId, { focused: true });
      } catch {}
    }
    try {
      await chrome.tabs.update(tabId, { active: true });
    } catch {}
  } catch {}
}

function htmlToPlainText(html) {
  if (!html) return "";
  return html
    .replace(/<\s*br\s*\/?>/gi, "\n")
    .replace(/<\s*\/p\s*>/gi, "\n\n")
    .replace(/<\s*\/div\s*>/gi, "\n")
    .replace(/<\s*\/li\s*>/gi, "\n")
    .replace(/<li\b[^>]*>/gi, "- ")
    .replace(/<[^>]+>/g, " ")
    .replace(/&nbsp;/gi, " ")
    .replace(/&amp;/gi, "&")
    .replace(/&lt;/gi, "<")
    .replace(/&gt;/gi, ">")
    .replace(/&quot;/gi, "\"")
    .replace(/&#39;/gi, "'")
    .replace(/\r\n/g, "\n")
    .replace(/[ \t]+\n/g, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .replace(/[ \t]{2,}/g, " ")
    .trim();
}

async function trustedPasteViaDebugger(tabId) {
  const target = { tabId };
  const version = "1.3";
  const isMac = /Mac/i.test(navigator.userAgent || "");
  const modifierBit = isMac ? 4 : 2; // Meta on macOS, Control elsewhere

  async function send(method, params) {
    return await chrome.debugger.sendCommand(target, method, params);
  }

  await focusTabWindow(tabId);

  try {
    await chrome.debugger.attach(target, version);
    await send("Input.dispatchKeyEvent", {
      type: "rawKeyDown",
      modifiers: modifierBit,
      windowsVirtualKeyCode: 86,
      code: "KeyV",
      key: "v",
    });
    await send("Input.dispatchKeyEvent", {
      type: "keyUp",
      modifiers: modifierBit,
      windowsVirtualKeyCode: 86,
      code: "KeyV",
      key: "v",
    });
    return { ok: true, mode: isMac ? "debugger-meta-v" : "debugger-ctrl-v", verified: true };
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  } finally {
    try {
      await chrome.debugger.detach(target);
    } catch {}
  }
}

async function insertTextViaDebugger(tabId, text) {
  console.log("[sidekar] insertTextViaDebugger called", { tabId, textLength: text?.length });
  // Use CDP via chrome.debugger - this WILL block user input but is the only way
  // to inject text into Google Docs from the extension
  const target = { tabId };
  const version = "1.3";

  if (!text) {
    return { ok: false, error: "No text available for insertText" };
  }

  try {
    console.log("[sidekar] attaching debugger...");
    await chrome.debugger.attach(target, version);
    console.log("[sidekar] sending insertText command...");
    await chrome.debugger.sendCommand(target, "Input.insertText", { text });
    console.log("[sidekar] insertText completed successfully");
    return { ok: true, mode: "debugger-insertText", verified: true };
  } catch (e) {
    console.error("[sidekar] insertTextViaDebugger error:", e);
    return { ok: false, error: e?.message || String(e) };
  } finally {
    try {
      await chrome.debugger.detach(target);
      console.log("[sidekar] debugger detached");
    } catch (e) {
      console.error("[sidekar] detach error:", e);
    }
  }
}

async function insertTextViaCliExec(tabId, text) {
  console.log("[sidekar] insertTextViaCliExec called", { tabId, textLength: text?.length });
  if (!text) {
    return { ok: false, error: "No text available for insertText" };
  }

  const result = await sendNativeAwait(
    {
      type: "cli_exec",
      command: "inserttext",
      text: text,
    },
    120000
  );
  console.log("[sidekar] cli_exec result:", result);
  if (result.ok) {
    return { ok: true, mode: result.mode || "cli-insertText", verified: true };
  }
  return { ok: false, error: result.error || "cli_exec failed" };
}

async function typeViaCliExec(tabId, text) {
  console.log("[sidekar] typeViaCliExec called", { tabId, textLength: text?.length });
  if (!text) {
    return { ok: false, error: "No text available" };
  }

  const result = await sendNativeAwait(
    {
      type: "cli_exec",
      command: "keyboard",
      text: text,
    },
    120000
  );
  console.log("[sidekar] cli_exec keyboard result:", result);
  if (result.ok) {
    return { ok: true, mode: result.mode || "cli-keyboard" };
  }
  return { ok: false, error: result.error || "cli_exec keyboard failed" };
}

async function clickGoogleDocsEditorViaDebugger(tabId) {
  const rect = await executeScriptResult(tabId, () => {
    const page =
      document.querySelector(".kix-page") ||
      document.querySelector(".kix-page-paginated") ||
      document.querySelector(".kix-appview-editor");
    if (!page) {
      return { error: "Google Docs page surface not found" };
    }
    const box = page.getBoundingClientRect();
    if (!box || box.width <= 0 || box.height <= 0) {
      return { error: "Google Docs page surface has no visible bounds" };
    }
    return {
      x: Math.round(box.left + Math.min(140, Math.max(80, box.width * 0.14))),
      y: Math.round(box.top + Math.min(240, Math.max(180, box.height * 0.22))),
    };
  });

  if (!rect || rect.error) {
    return { ok: false, error: rect?.error || "Could not resolve Google Docs editor rect" };
  }

  const target = { tabId };
  const version = "1.3";

  await focusTabWindow(tabId);

  try {
    await chrome.debugger.attach(target, version);
    await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
      type: "mouseMoved",
      x: rect.x,
      y: rect.y,
      button: "left",
      buttons: 1,
      clickCount: 1,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
      type: "mousePressed",
      x: rect.x,
      y: rect.y,
      button: "left",
      buttons: 1,
      clickCount: 1,
    });
    await chrome.debugger.sendCommand(target, "Input.dispatchMouseEvent", {
      type: "mouseReleased",
      x: rect.x,
      y: rect.y,
      button: "left",
      buttons: 0,
      clickCount: 1,
    });
    return { ok: true, mode: "debugger-editor-click", x: rect.x, y: rect.y };
  } catch (e) {
    return { ok: false, error: e?.message || String(e) };
  } finally {
    try {
      await chrome.debugger.detach(target);
    } catch {}
  }
}

async function shouldPreferInsertText(tabId) {
  try {
    const tab = await chrome.tabs.get(tabId);
    const url = String(tab?.url || "");
    return url.startsWith("https://docs.google.com/document/");
  } catch {
    return false;
  }
}

async function pingOffscreen(ms) {
  return await Promise.race([
    chrome.runtime.sendMessage({ target: "offscreen", type: "offscreenPing" }),
    new Promise((resolve) =>
      setTimeout(() => resolve({ ok: false, error: "offscreen ping timeout" }), ms)
    ),
  ]);
}

async function resetOffscreenDocument() {
  try {
    if (chrome.offscreen?.closeDocument) {
      await chrome.offscreen.closeDocument();
    }
  } catch {
    // No offscreen document or already torn down.
  }
  await delay(80);
}

/** Create offscreen doc if needed and verify runtime.sendMessage reaches it (MV3 race workaround). */
async function ensureOffscreenReady() {
  for (let attempt = 0; attempt < 3; attempt++) {
    await ensureOffscreen();
    await delay(attempt === 0 ? 50 : 120 + attempt * 80);
    const ping = await pingOffscreen(2000);
    if (ping && ping.pong) return;
    await resetOffscreenDocument();
  }
  throw new Error("Offscreen messaging unavailable (ping failed after recreate)");
}

async function writeClipboardContents(_tabId, html, plainText) {
  const offscreenMessageTimeoutMs = 12000;
  async function sendClipboardMessage() {
    return await Promise.race([
      chrome.runtime.sendMessage({
        target: "offscreen",
        type: "writeClipboard",
        html,
        plainText,
      }),
      new Promise((resolve) =>
        setTimeout(
          () => resolve({ ok: false, error: "Offscreen clipboard write timed out" }),
          offscreenMessageTimeoutMs
        )
      ),
    ]);
  }

  try {
    await ensureOffscreenReady();
    let response;
    try {
      response = await sendClipboardMessage();
    } catch (e) {
      const msg = e?.message || String(e);
      if (!/Receiving end does not exist|message port closed/i.test(msg)) {
        throw e;
      }
      await delay(75);
      response = await sendClipboardMessage();
    }
    if (response && response.ok) {
      return { ok: true, stage: "clipboard", mode: response.mode || "offscreen" };
    }
    return {
      ok: false,
      stage: "clipboard",
      mode: response?.mode || "offscreen",
      error: response?.error || "Offscreen clipboard write failed",
      async_error: response?.async_error,
    };
  } catch (e) {
    return { ok: false, stage: "clipboard", error: e?.message || String(e) };
  }
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function getDocsSnapshot(tabId) {
  const result = await executeScriptResult(tabId, () => {
    function clean(text) {
      return String(text || "")
        .replace(/[\u200B\u200C]/g, "")
        .replace(/\u00A0/g, " ")
        .replace(/\s+/g, " ")
        .trim();
    }
    const text = Array.from(document.querySelectorAll(".kix-wordhtmlgenerator-word-node"))
      .map((node) => clean(node.textContent))
      .filter(Boolean)
      .join(" ");
    return { ok: true, text };
  });
  if (!result || result.error) {
    return "";
  }
  return typeof result.text === "string" ? result.text : "";
}

async function getGoogleDocsAnnotatedState(tabId) {
  return await executeScriptResult(
    tabId,
    async () => {
      const getAnnotatedText = window._docs_annotate_getAnnotatedText;
      if (typeof getAnnotatedText !== "function") {
        return { ok: false, error: "Google Docs annotated text API unavailable" };
      }

      const annotated = await Promise.race([
        getAnnotatedText(),
        new Promise((resolve) =>
          setTimeout(() => resolve({ __sidekar_timeout: true }), 500)
        ),
      ]);
      if (annotated && annotated.__sidekar_timeout) {
        return { ok: false, error: "Google Docs annotated text API timed out" };
      }
      if (!annotated || typeof annotated.setSelection !== "function") {
        return { ok: false, error: "Google Docs annotated text object unavailable" };
      }

      const text = typeof annotated.getText === "function" ? String(annotated.getText() || "") : "";
      const selection = typeof annotated.getSelection === "function" ? annotated.getSelection()?.[0] : null;

      function asFiniteNumber(value) {
        const num = Number(value);
        return Number.isFinite(num) ? num : null;
      }

      function getSelectionEndpoints(selection) {
        if (!selection || typeof selection !== "object") {
          return null;
        }
        const candidates = [
          ["anchor", "focus"],
          ["base", "extent"],
          ["start", "end"],
        ];
        for (const [a, b] of candidates) {
          const start = asFiniteNumber(selection[a]);
          const end = asFiniteNumber(selection[b]);
          if (start != null && end != null) {
            return { start, end };
          }
        }
        return null;
      }

      return {
        ok: true,
        text,
        selection: getSelectionEndpoints(selection),
      };
    },
    [],
    "MAIN"
  );
}

async function setGoogleDocsAnnotatedSelection(tabId, start, end) {
  return await executeScriptResult(
    tabId,
    async (start, end) => {
      const getAnnotatedText = window._docs_annotate_getAnnotatedText;
      if (typeof getAnnotatedText !== "function") {
        return { ok: false, error: "Google Docs annotated text API unavailable" };
      }

      const annotated = await Promise.race([
        getAnnotatedText(),
        new Promise((resolve) =>
          setTimeout(() => resolve({ __sidekar_timeout: true }), 500)
        ),
      ]);
      if (annotated && annotated.__sidekar_timeout) {
        return { ok: false, error: "Google Docs annotated text API timed out" };
      }
      if (!annotated || typeof annotated.setSelection !== "function") {
        return { ok: false, error: "Google Docs annotated text object unavailable" };
      }

      annotated.setSelection(Number(start), Number(end));
      return { ok: true };
    },
    [start, end],
    "MAIN"
  );
}

// Shared in-page helper for Google Docs text sink operations
function getGoogleDocsTextSinkCode() {
  return () => {
    const iframe = document.querySelector("iframe.docs-texteventtarget-iframe");
    if (!iframe) {
      return { ok: false, error: "Google Docs text input iframe not found" };
    }
    const doc = iframe.contentDocument || iframe.contentWindow?.document;
    const target =
      doc?.activeElement ||
      doc?.querySelector('[contenteditable="true"], textarea, input, [role="textbox"]') ||
      doc?.body ||
      doc?.documentElement;
    if (!doc || !target) {
      return { ok: false, error: "Google Docs text input target not found" };
    }
    if (typeof iframe.focus === "function") iframe.focus();
    if (doc.defaultView && typeof doc.defaultView.focus === "function") doc.defaultView.focus();
    if (typeof target.focus === "function") target.focus();
    return {
      ok: true,
      target: {
        tag: target.tagName || "",
        id: target.id || "",
        className: typeof target.className === "string" ? target.className : "",
      },
    };
  };
}

async function focusGoogleDocsTextSink(tabId) {
  return await executeScriptResult(tabId, getGoogleDocsTextSinkCode());
}

async function pasteIntoGoogleDocsTextSink(tabId, html, plainText) {
  return await executeScriptResult(
    tabId,
    (html, plainText) => {
      const iframe = document.querySelector("iframe.docs-texteventtarget-iframe");
      if (!iframe) {
        return { ok: false, error: "Google Docs text input iframe not found" };
      }
      try {
        const doc = iframe.contentDocument || iframe.contentWindow?.document;
        const target =
          doc?.activeElement ||
          doc?.querySelector('[contenteditable="true"], textarea, input, [role="textbox"]') ||
          doc?.body ||
          doc?.documentElement;
        if (!doc || !target) {
          return { ok: false, error: "Google Docs text input target not found" };
        }
        if (typeof iframe.focus === "function") iframe.focus();
        if (doc.defaultView && typeof doc.defaultView.focus === "function") doc.defaultView.focus();
        if (typeof target.focus === "function") target.focus();

        const data = new DataTransfer();
        data.setData("text/plain", plainText);
        if (html) {
          data.setData("text/html", html);
        }
        const pasteEvent = new ClipboardEvent("paste", {
          clipboardData: data,
          bubbles: true,
          cancelable: true,
          composed: true,
        });
        target.dispatchEvent(pasteEvent);

        return {
          ok: true,
          mode: "google-docs-iframe-paste",
          target: {
            tag: target.tagName || "",
            id: target.id || "",
            className: typeof target.className === "string" ? target.className : "",
          },
        };
      } catch (e) {
        return { ok: false, error: e?.message || String(e) };
      }
    },
    [html, plainText]
  );
}

async function typeIntoGoogleDocsIframe(tabId, plainText) {
  return await executeScriptResult(tabId, (text) => {
    const iframe = document.querySelector("iframe.docs-texteventtarget-iframe");
    if (!iframe) {
      return { ok: false, error: "Google Docs text input iframe not found" };
    }
    try {
      const doc = iframe.contentDocument || iframe.contentWindow?.document;
      const target =
        doc?.activeElement ||
        doc?.querySelector('[contenteditable="true"]') ||
        doc?.body;
      if (!doc || !target) {
        return { ok: false, error: "Google Docs text input target not found" };
      }
      if (typeof iframe.focus === "function") iframe.focus();
      if (doc.defaultView && typeof doc.defaultView.focus === "function") doc.defaultView.focus();
      if (typeof target.focus === "function") target.focus();

      const chars = Array.from(text);
      for (const char of chars) {
        const keydown = new KeyboardEvent("keydown", {
          key: char,
          code: `Key${char.toUpperCase()}`,
          bubbles: true,
          cancelable: true,
          composed: true,
        });
        target.dispatchEvent(keydown);

        const keypress = new KeyboardEvent("keypress", {
          key: char,
          code: `Key${char.toUpperCase()}`,
          bubbles: true,
          cancelable: true,
          composed: true,
        });
        target.dispatchEvent(keypress);

        const keyup = new KeyboardEvent("keyup", {
          key: char,
          code: `Key${char.toUpperCase()}`,
          bubbles: true,
          cancelable: true,
          composed: true,
        });
        target.dispatchEvent(keyup);
      }

      return {
        ok: true,
        mode: "google-docs-keypress-type",
        charsTyped: chars.length,
      };
    } catch (e) {
      return { ok: false, error: e?.message || String(e) };
    }
  }, [plainText]);
}

async function insertTextViaExecCommand(tabId, text) {
  return await executeScriptResult(tabId, (txt) => {
    try {
      const success = document.execCommand("insertText", false, txt);
      return {
        ok: success,
        mode: success ? "execCommand-insertText" : "execCommand-failed",
        chars: txt.length,
      };
    } catch (e) {
      return { ok: false, error: e?.message || String(e) };
    }
  }, [text]);
}

function buildDocsResult(mode, plainText, clipboardWrite, beforeText, afterText, opts = {}) {
  const result = {
    ok: true,
    mode,
    length: plainText.length,
    clipboard: clipboardWrite.ok === true,
    verified: afterText !== beforeText,
  };
  if (clipboardWrite.mode) result.clipboard_mode = clipboardWrite.mode;
  if (clipboardWrite.error) result.clipboard_error = clipboardWrite.error;
  if (opts.plain_text_fallback) result.plain_text_fallback = true;
  if (opts.fallback_from) result.fallback_from = opts.fallback_from;
  return result;
}

async function pasteIntoGoogleDocs(tabId, html, plainText, clipboardWrite) {
  console.log("[sidekar] pasteIntoGoogleDocs called", { html: !!html, plainTextLength: plainText?.length });
  await clickGoogleDocsEditorViaDebugger(tabId);
  console.log("[sidekar] clickGoogleDocsEditorViaDebugger done");
  await delay(120);
  const before = await getDocsSnapshot(tabId);
  const annotated = await getGoogleDocsAnnotatedState(tabId);
  if (annotated?.ok) {
    const selection = annotated.selection;
    const anchor = selection
      ? Math.max(0, Math.min(Number(selection.end), annotated.text.length))
      : annotated.text.length;
    await setGoogleDocsAnnotatedSelection(tabId, anchor, anchor);
    await delay(40);
  }

  // HTML path: trusted paste first (preserves formatting via real clipboard)
  if (html) {
    if (!clipboardWrite.ok) {
      return {
        error: "Google Docs HTML paste requires clipboard write",
        clipboard: false,
        clipboard_error: clipboardWrite.error,
        clipboard_mode: clipboardWrite.mode,
      };
    }
    await focusGoogleDocsTextSink(tabId);
    const trusted = await trustedPasteViaDebugger(tabId);
    await delay(150);
    const after = await getDocsSnapshot(tabId);
    if (trusted.ok) {
      return buildDocsResult(trusted.mode, plainText, clipboardWrite, before, after, {
        plain_text_fallback: false,
      });
    }
    return {
      error: trusted.error || "Google Docs trusted paste failed",
      clipboard: true,
      clipboard_mode: clipboardWrite.mode,
      verified: after !== before,
    };
  }

  // Plain text: route through CLI's CDP for insertText (doesn't block user input)
  await focusGoogleDocsTextSink(tabId);
  const insertedText = await insertTextViaCliExec(tabId, plainText);
  await delay(150);
  const afterInsert = await getDocsSnapshot(tabId);
  const insertVerified = afterInsert !== before;
  if (insertedText.ok && insertVerified) {
    return buildDocsResult(insertedText.mode, plainText, clipboardWrite, before, afterInsert);
  }

  // Fallback: try synthetic iframe paste
  const docsPaste = await pasteIntoGoogleDocsTextSink(tabId, html, plainText);
  await delay(200);
  const afterDocsPaste = await getDocsSnapshot(tabId);
  const docsVerified = afterDocsPaste !== before;
  if (docsPaste.ok && docsVerified) {
    return buildDocsResult(docsPaste.mode, plainText, clipboardWrite, before, afterDocsPaste);
  }

  // Fallback: try execCommand insertText
  const execResult = await insertTextViaExecCommand(tabId, plainText);
  await delay(200);
  const afterExec = await getDocsSnapshot(tabId);
  const execVerified = afterExec !== before;
  if (execResult.ok && execVerified) {
    return buildDocsResult(execResult.mode, plainText, clipboardWrite, before, afterExec);
  }

  console.log("[sidekar] Final result - insertText:", insertedText, "docsPaste:", docsPaste, "execResult:", execResult);
  console.log("[sidekar] Verified status:", { insertVerified, docsVerified, execVerified });
  return {
    error: insertedText.error || docsPaste.error || execResult.error || "Google Docs insertion failed",
    insertText_error: insertedText.error,
    docsPaste_error: docsPaste.error,
    execCommand_error: execResult.error,
    clipboard: clipboardWrite.ok === true,
    clipboard_error: clipboardWrite.error,
    verified: insertVerified || docsVerified || execVerified,
  };
}

async function cmdPaste(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector || "";
  const html = typeof msg.html === "string" ? msg.html : "";
  const text = typeof msg.text === "string" && msg.text.length > 0 ? msg.text : "";
  const plainText = text || htmlToPlainText(html);

  if (!html && !plainText) {
    return { error: "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]" };
  }

  await focusTabWindow(tabId);

  const isGoogleDocs = await shouldPreferInsertText(tabId);
  // Google Docs plain-text path uses cli_exec inserttext, not the real clipboard — skip
  // offscreen (avoids "Offscreen clipboard write timed out" on broken/slow clipboard APIs).
  const clipboardWrite =
    html || !isGoogleDocs
      ? await writeClipboardContents(tabId, html, plainText)
      : { ok: false, stage: "clipboard", mode: "skipped-google-docs-plain" };

  if (isGoogleDocs) {
    return await pasteIntoGoogleDocs(tabId, html, plainText, clipboardWrite);
  }

  const inserted = await executeScriptResult(
    tabId,
    (selector, html, plainText, clipboardOk) => {
      function resolveTarget(selector) {
        if (!selector || selector === "active") {
          return descendIntoFrame(document.activeElement || document.body);
        }
        if (/^\d+$/.test(selector)) {
          return descendIntoFrame(document.querySelector(`[data-sidekar-ref="${selector}"]`));
        }
        return descendIntoFrame(document.querySelector(selector));
      }

      function descendIntoFrame(target) {
        if (!target || target.tagName !== "IFRAME") {
          return target;
        }
        try {
          const doc = target.contentDocument;
          if (!doc) return target;
          return (
            doc.activeElement ||
            doc.querySelector('[contenteditable="true"], textarea, input, [role="textbox"]') ||
            doc.body ||
            target
          );
        } catch {
          return target;
        }
      }

      function describeElement(el) {
        if (!el) return null;
        return {
          tag: el.tagName || "",
          id: el.id || "",
          className: typeof el.className === "string" ? el.className : "",
        };
      }

      function findMonacoEditor(target) {
        const monaco = window.monaco;
        if (!monaco || !monaco.editor || typeof monaco.editor.getEditors !== "function") {
          return null;
        }
        const editors = monaco.editor.getEditors();
        if (!Array.isArray(editors) || editors.length === 0) return null;
        if (!target) return editors[0] || null;
        for (const editor of editors) {
          const node = typeof editor.getDomNode === "function" ? editor.getDomNode() : null;
          if (node && (node === target || node.contains(target) || target.contains(node))) {
            return editor;
          }
        }
        return editors[0] || null;
      }

      const target = resolveTarget(selector);
      if (!target) {
        return { error: `Element not found: ${selector}` };
      }

      const doc = target.ownerDocument || document;
      const win = doc.defaultView || window;

      if (typeof target.focus === "function") {
        target.focus();
      }

      const monaco = findMonacoEditor(target);
      if (monaco) {
        try {
          const range = typeof monaco.getSelection === "function" ? monaco.getSelection() : null;
          if (range && typeof monaco.executeEdits === "function") {
            monaco.executeEdits("sidekar.ext.paste", [
              { range, text: plainText, forceMoveMarkers: true },
            ]);
          } else if (typeof monaco.setValue === "function") {
            monaco.setValue(plainText);
          } else {
            throw new Error("Monaco editor does not support write APIs");
          }
          if (typeof monaco.focus === "function") {
            monaco.focus();
          }
          return {
            ok: true,
            mode: "monaco",
            length: plainText.length,
            clipboard: clipboardOk,
            target: describeElement(target),
          };
        } catch (e) {
          return { error: e?.message || String(e) };
        }
      }

      const cm5Host = target.closest ? target.closest(".CodeMirror") : null;
      if (cm5Host && cm5Host.CodeMirror && typeof cm5Host.CodeMirror.replaceSelection === "function") {
        cm5Host.CodeMirror.focus();
        cm5Host.CodeMirror.replaceSelection(plainText);
        return {
          ok: true,
          mode: "codemirror5",
          length: plainText.length,
          clipboard: clipboardOk,
          target: describeElement(target),
        };
      }

      const tag = (target.tagName || "").toUpperCase();
      if (tag === "TEXTAREA" || tag === "INPUT") {
        const start = typeof target.selectionStart === "number" ? target.selectionStart : target.value.length;
        const end = typeof target.selectionEnd === "number" ? target.selectionEnd : target.value.length;
        if (typeof target.setRangeText === "function") {
          target.setRangeText(plainText, start, end, "end");
        } else {
          target.value = plainText;
        }
        const InputEvt = win.InputEvent || InputEvent;
        target.dispatchEvent(new InputEvt("input", { bubbles: true, data: plainText, inputType: "insertText" }));
        target.dispatchEvent(new Event("change", { bubbles: true }));
        return {
          ok: true,
          mode: "input",
          length: plainText.length,
          clipboard: clipboardOk,
          target: describeElement(target),
        };
      }

      if (target.isContentEditable) {
        let inserted = false;
        if (html && typeof doc.execCommand === "function") {
          try {
            inserted = doc.execCommand("insertHTML", false, html);
          } catch {}
        }
        if (!inserted && typeof doc.execCommand === "function") {
          try {
            inserted = doc.execCommand("insertText", false, plainText);
          } catch {}
        }
        if (!inserted) {
          const sel = win.getSelection ? win.getSelection() : window.getSelection();
          if (sel && sel.rangeCount > 0) {
            const range = sel.getRangeAt(0);
            range.deleteContents();
            if (html) {
              const tpl = doc.createElement("template");
              tpl.innerHTML = html;
              const frag = tpl.content.cloneNode(true);
              range.insertNode(frag);
            } else {
              range.insertNode(doc.createTextNode(plainText));
            }
            sel.collapseToEnd();
            inserted = true;
          } else {
            target.textContent = plainText;
            inserted = true;
          }
        }
        const InputEvt = win.InputEvent || InputEvent;
        target.dispatchEvent(new InputEvt("input", { bubbles: true, data: plainText, inputType: html ? "insertFromPaste" : "insertText" }));
        return {
          ok: true,
          mode: html ? "contenteditable-html" : "contenteditable-text",
          length: plainText.length,
          clipboard: clipboardOk,
          target: describeElement(target),
        };
      }

      let syntheticAttempted = false;
      let syntheticCanceled = false;
      const DataTransferCtor = win.DataTransfer || (typeof DataTransfer !== "undefined" ? DataTransfer : null);
      const ClipboardEventCtor = win.ClipboardEvent || (typeof ClipboardEvent !== "undefined" ? ClipboardEvent : null);
      if (DataTransferCtor && ClipboardEventCtor) {
        try {
          const dt = new DataTransferCtor();
          dt.setData("text/plain", plainText);
          if (html) {
            dt.setData("text/html", html);
          }
          const event = new ClipboardEventCtor("paste", {
            clipboardData: dt,
            bubbles: true,
            cancelable: true,
          });
          syntheticAttempted = true;
          syntheticCanceled = target.dispatchEvent(event) === false;
        } catch {}
      }

      if (!syntheticAttempted && typeof doc.execCommand === "function") {
        try {
          syntheticAttempted = doc.execCommand("paste");
        } catch {}
      }

      if (syntheticAttempted) {
        return {
          ok: true,
          mode: syntheticCanceled ? "synthetic-paste-canceled" : "synthetic-paste",
          length: plainText.length,
          clipboard: clipboardOk,
          verified: false,
          target: describeElement(target),
        };
      }

      return {
        error: "Could not paste into target. Try `sidekar ext eval-page` for a framework-specific write path.",
        clipboard: clipboardOk,
        target: describeElement(target),
      };
    },
    [selector, html, plainText, !clipboardWrite.error && clipboardWrite.ok === true],
    "MAIN"
  );

  if (inserted && !inserted.error && clipboardWrite.error) {
    inserted.clipboard_error = clipboardWrite.error;
  }
  if (inserted && inserted.error && clipboardWrite.error) {
    inserted.error = `${inserted.error} Clipboard write also failed: ${clipboardWrite.error}`;
  }

  const shouldTryTrustedPaste =
    clipboardWrite &&
    clipboardWrite.ok === true &&
    (!inserted ||
      inserted.error ||
      inserted.verified === false ||
      (typeof inserted.mode === "string" && inserted.mode.startsWith("synthetic")));

  if (shouldTryTrustedPaste) {
    const trusted = await trustedPasteViaDebugger(tabId);
    if (trusted.ok) {
      return {
        ok: true,
        mode: trusted.mode,
        length: text.length,
        clipboard: true,
        fallback_from: inserted && inserted.mode ? inserted.mode : inserted && inserted.error ? "error" : "none",
        target: inserted && inserted.target ? inserted.target : null,
        verified: true,
      };
    }
    const insertedText = await insertTextViaCliExec(tabId, plainText);
    if (insertedText.ok) {
      return {
        ok: true,
        mode: insertedText.mode,
        length: plainText.length,
        clipboard: clipboardWrite.ok === true,
        fallback_from: trusted.mode || (inserted && inserted.mode ? inserted.mode : inserted && inserted.error ? "error" : "none"),
        target: inserted && inserted.target ? inserted.target : null,
        verified: true,
        plain_text_fallback: !!html,
      };
    }
    if (inserted && !inserted.error) {
      inserted.debugger_error = trusted.error;
      inserted.insert_text_error = insertedText.error;
      return inserted;
    }
    return {
      error: trusted.error,
      clipboard: true,
      fallback_from: inserted && inserted.mode ? inserted.mode : inserted && inserted.error ? "error" : "none",
      target: inserted && inserted.target ? inserted.target : null,
      insert_text_error: insertedText.error,
    };
  }

  return inserted;
}

async function cmdSetValue(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector || "";
  const text = typeof msg.text === "string" ? msg.text : "";

  const isGoogleDocs = await shouldPreferInsertText(tabId);
  if (isGoogleDocs) {
    await focusTabWindow(tabId);
    await clickGoogleDocsEditorViaDebugger(tabId);
    await delay(120);
    await focusGoogleDocsTextSink(tabId);
    const before = await getDocsSnapshot(tabId);
    const insertedText = await insertTextViaCliExec(tabId, text);
    await delay(150);
    const after = await getDocsSnapshot(tabId);
    if (insertedText.ok) {
      return {
        ok: true,
        mode: insertedText.mode,
        length: text.length,
        verified: after !== before,
      };
    }
    return { error: insertedText.error };
  }

  return await executeScriptResult(
    tabId,
    (selector, text) => {
      function resolveTarget(selector) {
        if (!selector || selector === "active") {
          return document.activeElement || document.body;
        }
        if (/^\d+$/.test(selector)) {
          return document.querySelector(`[data-sidekar-ref="${selector}"]`);
        }
        return document.querySelector(selector);
      }

      function describeElement(el) {
        if (!el) return null;
        return {
          tag: el.tagName || "",
          id: el.id || "",
          className: typeof el.className === "string" ? el.className : "",
        };
      }

      function findMonacoEditor(target) {
        const monaco = window.monaco;
        if (!monaco || !monaco.editor || typeof monaco.editor.getEditors !== "function") {
          return null;
        }
        const editors = monaco.editor.getEditors();
        if (!Array.isArray(editors) || editors.length === 0) return null;
        if (!target) return editors[0] || null;
        for (const editor of editors) {
          const node = typeof editor.getDomNode === "function" ? editor.getDomNode() : null;
          if (node && (node === target || node.contains(target) || target.contains(node))) {
            return editor;
          }
        }
        return editors[0] || null;
      }

      const target = resolveTarget(selector);
      if (!target) {
        return { error: `Element not found: ${selector}` };
      }

      if (typeof target.focus === "function") {
        target.focus();
      }

      const monaco = findMonacoEditor(target);
      if (monaco && typeof monaco.setValue === "function") {
        monaco.setValue(text);
        if (typeof monaco.focus === "function") {
          monaco.focus();
        }
        return {
          ok: true,
          mode: "monaco",
          length: text.length,
          target: describeElement(target),
        };
      }

      const cm5Host = target.closest ? target.closest(".CodeMirror") : null;
      if (cm5Host && cm5Host.CodeMirror && typeof cm5Host.CodeMirror.setValue === "function") {
        cm5Host.CodeMirror.setValue(text);
        cm5Host.CodeMirror.focus();
        return {
          ok: true,
          mode: "codemirror5",
          length: text.length,
          target: describeElement(target),
        };
      }

      const tag = (target.tagName || "").toUpperCase();
      if (tag === "TEXTAREA" || tag === "INPUT") {
        target.value = text;
        target.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: "insertReplacementText" }));
        target.dispatchEvent(new Event("change", { bubbles: true }));
        return {
          ok: true,
          mode: "input",
          length: text.length,
          target: describeElement(target),
        };
      }

      if (target.isContentEditable) {
        target.textContent = text;
        target.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: "insertReplacementText" }));
        return {
          ok: true,
          mode: "contenteditable",
          length: text.length,
          target: describeElement(target),
        };
      }

      return {
        error: "No supported editor API found for target. Try `sidekar ext eval-page` for direct page-world access.",
        target: describeElement(target),
      };
    },
    [selector, text],
    "MAIN"
  );
}

async function cmdAxtree(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const out = await executeScriptResult(tabId, () => {
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
  });
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
  return await executeScriptResult(tabId, (direction) => {
      const amount = Math.round(window.innerHeight * 0.8);
      switch (direction) {
        case "up": window.scrollBy(0, -amount); break;
        case "down": window.scrollBy(0, amount); break;
        case "top": window.scrollTo(0, 0); break;
        case "bottom": window.scrollTo(0, document.body.scrollHeight); break;
      }
      return { scrolled: direction, y: window.scrollY };
    }, [direction]);
}

async function cmdEval(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  return await executeScriptResult(tabId, (code) => {
    try {
      return { result: String(eval(code)) };
    } catch (e) {
      return { error: e.message };
    }
  }, [msg.code]);
}

async function cmdEvalPage(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const code = typeof msg.code === "string" ? msg.code : "";
  if (!code) {
    return { error: "Usage: sidekar ext eval-page <javascript>" };
  }

  return await executeScriptResult(
    tabId,
    async (code) => {
      function normalize(value, depth = 0, seen = new WeakSet()) {
        if (value == null || typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
          return value;
        }
        if (typeof value === "bigint") {
          return value.toString();
        }
        if (typeof value === "function") {
          return `[Function ${value.name || "anonymous"}]`;
        }
        if (value instanceof Element) {
          return {
            tag: value.tagName,
            id: value.id || "",
            className: typeof value.className === "string" ? value.className : "",
            text: (value.innerText || value.textContent || "").substring(0, 200),
          };
        }
        if (depth >= 3) {
          return `[MaxDepth:${Object.prototype.toString.call(value)}]`;
        }
        if (typeof value === "object") {
          if (seen.has(value)) {
            return "[Circular]";
          }
          seen.add(value);
          if (Array.isArray(value)) {
            return value.slice(0, 50).map((item) => normalize(item, depth + 1, seen));
          }
          const out = {};
          for (const key of Object.keys(value).slice(0, 50)) {
            try {
              out[key] = normalize(value[key], depth + 1, seen);
            } catch (e) {
              out[key] = `[Thrown: ${e?.message || String(e)}]`;
            }
          }
          return out;
        }
        return String(value);
      }

      try {
        let result = (0, eval)(code);
        if (result && typeof result.then === "function") {
          result = await result;
        }
        return { result: normalize(result) };
      } catch (e) {
        return { error: e?.stack || e?.message || String(e) };
      }
    },
    [code],
    "MAIN"
  );
}

// ---------------------------------------------------------------------------
// History, Watch, Context commands
// ---------------------------------------------------------------------------

async function cmdHistory(msg) {
  const query = msg.query || "";
  const maxResults = msg.maxResults || 25;
  const results = await chrome.history.search({
    text: query,
    maxResults,
    startTime: 0,
  });
  return {
    entries: results.map((item) => ({
      url: item.url,
      title: item.title || "",
      lastVisitTime: item.lastVisitTime,
      visitCount: item.visitCount || 0,
    })),
  };
}

// Injection helper shared by initial watch setup and navigation re-injection.
async function injectWatchObserver(tabId, selector, watchId) {
  return await executeScriptResult(
    tabId,
    (selector, watchId, tabId) => {
      const el = document.querySelector(selector);
      if (!el) return { error: `Element not found: ${selector}` };

      // Store watchers globally in isolated world
      if (!globalThis.__sidekar_watchers) globalThis.__sidekar_watchers = new Map();

      // Remove existing watcher for this ID
      if (globalThis.__sidekar_watchers.has(watchId)) {
        try { globalThis.__sidekar_watchers.get(watchId).disconnect(); } catch {}
      }

      const initialState = el.innerText?.substring(0, 5000) || "";
      let lastState = initialState;
      let debounceTimer = null;

      const observer = new MutationObserver(() => {
        if (debounceTimer) clearTimeout(debounceTimer);
        debounceTimer = setTimeout(() => {
          debounceTimer = null;
          const newState = el.innerText?.substring(0, 5000) || "";
          if (newState !== lastState) {
            const prev = lastState;
            lastState = newState;
            chrome.runtime.sendMessage({
              type: "watch_event",
              watchId,
              selector,
              tabId,
              url: location.href,
              previous: prev.substring(0, 2000),
              current: newState.substring(0, 2000),
              timestamp: Date.now(),
            });
          }
        }, 500);
      });

      observer.observe(el, {
        childList: true,
        subtree: true,
        characterData: true,
        attributes: true,
      });
      globalThis.__sidekar_watchers.set(watchId, observer);

      return {
        ok: true,
        watchId,
        selector,
        url: location.href,
        initialState: initialState.substring(0, 2000),
        element: {
          tag: el.tagName,
          id: el.id || "",
          className: typeof el.className === "string" ? el.className : "",
        },
      };
    },
    [selector, watchId, tabId]
  );
}

async function cmdWatch(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector;
  if (!selector) return { error: "Usage: sidekar ext watch <selector>" };
  const watchId = msg.watchId || `w_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`;

  const result = await injectWatchObserver(tabId, selector, watchId);

  if (result && result.ok) {
    const tabUrl = result.url || "";
    let origin = "";
    try { origin = new URL(tabUrl).origin; } catch {}
    activeWatchers.set(watchId, { tabId, selector, origin });
  }
  return result;
}

async function cmdUnwatch(msg) {
  const watchId = msg.watchId;

  if (!watchId) {
    // Remove all watchers
    const removed = [];
    for (const [wid, info] of activeWatchers) {
      try {
        await executeScriptResult(info.tabId, (wid) => {
          if (globalThis.__sidekar_watchers && globalThis.__sidekar_watchers.has(wid)) {
            globalThis.__sidekar_watchers.get(wid).disconnect();
            globalThis.__sidekar_watchers.delete(wid);
          }
          return { ok: true };
        }, [wid]);
      } catch {}
      removed.push(wid);
    }
    activeWatchers.clear();
    return { ok: true, removed, count: removed.length };
  }

  const info = activeWatchers.get(watchId);
  if (!info) return { error: `Watch ${watchId} not found` };

  try {
    await executeScriptResult(info.tabId, (wid) => {
      if (globalThis.__sidekar_watchers && globalThis.__sidekar_watchers.has(wid)) {
        globalThis.__sidekar_watchers.get(wid).disconnect();
        globalThis.__sidekar_watchers.delete(wid);
      }
      return { ok: true };
    }, [watchId]);
  } catch {}

  activeWatchers.delete(watchId);
  return { ok: true, watchId };
}

async function cmdWatchers() {
  const watchers = [];
  for (const [watchId, info] of activeWatchers) {
    watchers.push({ watchId, tabId: info.tabId, selector: info.selector, origin: info.origin || "" });
  }
  return { watchers };
}

// Re-inject observers after a watched tab navigates. Only re-inject when the
// new URL stays on the same origin — otherwise the watcher silently unbinds.
chrome.tabs.onUpdated.addListener(async (tabId, changeInfo, tab) => {
  if (changeInfo.status !== "complete") return;
  if (activeWatchers.size === 0) return;

  const relevant = [];
  for (const [wid, info] of activeWatchers) {
    if (info.tabId === tabId) relevant.push({ wid, info });
  }
  if (relevant.length === 0) return;

  let newOrigin = "";
  try { newOrigin = new URL(tab.url || "").origin; } catch {}

  for (const { wid, info } of relevant) {
    if (info.origin && newOrigin && info.origin !== newOrigin) {
      // Cross-origin navigation — drop the watcher.
      activeWatchers.delete(wid);
      continue;
    }
    try {
      await injectWatchObserver(tabId, info.selector, wid);
    } catch (e) {
      console.warn("[sidekar] watch re-inject failed", wid, e?.message || e);
    }
  }
});

// Tab closed — drop its watchers.
chrome.tabs.onRemoved.addListener((tabId) => {
  for (const [wid, info] of activeWatchers) {
    if (info.tabId === tabId) activeWatchers.delete(wid);
  }
});

async function cmdContext() {
  // Gather active tab
  const [activeTab] = await chrome.tabs.query({ active: true, lastFocusedWindow: true });

  // Gather all tabs grouped by window
  const allTabs = await chrome.tabs.query({});
  const windows = {};
  for (const tab of allTabs) {
    const wid = tab.windowId;
    if (!windows[wid]) windows[wid] = [];
    windows[wid].push({
      id: tab.id,
      url: tab.url || "",
      title: tab.title || "",
      active: tab.active,
    });
  }

  // Recent history (last 15 entries from the past hour)
  const oneHourAgo = Date.now() - 60 * 60 * 1000;
  let recentHistory = [];
  try {
    const history = await chrome.history.search({
      text: "",
      maxResults: 15,
      startTime: oneHourAgo,
    });
    recentHistory = history.map((item) => ({
      url: item.url,
      title: item.title || "",
      lastVisitTime: item.lastVisitTime,
    }));
  } catch {}

  // Active watchers summary
  const watchers = [];
  for (const [watchId, info] of activeWatchers) {
    watchers.push({ watchId, tabId: info.tabId, selector: info.selector });
  }

  return {
    active_tab: activeTab
      ? { id: activeTab.id, url: activeTab.url, title: activeTab.title }
      : null,
    windows,
    tab_count: allTabs.length,
    window_count: Object.keys(windows).length,
    recent_history: recentHistory,
    watchers,
  };
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
  // Watch events from content scripts (injected MutationObservers) — forward to daemon for bus delivery.
  if (msg.type === "watch_event") {
    sendWs({
      type: "watch_event",
      watchId: msg.watchId,
      selector: msg.selector,
      tabId: msg.tabId,
      url: msg.url || "",
      previous: msg.previous,
      current: msg.current,
      timestamp: msg.timestamp || Date.now(),
    });
    return false;
  }
  if (msg.type === "colorScheme") {
    setIconForScheme(msg.dark);
    return false;
  }
  if (msg.type === "status") {
    sendResponse({
      connected: ws !== null && ws.readyState === WebSocket.OPEN,
      authenticated,
      lastError: lastConnectError,
      cliLoggedIn,
    });
    return false;
  }
  if (msg.type === "reconnect") {
    if (ws) {
      try { ws.close(); } catch {}
    }
    ws = null;
    authenticated = false;
    lastConnectError = null;
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
  const authUrl = "https://sidekar.dev/login?redirect=/ext-callback";

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
              if (!el) return null;
              const token = el.getAttribute("data-token");
              const profileStr = el.getAttribute("data-profile");
              let profile = null;
              try { profile = profileStr ? JSON.parse(profileStr) : null; } catch {}
              return { token, profile };
            },
          },
          (results) => {
            if (chrome.runtime.lastError) {
              console.error("[sidekar] Could not read token:", chrome.runtime.lastError);
              return;
            }

            const data =
              results && results[0] && results[0].result
                ? results[0].result
                : null;
            const token = data && data.token;
            const profile = data && data.profile;

            if (token && token.length > 0) {
              console.log("[sidekar] Got token from callback page");
              const toStore = { extToken: token };
              if (profile) toStore.extProfile = profile;
              chrome.storage.local.set(toStore, () => {
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
  const offscreenUrl = chrome.runtime.getURL("offscreen.html");
  const contexts = await chrome.runtime.getContexts({
    contextTypes: ["OFFSCREEN_DOCUMENT"],
    documentUrls: [offscreenUrl],
  });
  if (contexts.length === 0) {
    if (!creatingOffscreen) {
      creatingOffscreen = chrome.offscreen.createDocument({
        url: "offscreen.html",
        reasons: ["MATCH_MEDIA", "CLIPBOARD"],
        justification: "Detect theme and write clipboard content for browser automation",
      }).finally(() => {
        creatingOffscreen = null;
      });
    }
    await creatingOffscreen;
  }
}

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

chrome.alarms.create("keepalive", { periodInMinutes: 0.4 });

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === "keepalive") {
    if (!ws || ws.readyState !== WebSocket.OPEN) {
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
