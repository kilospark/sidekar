use super::*;

pub(crate) fn capture_symbol(
    lines: &[&str],
    line_idx: usize,
    patterns: &[(&Regex, SymbolKind)],
    file_path: &str,
    comment_prefix: &str,
) -> Option<Symbol> {
    let trimmed = lines[line_idx].trim_start();
    for (regex, kind) in patterns {
        let cap = regex.captures(trimmed)?;
        let end = match kind {
            SymbolKind::Import | SymbolKind::Variable => line_idx,
            _ => brace_block_end(lines, line_idx).unwrap_or(line_idx),
        };
        return Some(simple_symbol(
            &cap[1],
            kind.clone(),
            file_path,
            line_idx,
            end,
            Some(trimmed.to_string()),
            collect_comments(lines, line_idx, comment_prefix),
        ));
    }
    None
}

pub(crate) fn simple_symbol(
    name: &str,
    kind: SymbolKind,
    file_path: &str,
    start: usize,
    end: usize,
    signature: Option<String>,
    doc_comment: Option<String>,
) -> Symbol {
    Symbol {
        name: name.to_string(),
        kind,
        file: file_path.to_string(),
        line_start: start as u32,
        line_end: end as u32,
        signature,
        doc_comment,
        children: Vec::new(),
    }
}

pub(crate) trait WithChildren {
    fn with_children(self, children: Vec<Symbol>) -> Symbol;
}

impl WithChildren for Symbol {
    fn with_children(mut self, children: Vec<Symbol>) -> Symbol {
        self.children = children;
        self
    }
}

pub(crate) fn collect_comments(lines: &[&str], line_idx: usize, prefix: &str) -> Option<String> {
    match prefix {
        "#" => collect_hash_comments(lines, line_idx),
        _ => collect_line_comments(lines, line_idx, prefix),
    }
}

pub(crate) fn collect_line_comments(lines: &[&str], line_idx: usize, prefix: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut idx = line_idx;
    while idx > 0 {
        idx -= 1;
        let trimmed = lines[idx].trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with(prefix) {
            out.push(trimmed.to_string());
            continue;
        }
        if trimmed.starts_with("#[") || trimmed.starts_with("@") {
            continue;
        }
        break;
    }
    if out.is_empty() {
        None
    } else {
        out.reverse();
        Some(out.join("\n"))
    }
}

pub(crate) fn collect_hash_comments(lines: &[&str], line_idx: usize) -> Option<String> {
    let mut out = Vec::new();
    let mut idx = line_idx;
    while idx > 0 {
        idx -= 1;
        let trimmed = lines[idx].trim();
        if trimmed.is_empty() {
            break;
        }
        if trimmed.starts_with('#') {
            out.push(trimmed.to_string());
            continue;
        }
        if trimmed.starts_with('@') {
            continue;
        }
        break;
    }
    if out.is_empty() {
        None
    } else {
        out.reverse();
        Some(out.join("\n"))
    }
}

pub(crate) fn brace_block_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut saw_open = false;
    for (idx, line) in lines.iter().enumerate().skip(start) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' if saw_open => {
                    depth -= 1;
                    if depth <= 0 {
                        return Some(idx);
                    }
                }
                _ => {}
            }
        }
        if idx > start && saw_open && depth <= 0 {
            return Some(idx);
        }
    }
    if saw_open {
        Some(lines.len().saturating_sub(1))
    } else {
        None
    }
}

pub(crate) fn python_block_end(lines: &[&str], start: usize, indent: usize) -> Option<usize> {
    let mut body_indent = None;
    for (idx, line) in lines.iter().enumerate().skip(start + 1) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let current_indent = line.len().saturating_sub(line.trim_start().len());
        if body_indent.is_none() {
            body_indent = Some(current_indent);
        }
        if current_indent <= indent && !trimmed.starts_with('#') {
            return Some(idx.saturating_sub(1));
        }
    }
    Some(lines.len().saturating_sub(1))
}

pub(crate) fn ruby_block_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let opener =
        Regex::new(r"^\s*(class|module|def|if|unless|case|begin|for|while|until|do)\b").unwrap();
    let ender = Regex::new(r"^\s*end\b").unwrap();
    for (idx, line) in lines.iter().enumerate().skip(start) {
        let trimmed = line.trim_start();
        if opener.is_match(trimmed) {
            depth += 1;
        }
        if ender.is_match(trimmed) {
            depth -= 1;
            if depth <= 0 {
                return Some(idx);
            }
        }
    }
    Some(lines.len().saturating_sub(1))
}
