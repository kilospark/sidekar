//! Deterministic extractors for project manifests. No LLM. Each
//! function takes a file's bytes + path and returns zero or more
//! `Candidate`s ready to hand to `write_memory_event`.
//!
//! Ported from norsu's `intelligence.rs::scan_project` with a few
//! additions borrowed from nairo's TypeScript extractors (README
//! purpose extraction, scripts as conventions, ADR files).

use super::Candidate;
use serde_json::Value;
use std::path::Path;

#[allow(clippy::too_many_arguments)]
fn push(
    out: &mut Vec<Candidate>,
    project: &str,
    event_type: &str,
    scope: &str,
    confidence: f64,
    summary: String,
    source_file: &Path,
    source_kind: &str,
    tags: &[&str],
) {
    out.push(Candidate {
        event_type: event_type.to_string(),
        summary,
        scope: scope.to_string(),
        project: project.to_string(),
        confidence,
        tags: tags.iter().map(|s| s.to_string()).collect(),
        source_kind: source_kind.to_string(),
        source_file: source_file.to_path_buf(),
    })
}

/// Dispatch on file basename. Unknown files return `Ok(Vec::new())`
/// — the caller doesn't need to pre-filter.
pub(super) fn extract(
    path: &Path,
    content: &str,
    project: &str,
) -> Result<Vec<Candidate>, anyhow::Error> {
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut out = Vec::new();

    match base.as_str() {
        "package.json" => extract_package_json(path, content, project, &mut out)?,
        "cargo.toml" => extract_cargo_toml(path, content, project, &mut out),
        "pyproject.toml" => extract_pyproject_toml(path, content, project, &mut out),
        "go.mod" => extract_go_mod(path, content, project, &mut out),
        "requirements.txt" => extract_requirements_txt(path, content, project, &mut out),
        "tsconfig.json" => extract_tsconfig(path, content, project, &mut out),
        "readme.md" => extract_readme(path, content, project, &mut out),
        "contributing.md" => extract_contributing(path, content, project, &mut out),
        _ => {
            // ADR / workflows live in subdirs; dispatch on parent.
            if let Some(parent_name) = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
            {
                match parent_name {
                    "adr" | "decisions" => extract_adr(path, content, project, &mut out),
                    "workflows" => extract_workflow(path, content, project, &mut out),
                    _ => {}
                }
            }
        }
    }
    Ok(out)
}

fn extract_package_json(
    path: &Path,
    content: &str,
    project: &str,
    out: &mut Vec<Candidate>,
) -> Result<(), anyhow::Error> {
    let pkg: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let deps = pkg
        .get("dependencies")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let dev_deps = pkg
        .get("devDependencies")
        .and_then(Value::as_object)
        .map(|o| o.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let all_deps: Vec<String> = deps.iter().chain(dev_deps.iter()).cloned().collect();

    let frameworks = detect_frameworks(&all_deps);
    let mut stack_parts = Vec::new();
    if !frameworks.is_empty() {
        stack_parts.push(format!("Framework: {}", frameworks.join(", ")));
    }
    let key_deps = pick_key_deps(&all_deps);
    if !key_deps.is_empty() {
        stack_parts.push(format!("Key deps: {}", key_deps.join(", ")));
    }
    if !stack_parts.is_empty() {
        push(
            out,
            project,
            "convention",
            crate::scope::PROJECT_SCOPE,
            0.9,
            format!("Tech stack from package.json: {}", stack_parts.join(". ")),
            path,
            "import:manifest:package.json",
            &["imported", "manifest", "package.json"],
        );
    }

    let scripts = pkg
        .get("scripts")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .take(10)
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !scripts.is_empty() {
        push(
            out,
            project,
            "convention",
            crate::scope::PROJECT_SCOPE,
            0.8,
            format!("Available npm scripts: {}", scripts.join("; ")),
            path,
            "import:manifest:package.json",
            &["imported", "manifest", "package.json", "scripts"],
        );
    }
    Ok(())
}

fn detect_frameworks(deps: &[String]) -> Vec<String> {
    let map: &[(&str, &str)] = &[
        ("next", "Next.js"),
        ("react", "React"),
        ("vue", "Vue.js"),
        ("nuxt", "Nuxt"),
        ("svelte", "Svelte"),
        ("@angular/core", "Angular"),
        ("express", "Express"),
        ("fastify", "Fastify"),
        ("@nestjs/core", "NestJS"),
        ("hono", "Hono"),
    ];
    let mut out = Vec::new();
    for (needle, label) in map {
        if deps.iter().any(|d| d == needle) {
            out.push((*label).to_string());
        }
    }
    out
}

fn pick_key_deps(deps: &[String]) -> Vec<String> {
    let interesting = [
        "typescript",
        "jest",
        "vitest",
        "mocha",
        "playwright",
        "cypress",
        "eslint",
        "prettier",
        "drizzle-orm",
        "prisma",
    ];
    interesting
        .iter()
        .filter(|needle| deps.iter().any(|d| d == **needle))
        .map(|s| (*s).to_string())
        .collect()
}

fn extract_cargo_toml(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    let mut section = String::new();
    let mut deps: Vec<String> = Vec::new();
    for raw in content.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(|c| c == '[' || c == ']').to_string();
            continue;
        }
        if matches!(section.as_str(), "dependencies" | "dev-dependencies")
            && let Some((k, _)) = line.split_once('=')
        {
            let name = k.trim().trim_matches('"').to_string();
            if !name.is_empty() {
                deps.push(name);
            }
        }
    }
    deps.sort();
    deps.dedup();
    let summary = if deps.is_empty() {
        "Rust project via Cargo.toml".to_string()
    } else {
        let preview: Vec<_> = deps.iter().take(8).cloned().collect();
        let extra = deps.len().saturating_sub(preview.len());
        let suffix = if extra > 0 {
            format!(", +{extra} more")
        } else {
            String::new()
        };
        format!(
            "Rust project via Cargo.toml. Crates: {}{}",
            preview.join(", "),
            suffix
        )
    };
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.9,
        summary,
        path,
        "import:manifest:cargo.toml",
        &["imported", "manifest", "cargo", "rust"],
    );
}

fn extract_pyproject_toml(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    // Extract just the [project] name + dependencies block. Good-enough
    // parser — we don't need a full TOML tree for this.
    let mut deps = Vec::new();
    let mut in_deps = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with("dependencies") && line.contains('[') {
            in_deps = true;
            for token in line.split('"').skip(1).step_by(2) {
                deps.push(token.to_string());
            }
            if line.contains(']') {
                in_deps = false;
            }
            continue;
        }
        if in_deps {
            if line.contains(']') {
                in_deps = false;
            }
            for token in line.split('"').skip(1).step_by(2) {
                deps.push(token.to_string());
            }
        }
    }
    deps.sort();
    deps.dedup();
    let summary = if deps.is_empty() {
        "Python project via pyproject.toml".to_string()
    } else {
        let preview: Vec<_> = deps.iter().take(8).cloned().collect();
        let extra = deps.len().saturating_sub(preview.len());
        let suffix = if extra > 0 {
            format!(", +{extra} more")
        } else {
            String::new()
        };
        format!(
            "Python project via pyproject.toml. Packages: {}{}",
            preview.join(", "),
            suffix
        )
    };
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.9,
        summary,
        path,
        "import:manifest:pyproject.toml",
        &["imported", "manifest", "python", "pyproject"],
    );
}

fn extract_go_mod(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    let mut deps = Vec::new();
    let mut in_require = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with("//") || line.is_empty() {
            continue;
        }
        if line == "require (" {
            in_require = true;
            continue;
        }
        if in_require && line == ")" {
            in_require = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix("require ") {
            if let Some(name) = rest.split_whitespace().next() {
                deps.push(name.to_string());
            }
        } else if in_require && let Some(name) = line.split_whitespace().next() {
            deps.push(name.to_string());
        }
    }
    deps.sort();
    deps.dedup();
    let summary = if deps.is_empty() {
        "Go module via go.mod".to_string()
    } else {
        let preview: Vec<_> = deps.iter().take(8).cloned().collect();
        let extra = deps.len().saturating_sub(preview.len());
        let suffix = if extra > 0 {
            format!(", +{extra} more")
        } else {
            String::new()
        };
        format!(
            "Go module via go.mod. Dependencies: {}{}",
            preview.join(", "),
            suffix
        )
    };
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.9,
        summary,
        path,
        "import:manifest:go.mod",
        &["imported", "manifest", "go"],
    );
}

fn extract_requirements_txt(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    let mut deps: Vec<String> = content
        .lines()
        .filter_map(|line| {
            let clean = line.split('#').next()?.trim();
            if clean.is_empty() {
                return None;
            }
            Some(
                clean
                    .split(['=', '<', '>', '!', '~'])
                    .next()
                    .unwrap_or(clean)
                    .trim()
                    .to_string(),
            )
        })
        .collect();
    deps.sort();
    deps.dedup();
    if deps.is_empty() {
        return;
    }
    let preview: Vec<_> = deps.iter().take(8).cloned().collect();
    let extra = deps.len().saturating_sub(preview.len());
    let suffix = if extra > 0 {
        format!(", +{extra} more")
    } else {
        String::new()
    };
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.85,
        format!(
            "Python packages in requirements.txt: {}{}",
            preview.join(", "),
            suffix
        ),
        path,
        "import:manifest:requirements.txt",
        &["imported", "manifest", "python", "requirements"],
    );
}

fn extract_tsconfig(path: &Path, _content: &str, project: &str, out: &mut Vec<Candidate>) {
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.8,
        "TypeScript enabled (tsconfig.json present)".to_string(),
        path,
        "import:manifest:tsconfig.json",
        &["imported", "manifest", "typescript"],
    );
}

fn extract_readme(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    // Purpose: first 3 non-empty paragraphs stripped of heading
    // markers, capped at 500 chars.
    let paragraphs: Vec<&str> = content
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .take(3)
        .collect();
    if !paragraphs.is_empty() {
        let joined = paragraphs
            .iter()
            .map(|p| p.trim_start_matches('#').trim().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        let clipped: String = joined.chars().take(500).collect();
        push(
            out,
            project,
            "decision",
            crate::scope::PROJECT_SCOPE,
            0.7,
            format!("Project purpose (from README.md): {clipped}"),
            path,
            "import:manifest:readme.md",
            &["imported", "manifest", "readme", "purpose"],
        );
    }

    // Architecture / Tech Stack section.
    if let Some(section) = find_section(content, &["architecture", "tech stack", "stack"]) {
        let clipped: String = section.chars().take(500).collect();
        push(
            out,
            project,
            "decision",
            crate::scope::PROJECT_SCOPE,
            0.7,
            format!("Architecture overview (from README.md): {clipped}"),
            path,
            "import:manifest:readme.md",
            &["imported", "manifest", "readme", "architecture"],
        );
    }
}

fn extract_contributing(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    // Grab the first few paragraphs; enough to capture "PR style /
    // commit message style / test before merge" guidance most
    // CONTRIBUTING files open with.
    let clipped: String = content.chars().take(800).collect();
    if clipped.trim().is_empty() {
        return;
    }
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.75,
        format!("Contribution guidelines (from CONTRIBUTING.md): {clipped}"),
        path,
        "import:manifest:contributing.md",
        &["imported", "manifest", "contributing"],
    );
}

fn extract_adr(path: &Path, content: &str, project: &str, out: &mut Vec<Candidate>) {
    let title = content
        .lines()
        .find(|l| l.trim_start().starts_with('#'))
        .map(|l| l.trim_start_matches('#').trim().to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("ADR")
                .to_string()
        });
    let clipped: String = content.chars().take(600).collect();
    push(
        out,
        project,
        "decision",
        crate::scope::PROJECT_SCOPE,
        0.8,
        format!("ADR '{title}': {clipped}"),
        path,
        "import:manifest:adr",
        &["imported", "manifest", "adr", "decision"],
    );
}

fn extract_workflow(path: &Path, _content: &str, project: &str, out: &mut Vec<Candidate>) {
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("workflow");
    push(
        out,
        project,
        "convention",
        crate::scope::PROJECT_SCOPE,
        0.6,
        format!("GitHub Actions workflow present: {name}"),
        path,
        "import:manifest:workflow",
        &["imported", "manifest", "ci", "github-actions"],
    );
}

fn find_section(content: &str, headings: &[&str]) -> Option<String> {
    let mut collecting = false;
    let mut collected = String::new();
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            if collecting {
                break;
            }
            let heading = trimmed.trim_start_matches('#').trim().to_ascii_lowercase();
            if headings.iter().any(|h| heading.starts_with(h)) {
                collecting = true;
                continue;
            }
        } else if collecting {
            collected.push_str(line);
            collected.push('\n');
        }
    }
    let trimmed = collected.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn p(name: &str) -> PathBuf {
        PathBuf::from("/tmp").join(name)
    }

    #[test]
    fn package_json_extracts_frameworks_and_scripts() {
        let content = r#"{
            "name":"demo",
            "dependencies":{"next":"14","react":"18"},
            "devDependencies":{"typescript":"5","vitest":"1"},
            "scripts":{"build":"next build","test":"vitest"}
        }"#;
        let got = extract(&p("package.json"), content, "demo").unwrap();
        let stack = got
            .iter()
            .find(|c| c.summary.starts_with("Tech stack"))
            .expect("stack summary");
        assert!(stack.summary.contains("Next.js"));
        assert!(stack.summary.contains("React"));
        assert!(stack.summary.contains("typescript"));
        assert!(stack.summary.contains("vitest"));
        assert!(
            got.iter()
                .any(|c| c.summary.starts_with("Available npm scripts"))
        );
        for c in &got {
            assert_eq!(c.scope, "project");
            assert!(c.tags.contains(&"imported".to_string()));
        }
    }

    #[test]
    fn cargo_toml_extracts_deps_preview() {
        let content = r#"
            [package]
            name = "demo"

            [dependencies]
            serde = "1"
            tokio = { version = "1" }
            anyhow = "1"
        "#;
        let got = extract(&p("Cargo.toml"), content, "demo").unwrap();
        assert_eq!(got.len(), 1);
        let summary = &got[0].summary;
        assert!(summary.contains("Crates:"));
        assert!(summary.contains("serde"));
        assert!(summary.contains("tokio"));
    }

    #[test]
    fn readme_extracts_purpose_and_arch() {
        let content = "\
# Demo\n\n\
Demo is a tool that does a thing and another thing to be useful.\n\n\
## Architecture\n\n\
It uses a SQLite backend with an HTTP facade.\n";
        let got = extract(&p("README.md"), content, "demo").unwrap();
        assert!(got.iter().any(|c| c.summary.contains("Project purpose")));
        assert!(
            got.iter()
                .any(|c| c.summary.contains("Architecture overview"))
        );
    }

    #[test]
    fn adr_uses_first_heading_as_title() {
        let content = "# 0001 Use SQLite\n\nStatus: accepted.\n";
        let got = extract(
            &PathBuf::from("/tmp/docs/adr/0001-use-sqlite.md"),
            content,
            "demo",
        )
        .unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].summary.starts_with("ADR '0001 Use SQLite':"));
        assert_eq!(got[0].event_type, "decision");
    }

    #[test]
    fn go_mod_truncates_dep_preview() {
        let mut content = String::from("module example.com/demo\n\nrequire (\n");
        for i in 0..15 {
            content.push_str(&format!("  github.com/pkg/pkg{i} v1.0.0\n"));
        }
        content.push_str(")\n");
        let got = extract(&p("go.mod"), &content, "demo").unwrap();
        assert_eq!(got.len(), 1);
        assert!(got[0].summary.contains("more"));
    }

    #[test]
    fn unknown_file_returns_empty() {
        let got = extract(&p("LICENSE"), "MIT", "demo").unwrap();
        assert!(got.is_empty());
    }
}
