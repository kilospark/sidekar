pub const PAGE_BRIEF_SCRIPT: &str = r#"(function() {
  function qsa(sel) {
    const results = [...document.querySelectorAll(sel)];
    (function walk(root) {
      for (const el of root.querySelectorAll('*')) {
        if (el.shadowRoot) {
          results.push(...el.shadowRoot.querySelectorAll(sel));
          walk(el.shadowRoot);
        }
      }
    })(document);
    return results;
  }
  const t = document.title, u = location.href;
  const seen = new Set();
  const inputs = [], buttons = [], links = [];
  qsa('input:not([type=hidden]),textarea,select').forEach(el => {
    if (!el.offsetParent && getComputedStyle(el).display === 'none') return;
    if (inputs.length >= 5) return;
    const key = el.name || el.id || el.type;
    if (seen.has(key)) return;
    seen.add(key);
    const a = [el.tagName.toLowerCase()];
    if (el.name) a.push('name=' + el.name);
    if (el.type && el.type !== 'text') a.push('type=' + el.type);
    if (el.placeholder) a.push(JSON.stringify(el.placeholder.substring(0, 40)));
    inputs.push('[' + a.join(' ') + ']');
  });
  qsa('button,[role=button],input[type=submit]').forEach(el => {
    if (!el.offsetParent && getComputedStyle(el).display === 'none') return;
    if (buttons.length >= 5) return;
    const txt = (el.textContent || el.value || '').trim().substring(0, 30);
    if (!txt || txt.includes('{') || seen.has(txt)) return;
    seen.add(txt);
    buttons.push('[button ' + JSON.stringify(txt) + ']');
  });
  qsa('a[href]').forEach(el => {
    if (!el.offsetParent) return;
    if (links.length >= 8) return;
    const txt = el.textContent.trim().substring(0, 25);
    if (txt && !seen.has(txt)) { seen.add(txt); links.push(txt); }
  });
  const totalInputs = qsa('input:not([type=hidden]),textarea,select').length;
  const totalButtons = qsa('button,[role=button],input[type=submit]').length;
  const totalLinks = qsa('a[href]').length;
  const short = u.length > 80 ? u.substring(0, 80) + '...' : u;
  let r = '--- ' + short + ' | ' + t + ' ---';
  if (inputs.length) r += '\n' + inputs.join(' ');
  if (buttons.length) r += '\n' + buttons.join(' ');
  if (links.length) r += '\nLinks: ' + links.join(', ');
  const dialogs = qsa('[role=dialog],[role=alertdialog],[aria-modal=true],[aria-modal="true"],.modal,.modal-dialog');
  const visibleDialogs = dialogs.filter(el => el.offsetParent !== null || getComputedStyle(el).position === 'fixed');
  if (visibleDialogs.length) {
    const labels = visibleDialogs.slice(0, 3).map(el => {
      const lbl = el.getAttribute('aria-label') || el.querySelector('[class*=title],[class*=header],h1,h2,h3')?.textContent?.trim()?.substring(0, 40) || '';
      return lbl ? 'Dialog: ' + JSON.stringify(lbl) : 'Dialog visible';
    });
    r += '\n' + labels.join('\n');
  }
  const alerts = qsa('[role=alert],[role=status]');
  const visibleAlerts = alerts.filter(el => {
    if (!el.offsetParent && getComputedStyle(el).display === 'none') return false;
    const txt = (el.textContent || '').trim();
    return txt.length > 0 && txt.length < 200;
  });
  if (visibleAlerts.length) {
    const msgs = visibleAlerts.slice(0, 3).map(el => {
      const role = el.getAttribute('role');
      const prefix = role === 'alert' ? 'Alert' : role === 'status' ? 'Status' : 'Notice';
      return prefix + ': ' + JSON.stringify(el.textContent.trim().substring(0, 80));
    });
    r += '\n' + msgs.join('\n');
  }
  const counts = [];
  if (totalInputs > inputs.length) counts.push(totalInputs + ' inputs');
  if (totalButtons > buttons.length) counts.push(totalButtons + ' buttons');
  if (totalLinks > links.length) counts.push(totalLinks + ' links');
  if (counts.length) r += '\n(' + counts.join(', ') + ' total — use dom or ax-tree for full list)';
  return r;
})()"#;

pub const SELECTOR_GEN_SCRIPT: &str = r#"if (!window.__sidekarGenSelector) {
  window.__sidekarGenSelector = function(el) {
    if (el.id) {
      try {
        var sel = '#' + CSS.escape(el.id);
        if (document.querySelectorAll(sel).length === 1) return sel;
      } catch(e) {}
    }
    var tid = el.getAttribute('data-testid');
    if (tid && tid.indexOf('"') < 0 && tid.indexOf(']') < 0) {
      var sel = '[data-testid="' + tid + '"]';
      try { if (document.querySelectorAll(sel).length === 1) return sel; } catch(e) {}
    }
    var al = el.getAttribute('aria-label');
    if (al && al.indexOf('"') < 0 && al.indexOf(']') < 0) {
      var sel = '[aria-label="' + al + '"]';
      try { if (document.querySelectorAll(sel).length === 1) return sel; } catch(e) {}
    }
    var parts = [];
    var cur = el;
    while (cur && cur !== document.body && cur !== document.documentElement) {
      var tag = cur.tagName.toLowerCase();
      var parent = cur.parentElement;
      if (parent) {
        var siblings = Array.from(parent.children).filter(function(c) { return c.tagName === cur.tagName; });
        if (siblings.length > 1) tag += ':nth-of-type(' + (siblings.indexOf(cur) + 1) + ')';
      }
      parts.unshift(tag);
      cur = parent;
    }
    return parts.join(' > ');
  };
}"#;

pub const DISMISS_POPUPS_SCRIPT: &str = r#"(function() {
    const selectors = [
        '#onetrust-accept-btn-handler',
        '#CookieBoxSaveButton',
        '[data-testid="cookie-policy-manage-dialog-accept-button"]',
        '.cc-accept', '.cc-dismiss',
        '#accept-cookies', '#cookie-accept',
        '#cookie-consent-accept', '#cookies-accept',
        '[data-cookiefirst-action="accept"]',
        '.js-cookie-consent-agree',
        '#truste-consent-button',
        '#didomi-notice-agree-button',
    ];
    for (const sel of selectors) {
        const el = document.querySelector(sel);
        if (el && el.offsetParent !== null) { el.click(); return 'dismissed:' + sel; }
    }
    const textPatterns = [
        /^accept\s*(all|cookies)?$/i,
        /^(i\s+)?agree$/i,
        /^got\s*it$/i,
        /^(ok|okay)$/i,
        /^allow\s*(all|cookies)?$/i,
        /^close$/i,
    ];
    const buttons = document.querySelectorAll('button, [role="button"], a.button, a.btn');
    for (const btn of buttons) {
        const text = (btn.textContent || '').trim();
        if (text.length > 30) continue;
        for (const pat of textPatterns) {
            if (pat.test(text) && btn.offsetParent !== null) {
                btn.click();
                return 'dismissed:text:' + text;
            }
        }
    }
    return 'none';
})()"#;

pub fn deep_query_expr(selector: &str) -> std::result::Result<String, anyhow::Error> {
    Ok(format!(
        r#"(function() {{
          const sel = {sel};
          function find(root) {{
            try {{
              const direct = root.querySelector(sel);
              if (direct) return direct;
            }} catch (e) {{
              return {{ error: 'Invalid CSS selector: ' + sel + '. Use CSS selectors (#id, .class, tag).' }};
            }}
            for (const el of root.querySelectorAll('*')) {{
              if (el.shadowRoot) {{
                const found = find(el.shadowRoot);
                if (found) return found;
              }}
            }}
            return null;
          }}
          return find(document);
        }})()"#,
        sel = serde_json::to_string(selector)?
    ))
}
