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

function clearStoredExtToken() {
  return new Promise((resolve) => {
    chrome.storage.local.remove(["extToken"], () => resolve());
  });
}

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
    if (!lastConnectError) {
      lastConnectError = "Sign in from the extension popup";
    }
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
      const reason = msg.reason ||
        "Authentication failed — try logging in again from the extension popup.";
      lastConnectError = reason;
      if (reason.includes("invalid ext token")) {
        await clearStoredExtToken();
        lastConnectError = "Extension session expired — sign in again from the extension popup.";
      }
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
          lastConnectError = "Run `sidekar login`";
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
    }, [selector, text, refNum]);
}

async function cmdPaste(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector || "";
  const html = typeof msg.html === "string" ? msg.html : "";
  const text = typeof msg.text === "string" && msg.text.length > 0 ? msg.text : html;

  if (!html && !text) {
    return { error: "Usage: sidekar ext paste [--html <html>] [--text <text>] [--selector <selector>]" };
  }

  const clipboardWrite = await executeScriptResult(
    tabId,
    async (html, text) => {
      try {
        if (!navigator.clipboard) {
          return { ok: false, stage: "clipboard", error: "navigator.clipboard unavailable" };
        }
        if (html) {
          if (typeof ClipboardItem === "undefined") {
            return { ok: false, stage: "clipboard", error: "ClipboardItem unavailable" };
          }
          const item = new ClipboardItem({
            "text/html": new Blob([html], { type: "text/html" }),
            "text/plain": new Blob([text], { type: "text/plain" }),
          });
          await navigator.clipboard.write([item]);
        } else {
          await navigator.clipboard.writeText(text);
        }
        return { ok: true, stage: "clipboard" };
      } catch (e) {
        return { ok: false, stage: "clipboard", error: e?.message || String(e) };
      }
    },
    [html, text]
  );

  const inserted = await executeScriptResult(
    tabId,
    (selector, html, text, clipboardOk) => {
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
      if (monaco) {
        try {
          const range = typeof monaco.getSelection === "function" ? monaco.getSelection() : null;
          if (range && typeof monaco.executeEdits === "function") {
            monaco.executeEdits("sidekar.ext.paste", [
              { range, text, forceMoveMarkers: true },
            ]);
          } else if (typeof monaco.setValue === "function") {
            monaco.setValue(text);
          } else {
            throw new Error("Monaco editor does not support write APIs");
          }
          if (typeof monaco.focus === "function") {
            monaco.focus();
          }
          return {
            ok: true,
            mode: "monaco",
            length: text.length,
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
        cm5Host.CodeMirror.replaceSelection(text);
        return {
          ok: true,
          mode: "codemirror5",
          length: text.length,
          clipboard: clipboardOk,
          target: describeElement(target),
        };
      }

      const tag = (target.tagName || "").toUpperCase();
      if (tag === "TEXTAREA" || tag === "INPUT") {
        const start = typeof target.selectionStart === "number" ? target.selectionStart : target.value.length;
        const end = typeof target.selectionEnd === "number" ? target.selectionEnd : target.value.length;
        if (typeof target.setRangeText === "function") {
          target.setRangeText(text, start, end, "end");
        } else {
          target.value = text;
        }
        target.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: "insertText" }));
        target.dispatchEvent(new Event("change", { bubbles: true }));
        return {
          ok: true,
          mode: "input",
          length: text.length,
          clipboard: clipboardOk,
          target: describeElement(target),
        };
      }

      if (target.isContentEditable) {
        let inserted = false;
        if (html && typeof document.execCommand === "function") {
          inserted = document.execCommand("insertHTML", false, html);
        }
        if (!inserted && typeof document.execCommand === "function") {
          inserted = document.execCommand("insertText", false, text);
        }
        if (!inserted) {
          const sel = window.getSelection();
          if (sel && sel.rangeCount > 0) {
            const range = sel.getRangeAt(0);
            range.deleteContents();
            if (html) {
              const tpl = document.createElement("template");
              tpl.innerHTML = html;
              const frag = tpl.content.cloneNode(true);
              range.insertNode(frag);
            } else {
              range.insertNode(document.createTextNode(text));
            }
            sel.collapseToEnd();
            inserted = true;
          } else {
            target.textContent = text;
            inserted = true;
          }
        }
        target.dispatchEvent(new InputEvent("input", { bubbles: true, data: text, inputType: html ? "insertFromPaste" : "insertText" }));
        return {
          ok: true,
          mode: html ? "contenteditable-html" : "contenteditable-text",
          length: text.length,
          clipboard: clipboardOk,
          target: describeElement(target),
        };
      }

      let syntheticAttempted = false;
      let syntheticCanceled = false;
      if (typeof DataTransfer !== "undefined" && typeof ClipboardEvent !== "undefined") {
        try {
          const dt = new DataTransfer();
          dt.setData("text/plain", text);
          if (html) {
            dt.setData("text/html", html);
          }
          const event = new ClipboardEvent("paste", {
            clipboardData: dt,
            bubbles: true,
            cancelable: true,
          });
          syntheticAttempted = true;
          syntheticCanceled = target.dispatchEvent(event) === false;
        } catch {}
      }

      if (!syntheticAttempted && typeof document.execCommand === "function") {
        try {
          syntheticAttempted = document.execCommand("paste");
        } catch {}
      }

      if (syntheticAttempted) {
        return {
          ok: true,
          mode: syntheticCanceled ? "synthetic-paste-canceled" : "synthetic-paste",
          length: text.length,
          clipboard: clipboardOk,
          verified: false,
          target: describeElement(target),
        };
      }

      return {
        error: "Could not paste into target. Try `sidekar ext evalpage` for a framework-specific write path.",
        clipboard: clipboardOk,
        target: describeElement(target),
      };
    },
    [selector, html, text, !clipboardWrite.error && clipboardWrite.ok === true],
    "MAIN"
  );

  if (inserted && !inserted.error && clipboardWrite.error) {
    inserted.clipboard_error = clipboardWrite.error;
  }
  if (inserted && inserted.error && clipboardWrite.error) {
    inserted.error = `${inserted.error} Clipboard write also failed: ${clipboardWrite.error}`;
  }

  return inserted;
}

async function cmdSetValue(msg) {
  const tabId = msg.tabId || (await getActiveTabId());
  const selector = msg.selector || "";
  const text = typeof msg.text === "string" ? msg.text : "";

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
        error: "No supported editor API found for target. Try `sidekar ext evalpage` for direct page-world access.",
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
    return { error: "Usage: sidekar ext evalpage <javascript>" };
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
        // If CLI is now logged in but we're not connected, reconnect
        if (cliLoggedIn && (!ws || ws.readyState !== 1)) {
          lastConnectError = null;
          connect();
        }
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
