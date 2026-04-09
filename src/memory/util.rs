use super::*;

pub(super) fn extract_optional_value(args: &[String], prefix: &str) -> Option<String> {
    args.iter()
        .find_map(|arg| arg.strip_prefix(prefix).map(ToOwned::to_owned))
}

pub(super) fn normalize_event_type(value: &str) -> Result<String> {
    let normalized = value.trim().replace('_', "-");
    if MEMORY_TYPES.iter().any(|item| *item == normalized) {
        Ok(normalized)
    } else {
        bail!(
            "Invalid memory type: {}. Valid: {}",
            value,
            MEMORY_TYPES.join(", ")
        )
    }
}

pub(super) fn parse_csv_list(value: Option<String>) -> Vec<String> {
    value
        .map(|raw| {
            raw.split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn merge_tags(user_tags: &[String], auto_tags: &[String]) -> Vec<String> {
    let mut merged = Vec::new();
    for tag in user_tags.iter().chain(auto_tags.iter()) {
        if merged.len() >= 5 {
            break;
        }
        if !merged.iter().any(|existing| existing == tag) {
            merged.push(tag.clone());
        }
    }
    merged
}

pub(super) fn auto_tag(summary: &str) -> Vec<String> {
    let lower = summary.to_lowercase();
    let mut tags = Vec::new();
    for &(tag, keywords) in TAG_RULES {
        if tags.len() >= 5 {
            break;
        }
        if keywords.iter().any(|kw| lower.contains(kw)) {
            tags.push(tag.to_string());
        }
    }
    tags
}

pub(super) fn sanitize_fts_query(query: &str) -> String {
    normalize_summary(query)
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn normalize_summary(summary: &str) -> String {
    summary
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn summary_hash(summary: &str) -> String {
    let normalized = normalize_summary(summary);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in normalized.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(super) fn word_overlap_ratio(a: &str, b: &str) -> f64 {
    let a_words = significant_words(a);
    let b_words = significant_words(b);
    let min_size = a_words.len().min(b_words.len());
    if min_size == 0 {
        return 0.0;
    }
    let shared = a_words
        .iter()
        .filter(|word| b_words.contains(*word))
        .count();
    shared as f64 / min_size as f64
}

pub(super) fn significant_words(text: &str) -> HashSet<String> {
    const STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "shall", "can",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "through",
        "during", "before", "after", "and", "but", "or", "nor", "not", "so", "yet", "both",
        "either", "neither", "each", "every", "all", "any", "few", "more", "most", "other", "some",
        "such", "no", "only", "own", "same", "than", "too", "very", "just", "because", "if",
        "when", "where", "how", "what", "which", "who", "whom", "this", "that", "these", "those",
        "it", "its", "use", "using", "used", "sidekar",
    ];
    let stop_words: HashSet<&str> = STOP_WORDS.iter().copied().collect();
    text.to_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|word| word.len() > 2 && !stop_words.contains(*word))
        .map(ToOwned::to_owned)
        .collect()
}

pub(super) fn score_search_result(row: &MemoryEventRow, bm25_rank: f64) -> f64 {
    let fts_score = 1.0 / (1.0 + bm25_rank.abs());
    fts_score + recency_score(row)
}

pub(super) fn recency_score(row: &MemoryEventRow) -> f64 {
    let age_days = (now_epoch_ms() - row.created_at).max(0) as f64 / 86_400_000.0;
    let recency_boost = if age_days < 7.0 {
        1.2
    } else if age_days < 30.0 {
        1.1
    } else {
        1.0
    };
    let stale_multiplier = match row.event_type.as_str() {
        "open-thread" if age_days > 14.0 => 0.6,
        "artifact-pointer" if age_days > 30.0 => 0.7,
        "decision" | "preference" if age_days > 180.0 => 0.9,
        _ => 1.0,
    };
    (0.5 * recency_boost * stale_multiplier)
        + (row.confidence * 0.3)
        + ((row.reinforcement_count as f64).min(5.0) * 0.03)
        + type_priority(&row.event_type)
}

pub(super) fn type_priority(event_type: &str) -> f64 {
    match event_type {
        "constraint" => 0.15,
        "decision" => 0.12,
        "convention" => 0.10,
        "preference" => 0.08,
        "open-thread" => 0.05,
        "artifact-pointer" => 0.03,
        _ => 0.0,
    }
}

pub(super) fn event_type_label(event_type: &str) -> &'static str {
    match event_type {
        "decision" => "Decisions",
        "convention" => "Conventions",
        "constraint" => "Constraints",
        "preference" => "Preferences",
        "open-thread" => "Open Threads",
        "artifact-pointer" => "Artifacts",
        _ => "Memories",
    }
}

pub(super) fn cluster_by_similarity(rows: &[MemoryEventRow], threshold: f64) -> Vec<Vec<MemoryEventRow>> {
    let mut clusters = Vec::new();
    let mut used = HashSet::new();
    for row in rows {
        if used.contains(&row.id) {
            continue;
        }
        let mut cluster = vec![row.clone()];
        used.insert(row.id);
        for other in rows {
            if used.contains(&other.id) {
                continue;
            }
            if word_overlap_ratio(&row.summary, &other.summary) >= threshold {
                cluster.push(other.clone());
                used.insert(other.id);
            }
        }
        clusters.push(cluster);
    }
    clusters
}

pub(super) fn dedupe_rows_by_norm(rows: Vec<MemoryEventRow>) -> Vec<MemoryEventRow> {
    let mut best: HashMap<(String, String), MemoryEventRow> = HashMap::new();
    for row in rows.into_iter().filter(|row| row.superseded_by.is_none()) {
        let key = (row.event_type.clone(), normalize_summary(&row.summary));
        match best.get(&key) {
            Some(existing) if existing.scope == "project" && row.scope == "global" => {}
            Some(existing) if row.scope == "project" && existing.scope == "global" => {
                best.insert(key, row);
            }
            Some(existing) if existing.updated_at >= row.updated_at => {}
            _ => {
                best.insert(key, row);
            }
        }
    }
    best.into_values().collect()
}

pub(super) fn candidate_scope_match(row: &MemoryEventRow, project: &str, scope: &str) -> bool {
    if scope == "global" {
        row.scope == "global"
    } else {
        row.project == project || row.scope == "global"
    }
}
