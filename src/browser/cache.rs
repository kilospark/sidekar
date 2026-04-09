use super::*;

#[derive(Debug)]
pub struct InteractiveData {
    pub elements: Vec<InteractiveElement>,
    pub output: String,
}

pub async fn fetch_interactive_elements(
    ctx: &mut AppContext,
    cdp: &mut CdpClient,
) -> Result<InteractiveData> {
    let current_url_result = runtime_evaluate(cdp, "location.href", true, false).await?;
    let current_url = current_url_result
        .pointer("/result/value")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cache_key = cache_key_from_url(&current_url);

    let mut action_cache = load_action_cache(ctx)?;
    if let Some(cached) = action_cache.get(&cache_key).cloned() {
        if now_epoch_ms() - cached.timestamp < CACHE_TTL_MS && !cached.ref_map.is_empty() {
            let refs_to_check = cached.ref_map.values().take(3).cloned().collect::<Vec<_>>();
            let mut valid = !refs_to_check.is_empty();
            for sel in refs_to_check {
                let check = runtime_evaluate(
                    cdp,
                    &format!("!!document.querySelector({})", serde_json::to_string(&sel)?),
                    true,
                    false,
                )
                .await?;
                if !check
                    .pointer("/result/value")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    valid = false;
                    break;
                }
            }
            if valid {
                let overlay_check = runtime_evaluate(
                    cdp,
                    "document.querySelectorAll('[role=dialog],[role=alertdialog],[role=menu],[role=listbox],[aria-modal=true],[aria-modal=\"true\"],.modal,.modal-dialog,.drawer,.popover,[data-modal],[data-state=open],[data-headlessui-state~=open]').length",
                    true,
                    false,
                )
                .await?;
                let overlay_count = overlay_check
                    .pointer("/result/value")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                if overlay_count > 0 {
                    valid = false;
                }
            }
            if valid {
                let mut state = ctx.load_session_state()?;
                state.prev_elements = state.current_elements.clone();
                state.current_elements = Some(cached.elements.clone());
                state.ref_map = Some(cached.ref_map.clone());
                state.ref_map_url = Some(current_url);
                state.ref_map_timestamp = Some(cached.timestamp);
                ctx.save_session_state(&state)?;
                return Ok(InteractiveData {
                    elements: cached.elements,
                    output: cached.output,
                });
            }
        }
    }

    let script = AXTREE_INTERACTIVE_SCRIPT.replace("__SIDEKAR_SELECTOR_GEN__", SELECTOR_GEN_SCRIPT);
    let context_id = get_frame_context_id(ctx, cdp).await?;
    let result = runtime_evaluate_with_context(cdp, &script, true, false, context_id).await?;
    let items = result
        .pointer("/result/value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut elements = Vec::new();
    let mut ref_map = HashMap::new();
    let mut lines = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let ref_id = idx + 1;
        let selector = item
            .get("selector")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let role = item
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("element")
            .to_string();
        let name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let value = item
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        lines.push(if name.is_empty() {
            format!("[{}] {}", ref_id, role)
        } else {
            format!("[{}] {} \"{}\"", ref_id, role, truncate(&name, 80))
        });
        ref_map.insert(ref_id.to_string(), selector);
        elements.push(InteractiveElement {
            ref_id,
            role,
            name,
            value,
        });
    }
    let mut output = lines.join("\n");
    if output.len() > 6000 {
        let boundary = output.floor_char_boundary(6000);
        output = format!("{}\n... (truncated)", &output[..boundary]);
    }
    if output.is_empty() {
        output = "(no interactive elements found)".to_string();
    }

    let mut state = ctx.load_session_state()?;
    state.prev_elements = state.current_elements.clone();
    state.current_elements = Some(elements.clone());
    state.ref_map = Some(ref_map.clone());
    state.ref_map_url = Some(current_url.clone());
    state.ref_map_timestamp = Some(now_epoch_ms());
    ctx.save_session_state(&state)?;

    action_cache.insert(
        cache_key,
        ActionCacheEntry {
            ref_map: ref_map.clone(),
            elements: elements.clone(),
            output: output.clone(),
            timestamp: now_epoch_ms(),
        },
    );
    save_action_cache(ctx, &action_cache)?;

    Ok(InteractiveData { elements, output })
}

pub fn diff_elements(
    prev: &[InteractiveElement],
    curr: &[InteractiveElement],
) -> (
    Vec<InteractiveElement>,
    Vec<InteractiveElement>,
    Vec<(InteractiveElement, InteractiveElement)>,
) {
    let prev_map = prev
        .iter()
        .map(|e| (e.ref_id, e.clone()))
        .collect::<HashMap<_, _>>();
    let curr_map = curr
        .iter()
        .map(|e| (e.ref_id, e.clone()))
        .collect::<HashMap<_, _>>();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (ref_id, el) in &curr_map {
        if let Some(old) = prev_map.get(ref_id) {
            if old.role != el.role || old.name != el.name || old.value != el.value {
                changed.push((old.clone(), el.clone()));
            }
        } else {
            added.push(el.clone());
        }
    }
    for (ref_id, el) in &prev_map {
        if !curr_map.contains_key(ref_id) {
            removed.push(el.clone());
        }
    }
    (added, removed, changed)
}

pub fn cache_key_from_url(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        format!("{}{}", parsed.host_str().unwrap_or_default(), parsed.path())
    } else {
        url.to_string()
    }
}

pub fn load_action_cache(ctx: &AppContext) -> Result<HashMap<String, ActionCacheEntry>> {
    let path = ctx.action_cache_file();
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed reading {}", path.display()))?;
    serde_json::from_str(&content).with_context(|| format!("failed parsing {}", path.display()))
}

pub fn save_action_cache(
    ctx: &AppContext,
    cache: &HashMap<String, ActionCacheEntry>,
) -> Result<()> {
    let now = now_epoch_ms();
    let mut entries = cache
        .iter()
        .filter(|(_, v)| now - v.timestamp <= CACHE_TTL_MS)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| b.1.timestamp.cmp(&a.1.timestamp));
    entries.truncate(CACHE_MAX_ENTRIES);
    let pruned = entries.into_iter().collect::<HashMap<_, _>>();
    let path = ctx.action_cache_file();
    atomic_write_json(&path, &pruned)
}

/// Read-modify-write tab locks under an exclusive file lock.
/// Uses a separate `.lock` file to avoid flock+rename inode mismatch.
pub(crate) fn with_tab_locks_exclusive<F, R>(ctx: &AppContext, f: F) -> Result<R>
where
    F: FnOnce(&mut HashMap<String, TabLock>) -> Result<R>,
{
    let path = ctx.tab_locks_file();
    let lock_path = path.with_extension("lock");
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed opening lock file {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed locking {}", lock_path.display()))?;
    let mut locks: HashMap<String, TabLock> = if path.exists() {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed reading {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("failed parsing {}", path.display()))?
    } else {
        HashMap::new()
    };
    let result = f(&mut locks)?;
    atomic_write_json(&path, &locks)?;
    Ok(result)
}

pub fn check_tab_lock(ctx: &AppContext, tab_id: &str) -> Result<Option<TabLock>> {
    let tab_id = tab_id.to_string();
    let now = now_epoch_ms();
    with_tab_locks_exclusive(ctx, |locks| {
        if let Some(lock) = locks.get(&tab_id).cloned() {
            if now.saturating_sub(lock.expires) > 0 {
                locks.remove(&tab_id);
                return Ok(None);
            }
            return Ok(Some(lock));
        }
        Ok(None)
    })
}
