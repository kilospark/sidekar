//! Element reference system for desktop automation.
//!
//! Assigns stable `@e1`, `@e2`, ... refs to interactive AX elements during
//! tree snapshots. Subsequent commands (`click @e3`, `type @e5 "hello"`)
//! resolve refs without re-walking the tree.
//!
//! Inspired by agent-desktop's ref system (Apache-2.0, lahfir/agent-desktop).
//! Key differences: sidekar stores refs in-memory (process-global), not on
//! disk; refs are scoped per-pid and invalidated on re-snapshot.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use super::types::DesktopElementPath;

/// Roles considered "interactive" — elements with these roles get refs.
pub const INTERACTIVE_ROLES: &[&str] = &[
    "AXButton",
    "AXTextField",
    "AXTextArea",
    "AXCheckBox",
    "AXLink",
    "AXMenuItem",
    "AXTab",
    "AXSlider",
    "AXComboBox",
    "AXRadioButton",
    "AXIncrementor",
    "AXMenuButton",
    "AXSwitch",
    "AXColorWell",
    "AXPopUpButton",
    "AXDisclosureTriangle",
    "AXOutlineRow",
];

#[allow(dead_code)]
fn is_interactive_role(role: &str) -> bool {
    INTERACTIVE_ROLES.contains(&role)
}

/// A stored element reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefEntry {
    pub pid: i32,
    pub role: String,
    pub title: Option<String>,
    pub value: Option<String>,
    pub actions: Vec<String>,
    pub path: DesktopElementPath,
    /// Hash of the element's frame for staleness detection.
    pub frame_hash: Option<u64>,
}

/// The ref map — maps `@e1` → RefEntry.
/// Persisted to `~/.sidekar/desktop-refs.json` so refs survive across
/// CLI invocations.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RefMap {
    entries: HashMap<String, RefEntry>,
    counter: u32,
    /// Which pid this map was built for. Re-snapshot of the same pid
    /// clears old refs; different pids coexist.
    pid_scopes: HashMap<i32, Vec<String>>,
}

impl RefMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new ref for an element. Returns the ref id (e.g. `@e1`).
    pub fn allocate(&mut self, entry: RefEntry) -> String {
        self.counter += 1;
        let ref_id = format!("@e{}", self.counter);
        let pid = entry.pid;
        self.entries.insert(ref_id.clone(), entry);
        self.pid_scopes.entry(pid).or_default().push(ref_id.clone());
        ref_id
    }

    /// Look up a ref.
    pub fn get(&self, ref_id: &str) -> Option<&RefEntry> {
        self.entries.get(ref_id)
    }

    /// Clear all refs for a specific pid (called before re-snapshot).
    pub fn clear_pid(&mut self, pid: i32) {
        if let Some(ids) = self.pid_scopes.remove(&pid) {
            for id in ids {
                self.entries.remove(&id);
            }
        }
    }

    /// Total refs across all pids.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries, sorted by ref id for stable output.
    pub fn entries_sorted(&self) -> Vec<(&str, &RefEntry)> {
        let mut v: Vec<_> = self.entries.iter().map(|(k, v)| (k.as_str(), v)).collect();
        v.sort_by_key(|(k, _)| {
            // Sort by numeric suffix
            k.strip_prefix("@e")
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
        });
        v
    }
}

// ---------------------------------------------------------------------------
// Process-global singleton
// ---------------------------------------------------------------------------

static REF_MAP: OnceLock<Mutex<RefMap>> = OnceLock::new();

pub fn ref_map() -> &'static Mutex<RefMap> {
    REF_MAP.get_or_init(|| Mutex::new(RefMap::new()))
}

fn refs_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".sidekar")
        .join("desktop-refs.json")
}

/// Save the current ref map to disk.
pub fn save_refs() {
    let map = ref_map().lock().unwrap();
    if map.is_empty() {
        return;
    }
    let path = refs_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&*map) {
        let _ = std::fs::write(&path, json);
    }
}

/// Load refs from disk into the process-global map.
pub fn load_refs() {
    let path = refs_path();
    if let Ok(data) = std::fs::read_to_string(&path)
        && let Ok(loaded) = serde_json::from_str::<RefMap>(&data)
    {
        let mut map = ref_map().lock().unwrap();
        *map = loaded;
    }
}

/// Parse a ref id from a query string. Returns Some("@e3") if the query
/// starts with `@e` followed by digits.
pub fn parse_ref(query: &str) -> Option<&str> {
    let trimmed = query.trim();
    if trimmed.starts_with("@e")
        && trimmed[2..].chars().all(|c| c.is_ascii_digit())
        && trimmed.len() > 2
    {
        Some(trimmed)
    } else {
        None
    }
}

/// Hash a frame for staleness detection.
pub fn frame_hash(x: f64, y: f64, w: f64, h: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Quantize to avoid float comparison issues
    ((x * 10.0) as i64).hash(&mut hasher);
    ((y * 10.0) as i64).hash(&mut hasher);
    ((w * 10.0) as i64).hash(&mut hasher);
    ((h * 10.0) as i64).hash(&mut hasher);
    hasher.finish()
}
