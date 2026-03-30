function sendColorScheme() {
  try {
    const dark = !!window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
    chrome.runtime.sendMessage({ type: "colorScheme", dark });
  } catch {}
}

function ensureClipboardSink() {
  let sink = document.getElementById("sidekar-offscreen-clipboard-sink");
  if (sink) return sink;

  sink = document.createElement("div");
  sink.id = "sidekar-offscreen-clipboard-sink";
  sink.contentEditable = "true";
  sink.tabIndex = -1;
  sink.setAttribute("aria-hidden", "true");
  sink.style.position = "fixed";
  sink.style.left = "-9999px";
  sink.style.top = "0";
  sink.style.opacity = "0";
  sink.style.pointerEvents = "none";
  sink.style.whiteSpace = "pre-wrap";
  document.body.appendChild(sink);
  return sink;
}

/** Offscreen documents may not fire rAF reliably; always bound wait time. */
function nextFrame() {
  return Promise.race([
    new Promise((resolve) => requestAnimationFrame(() => resolve())),
    new Promise((resolve) => setTimeout(resolve, 32)),
  ]);
}

async function execCopy(html, plainText) {
  const sink = ensureClipboardSink();
  const text = String(plainText || "");
  const richHtml = String(html || "");

  sink.innerHTML = "";
  if (richHtml) {
    sink.innerHTML = richHtml;
  } else {
    sink.textContent = text;
  }

  const selection = window.getSelection();
  const range = document.createRange();
  range.selectNodeContents(sink);
  selection?.removeAllRanges();
  selection?.addRange(range);

  sink.focus();
  await nextFrame();

  return new Promise((resolve) => {
    let finished = false;
    const finish = (result) => {
      if (finished) return;
      finished = true;
      document.removeEventListener("copy", onCopy, true);
      selection?.removeAllRanges();
      sink.blur();
      sink.innerHTML = "";
      resolve(result);
    };

    const onCopy = (event) => {
      try {
        event.preventDefault();
        event.clipboardData?.setData("text/plain", text);
        if (richHtml) {
          event.clipboardData?.setData("text/html", richHtml);
        }
        finish({ ok: true, mode: "execCommand-copy" });
      } catch (e) {
        finish({ ok: false, error: e?.message || String(e), mode: "execCommand-copy" });
      }
    };

    document.addEventListener("copy", onCopy, true);

    try {
      const copied = document.execCommand("copy");
      if (!copied) {
        finish({ ok: false, error: "document.execCommand(copy) returned false", mode: "execCommand-copy" });
      } else {
        setTimeout(() => {
          finish({ ok: false, error: "copy event did not fire", mode: "execCommand-copy" });
        }, 50);
      }
    } catch (e) {
      finish({ ok: false, error: e?.message || String(e), mode: "execCommand-copy" });
    }
  });
}

const NAV_CLIPBOARD_TIMEOUT_MS = 8000;

function withTimeout(promise, ms, label) {
  return Promise.race([
    promise,
    new Promise((_, reject) =>
      setTimeout(() => reject(new Error(`${label} timed out after ${ms}ms`)), ms)
    ),
  ]);
}

async function writeClipboard(html, plainText) {
  const execResult = await execCopy(html, plainText);
  if (execResult.ok) {
    return execResult;
  }

  if (!navigator.clipboard) {
    return execResult;
  }

  try {
    if (html) {
      if (typeof ClipboardItem === "undefined") {
        return execResult;
      }
      const item = new ClipboardItem({
        "text/html": new Blob([html], { type: "text/html" }),
        "text/plain": new Blob([plainText], { type: "text/plain" }),
      });
      await withTimeout(
        navigator.clipboard.write([item]),
        NAV_CLIPBOARD_TIMEOUT_MS,
        "navigator.clipboard.write"
      );
    } else {
      await withTimeout(
        navigator.clipboard.writeText(plainText),
        NAV_CLIPBOARD_TIMEOUT_MS,
        "navigator.clipboard.writeText"
      );
    }
    return { ok: true, mode: "navigator-clipboard" };
  } catch (e) {
    return {
      ok: false,
      error: execResult.error || e?.message || String(e),
      mode: execResult.mode || "navigator-clipboard",
      async_error: e?.message || String(e),
    };
  }
}

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  if (msg?.target !== "offscreen") {
    return false;
  }

  if (msg.type === "offscreenPing") {
    sendResponse({ ok: true, pong: true });
    return true;
  }

  if (msg.type === "writeClipboard") {
    const deadlineMs = 9500;
    Promise.race([
      writeClipboard(String(msg.html || ""), String(msg.plainText || "")),
      new Promise((resolve) =>
        setTimeout(
          () =>
            resolve({
              ok: false,
              error: "writeClipboard exceeded offscreen deadline",
              mode: "offscreen-deadline",
            }),
          deadlineMs
        )
      ),
    ])
      .then((result) => sendResponse(result))
      .catch((e) => sendResponse({ ok: false, error: e?.message || String(e) }));
    return true;
  }

  return false;
});

sendColorScheme();
try {
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  if (typeof media.addEventListener === "function") {
    media.addEventListener("change", sendColorScheme);
  } else if (typeof media.addListener === "function") {
    media.addListener(sendColorScheme);
  }
} catch {}
