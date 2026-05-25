//! Model capability registry: catalog metadata, models.dev enrichment, learned rejections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ContentBlock, RemoteModel, MODEL_CATALOG_TIMEOUT_SECS, catalog_http_client};

const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const PERSIST_FILE: &str = "model-capabilities.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisionSupport {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub vision: VisionSupport,
}

impl ModelCapabilities {
    #[must_use]
    pub const fn vision_supported() -> Self {
        Self {
            vision: VisionSupport::Supported,
        }
    }

    #[must_use]
    pub const fn vision_unsupported() -> Self {
        Self {
            vision: VisionSupport::Unsupported,
        }
    }

    /// Whether outbound requests may include image blocks for this model.
    #[must_use]
    pub fn allows_vision(self, provider_type: &str) -> bool {
        match self.vision {
            VisionSupport::Supported => true,
            VisionSupport::Unsupported => false,
            VisionSupport::Unknown => default_allows_unknown_vision(provider_type),
        }
    }
}

fn default_allows_unknown_vision(provider_type: &str) -> bool {
    // Mixed catalogs: assume text-only until catalog or a 400 teaches otherwise.
    !matches!(provider_type, "opencode-go")
}

#[must_use]
pub fn capability_key(provider_type: &str, model_id: &str) -> String {
    format!("{}:{}", provider_type.trim(), model_id.trim())
}

#[must_use]
pub fn vision_from_input_modalities(modalities: &[String]) -> VisionSupport {
    if modalities
        .iter()
        .any(|m| m.eq_ignore_ascii_case("image") || m.eq_ignore_ascii_case("IMAGE"))
    {
        VisionSupport::Supported
    } else if modalities
        .iter()
        .any(|m| m.eq_ignore_ascii_case("text") || m.eq_ignore_ascii_case("TEXT"))
    {
        VisionSupport::Unsupported
    } else {
        VisionSupport::Unknown
    }
}

#[must_use]
pub fn vision_from_json_modalities(value: &Value) -> VisionSupport {
    let Some(arr) = value.as_array() else {
        return VisionSupport::Unknown;
    };
    let mods: Vec<String> = arr
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    if mods.is_empty() {
        VisionSupport::Unknown
    } else {
        vision_from_input_modalities(&mods)
    }
}

#[must_use]
pub fn vision_from_openrouter_architecture(model: &Value) -> VisionSupport {
    model
        .get("architecture")
        .and_then(|a| a.get("input_modalities"))
        .map(vision_from_json_modalities)
        .unwrap_or(VisionSupport::Unknown)
}

#[must_use]
pub fn is_vision_rejection_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    (m.contains("image_url")
        && (m.contains("expected 'text'") || m.contains("unknown variant")))
        || m.contains("does not support image")
        || m.contains("does not support vision")
        || (m.contains("multimodal") && m.contains("not support"))
}

#[must_use]
pub fn user_content_has_images(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|b| match b {
        ContentBlock::Image { .. } => true,
        ContentBlock::ToolResult { content_images, .. } => !content_images.is_empty(),
        _ => false,
    })
}

/// Pre-send warning when images are attached but resolver says model is text-only.
#[must_use]
pub fn preflight_image_warning(
    provider_type: &str,
    model_id: &str,
    blocks: &[ContentBlock],
) -> Option<String> {
    if !user_content_has_images(blocks) {
        return None;
    }
    if allows_vision(provider_type, model_id) {
        return None;
    }
    Some(format!(
        "Warning: model `{model_id}` ({provider_type}) does not accept images; they will be omitted from the API request."
    ))
}

#[must_use]
pub fn vision_list_tag(caps: &ModelCapabilities) -> &'static str {
    match caps.vision {
        VisionSupport::Supported => ", vision",
        VisionSupport::Unsupported => ", text-only",
        VisionSupport::Unknown => "",
    }
}

// ---------------------------------------------------------------------------
// Caches
// ---------------------------------------------------------------------------

static MODELS_DEV_VISION: LazyLock<Mutex<Option<HashMap<String, VisionSupport>>>> =
    LazyLock::new(|| Mutex::new(None));

static CATALOG_VISION: LazyLock<Mutex<HashMap<String, VisionSupport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static LEARNED_VISION: LazyLock<Mutex<HashMap<String, VisionSupport>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static PERSIST_LOADED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

#[derive(Serialize, Deserialize)]
struct PersistedStore {
    #[serde(default)]
    vision: HashMap<String, VisionSupport>,
}

fn persist_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".sidekar")
        .join(PERSIST_FILE)
}

fn ensure_persist_loaded() {
    let mut loaded = PERSIST_LOADED.lock().unwrap_or_else(|e| e.into_inner());
    if *loaded {
        return;
    }
    *loaded = true;
    let path = persist_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let Ok(store) = serde_json::from_str::<PersistedStore>(&text) else {
        return;
    };
    let mut learned = LEARNED_VISION.lock().unwrap_or_else(|e| e.into_inner());
    for (k, v) in store.vision {
        learned.insert(k, v);
    }
}

fn persist_learned() {
    let learned = LEARNED_VISION.lock().unwrap_or_else(|e| e.into_inner());
    let store = PersistedStore {
        vision: learned.clone(),
    };
    drop(learned);
    let path = persist_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&store) {
        let _ = std::fs::write(path, text);
    }
}

fn lookup_vision(provider_type: &str, model_id: &str) -> Option<VisionSupport> {
    ensure_persist_loaded();
    let key = capability_key(provider_type, model_id);
    if let Ok(learned) = LEARNED_VISION.lock() {
        if let Some(v) = learned.get(&key) {
            return Some(*v);
        }
    }
    if let Ok(catalog) = CATALOG_VISION.lock() {
        if let Some(v) = catalog.get(&key) {
            return Some(*v);
        }
    }
    if let Ok(dev) = MODELS_DEV_VISION.lock() {
        if let Some(map) = dev.as_ref() {
            if let Some(v) = map.get(&key) {
                return Some(*v);
            }
        }
    }
    None
}

/// Resolve whether image blocks may be sent for `(provider_type, model_id)`.
#[must_use]
pub fn allows_vision(provider_type: &str, model_id: &str) -> bool {
    let caps = lookup_vision(provider_type, model_id)
        .map(|vision| ModelCapabilities { vision })
        .unwrap_or_default();
    caps.allows_vision(provider_type)
}

/// Store catalog rows returned by `fetch_model_list*` (in-memory for this process).
pub fn ingest_model_catalog(provider_type: &str, models: &[RemoteModel]) {
    let mut catalog = CATALOG_VISION.lock().unwrap_or_else(|e| e.into_inner());
    for m in models {
        if m.capabilities.vision != VisionSupport::Unknown {
            catalog.insert(
                capability_key(provider_type, &m.id),
                m.capabilities.vision,
            );
        }
    }
}

/// Record a provider 400 indicating vision input is rejected; persisted across sessions.
pub fn record_vision_rejection(provider_type: &str, model_id: &str) {
    ensure_persist_loaded();
    let key = capability_key(provider_type, model_id);
    {
        let mut learned = LEARNED_VISION.lock().unwrap_or_else(|e| e.into_inner());
        learned.insert(key, VisionSupport::Unsupported);
    }
    {
        let mut catalog = CATALOG_VISION.lock().unwrap_or_else(|e| e.into_inner());
        catalog.insert(capability_key(provider_type, model_id), VisionSupport::Unsupported);
    }
    persist_learned();
}

async fn models_dev_vision_map() -> Result<HashMap<String, VisionSupport>, String> {
    {
        let guard = MODELS_DEV_VISION.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(map) = guard.as_ref() {
            return Ok(map.clone());
        }
    }

    let client = catalog_http_client(MODEL_CATALOG_TIMEOUT_SECS)?;
    let body = super::catalog_send_json_plain(client.get(MODELS_DEV_URL), "models.dev").await?;

    let mut out = HashMap::new();
    for (provider_slug, provider) in body.as_object().into_iter().flatten() {
        let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        for (model_id, entry) in models {
            let vision = vision_from_models_dev_entry(entry);
            if vision != VisionSupport::Unknown {
                out.insert(
                    format!("{provider_slug}:{model_id}"),
                    vision,
                );
            }
        }
    }

    let mut guard = MODELS_DEV_VISION.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some(out.clone());
    Ok(out)
}

fn vision_from_models_dev_entry(entry: &Value) -> VisionSupport {
    if let Some(inputs) = entry.pointer("/modalities/input") {
        return vision_from_json_modalities(inputs);
    }
    if let Some(image) = entry.pointer("/capabilities/input/image").and_then(|v| v.as_bool()) {
        return if image {
            VisionSupport::Supported
        } else {
            VisionSupport::Unsupported
        };
    }
    VisionSupport::Unknown
}

/// Merge models.dev vision metadata into OpenCode catalog rows.
pub async fn enrich_opencode_catalog(provider_slug: &str, models: &mut [RemoteModel]) {
    let dev = match models_dev_vision_map().await {
        Ok(m) => m,
        Err(e) => {
            crate::broker::try_log_event(
                "debug",
                "provider",
                "models-dev-capabilities-skip",
                Some(&e),
            );
            return;
        }
    };
    for m in models.iter_mut() {
        let key = format!("{provider_slug}:{}", m.id);
        if let Some(vision) = dev.get(&key) {
            m.capabilities.vision = *vision;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vision_rejection_detects_deepseek_shape() {
        let msg = r#"Error from provider (DeepSeek): Failed to deserialize the JSON body into the target type: messages[7]: unknown variant 'image_url', expected 'text'"#;
        assert!(is_vision_rejection_error(msg));
    }

    #[test]
    fn unknown_opencode_go_defaults_text_only() {
        assert!(!ModelCapabilities::default().allows_vision("opencode-go"));
        assert!(ModelCapabilities::default().allows_vision("openrouter"));
    }

    #[test]
    fn modalities_parse_image_and_text_only() {
        assert_eq!(
            vision_from_input_modalities(&["text".into(), "image".into()]),
            VisionSupport::Supported
        );
        assert_eq!(
            vision_from_input_modalities(&["text".into()]),
            VisionSupport::Unsupported
        );
    }

    #[test]
    fn preflight_warns_on_text_only_model_with_image() {
        let key = capability_key("opencode-go", "deepseek-v4-pro");
        {
            let mut catalog = CATALOG_VISION.lock().unwrap();
            catalog.insert(key, VisionSupport::Unsupported);
        }
        let blocks = vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data_base64: "x".into(),
            source_path: None,
        }];
        assert!(preflight_image_warning("opencode-go", "deepseek-v4-pro", &blocks).is_some());
    }
}
