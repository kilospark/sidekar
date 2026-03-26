/**
 * Themed dialogs (no window.alert / window.confirm). Uses page CSS variables (--bg, --border, etc.).
 * Globals: sidekarAlert, sidekarConfirm
 */
(function (global) {
  var STYLE_ID = "sidekar-modal-styles";
  var openCount = 0;
  var lastActive = null;

  function injectStyles() {
    if (document.getElementById(STYLE_ID)) return;
    var s = document.createElement("style");
    s.id = STYLE_ID;
    s.textContent =
      ".sidekar-modal-overlay{position:fixed;inset:0;background:rgba(0,0,0,.55);z-index:10000;display:flex;align-items:center;justify-content:center;padding:24px;backdrop-filter:blur(6px);-webkit-backdrop-filter:blur(6px);animation:sidekar-modal-in .15s ease-out;}@keyframes sidekar-modal-in{from{opacity:0}to{opacity:1}}" +
      ".sidekar-modal{max-width:420px;width:100%;background:var(--bg-subtle);border:1px solid var(--border);border-radius:12px;padding:24px;box-shadow:0 25px 50px -12px rgba(0,0,0,.45);animation:sidekar-modal-pop .18s ease-out;}@keyframes sidekar-modal-pop{from{opacity:0;transform:scale(.96)}to{opacity:1;transform:scale(1)}}" +
      ".sidekar-modal-title{font-size:16px;font-weight:600;color:var(--text);margin:0 0 0 0;letter-spacing:-.02em;}" +
      ".sidekar-modal-body{font-size:14px;color:var(--text-secondary);line-height:1.55;margin:12px 0 0 0;}" +
      ".sidekar-modal-actions{display:flex;gap:10px;justify-content:flex-end;margin-top:22px;flex-wrap:wrap;}" +
      ".sidekar-modal-btn{font-family:inherit;font-size:13px;font-weight:500;padding:8px 16px;border-radius:8px;cursor:pointer;border:1px solid var(--border);background:transparent;color:var(--text-secondary);transition:border-color .15s,color .15s,background .15s;}" +
      ".sidekar-modal-btn:hover{border-color:var(--border-subtle);color:var(--text);background:var(--bg-muted);}" +
      ".sidekar-modal-btn-primary{border-color:var(--border-subtle);color:var(--text);background:var(--bg-muted);}" +
      ".sidekar-modal-btn-primary:hover{background:var(--bg);}" +
      ".sidekar-modal-btn-danger{border-color:#7f1d1d;color:#fca5a5;background:rgba(127,29,29,.2);}" +
      ".sidekar-modal-btn-danger:hover{border-color:#b91c1c;color:#fecaca;background:rgba(185,28,28,.28);}" +
      "html.light .sidekar-modal-btn-danger{border-color:#fecaca;color:#b91c1c;background:rgba(254,202,202,.35);}" +
      "html.light .sidekar-modal-btn-danger:hover{border-color:#f87171;background:rgba(254,202,202,.55);}";
    document.head.appendChild(s);
  }

  function trapFocus(overlay, root) {
    var focusables = root.querySelectorAll(
      'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
    );
    var list = Array.prototype.slice.call(focusables).filter(function (el) {
      return el.offsetParent !== null || el === document.activeElement;
    });
    if (list.length === 0) return function () {};
    var first = list[0];
    var last = list[list.length - 1];
    function onKey(e) {
      if (e.key !== "Tab") return;
      if (e.shiftKey) {
        if (document.activeElement === first) {
          e.preventDefault();
          last.focus();
        }
      } else {
        if (document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    }
    return onKey;
  }

  function openModal() {
    openCount++;
    if (openCount === 1) {
      lastActive = document.activeElement;
      document.body.style.overflow = "hidden";
    }
  }

  function closeModal() {
    openCount = Math.max(0, openCount - 1);
    if (openCount === 0) {
      document.body.style.overflow = "";
      if (lastActive && typeof lastActive.focus === "function") {
        try {
          lastActive.focus();
        } catch (_) {}
      }
      lastActive = null;
    }
  }

  /**
   * @param {string|{title?:string,message:string}} opts
   * @returns {Promise<void>}
   */
  global.sidekarAlert = function (opts) {
    var message = typeof opts === "string" ? opts : opts && opts.message;
    var title = typeof opts === "object" && opts && opts.title;
    if (!message) message = "";

    return new Promise(function (resolve) {
      injectStyles();
      openModal();

      var overlay = document.createElement("div");
      overlay.className = "sidekar-modal-overlay";
      overlay.setAttribute("role", "dialog");
      overlay.setAttribute("aria-modal", "true");
      overlay.setAttribute("aria-labelledby", "sidekar-modal-alert-title");

      var box = document.createElement("div");
      box.className = "sidekar-modal";

      var titleEl = document.createElement("h2");
      titleEl.id = "sidekar-modal-alert-title";
      titleEl.className = "sidekar-modal-title";
      titleEl.textContent = title || "Notice";

      var body = document.createElement("p");
      body.className = "sidekar-modal-body";
      body.textContent = message;

      var actions = document.createElement("div");
      actions.className = "sidekar-modal-actions";
      var ok = document.createElement("button");
      ok.type = "button";
      ok.className = "sidekar-modal-btn sidekar-modal-btn-primary";
      ok.textContent = "OK";

      function cleanup() {
        document.removeEventListener("keydown", onDocKey);
        document.removeEventListener("keydown", tabTrap);
        overlay.remove();
        closeModal();
        resolve();
      }

      function onDocKey(e) {
        if (e.key === "Escape") {
          e.preventDefault();
          cleanup();
        }
      }

      actions.appendChild(ok);
      box.appendChild(titleEl);
      box.appendChild(body);
      box.appendChild(actions);
      overlay.appendChild(box);
      document.body.appendChild(overlay);

      var tabTrap = trapFocus(overlay, box);

      ok.addEventListener("click", cleanup);
      overlay.addEventListener("click", function (e) {
        if (e.target === overlay) cleanup();
      });

      document.addEventListener("keydown", onDocKey);
      document.addEventListener("keydown", tabTrap);
      setTimeout(function () {
        ok.focus();
      }, 0);
    });
  };

  /**
   * @param {{title?:string,message:string,confirmLabel?:string,cancelLabel?:string,danger?:boolean}} opts
   * @returns {Promise<boolean>}
   */
  global.sidekarConfirm = function (opts) {
    opts = opts || {};
    var message = opts.message || "";
    var title = opts.title || "Confirm";
    var confirmLabel = opts.confirmLabel || "OK";
    var cancelLabel = opts.cancelLabel || "Cancel";
    var danger = !!opts.danger;

    return new Promise(function (resolve) {
      injectStyles();
      openModal();

      var overlay = document.createElement("div");
      overlay.className = "sidekar-modal-overlay";
      overlay.setAttribute("role", "dialog");
      overlay.setAttribute("aria-modal", "true");
      overlay.setAttribute("aria-labelledby", "sidekar-modal-confirm-title");

      var box = document.createElement("div");
      box.className = "sidekar-modal";

      var titleEl = document.createElement("h2");
      titleEl.id = "sidekar-modal-confirm-title";
      titleEl.className = "sidekar-modal-title";
      titleEl.textContent = title;

      var body = document.createElement("p");
      body.className = "sidekar-modal-body";
      body.textContent = message;

      var actions = document.createElement("div");
      actions.className = "sidekar-modal-actions";

      var cancelBtn = document.createElement("button");
      cancelBtn.type = "button";
      cancelBtn.className = "sidekar-modal-btn";
      cancelBtn.textContent = cancelLabel;

      var confirmBtn = document.createElement("button");
      confirmBtn.type = "button";
      confirmBtn.className =
        "sidekar-modal-btn " + (danger ? "sidekar-modal-btn-danger" : "sidekar-modal-btn-primary");
      confirmBtn.textContent = confirmLabel;

      var settled = false;
      function finish(value) {
        if (settled) return;
        settled = true;
        document.removeEventListener("keydown", onDocKey);
        document.removeEventListener("keydown", tabTrap);
        overlay.remove();
        closeModal();
        resolve(value);
      }

      function onDocKey(e) {
        if (e.key === "Escape") {
          e.preventDefault();
          finish(false);
        }
      }

      actions.appendChild(cancelBtn);
      actions.appendChild(confirmBtn);
      box.appendChild(titleEl);
      box.appendChild(body);
      box.appendChild(actions);
      overlay.appendChild(box);
      document.body.appendChild(overlay);

      var tabTrap = trapFocus(overlay, box);

      cancelBtn.addEventListener("click", function () {
        finish(false);
      });
      confirmBtn.addEventListener("click", function () {
        finish(true);
      });
      overlay.addEventListener("click", function (e) {
        if (e.target === overlay) finish(false);
      });

      document.addEventListener("keydown", onDocKey);
      document.addEventListener("keydown", tabTrap);
      setTimeout(function () {
        cancelBtn.focus();
      }, 0);
    });
  };
})(typeof window !== "undefined" ? window : this);
