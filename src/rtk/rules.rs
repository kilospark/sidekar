use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    Supported {
        equivalent: &'static str,
        category: &'static str,
        estimated_savings_pct: u8,
    },
    Unsupported {
        base_command: String,
    },
    Ignored,
}

pub(super) struct RewriteRule {
    pub pattern: &'static str,
    pub equivalent: &'static str,
    pub category: &'static str,
    pub estimated_savings_pct: u8,
}

pub(super) struct OutputFilterDef {
    pub pattern: &'static str,
    pub strip_ansi: bool,
    pub strip_lines_matching: &'static [&'static str],
    pub keep_lines_matching: &'static [&'static str],
    pub max_lines: Option<usize>,
    pub on_empty: Option<&'static str>,
    pub dedupe_repeats: bool,
}

pub(super) struct OutputFilter {
    pub pattern: Regex,
    pub strip_ansi: bool,
    pub strip_lines_matching: Vec<Regex>,
    pub keep_lines_matching: Vec<Regex>,
    pub max_lines: Option<usize>,
    pub on_empty: Option<&'static str>,
    pub dedupe_repeats: bool,
}

pub(super) const REWRITE_RULES: &[RewriteRule] = &[
    RewriteRule {
        pattern: r"^git\s+(?:-[Cc]\s+\S+\s+)*(status|log|diff|show|add|commit|push|pull)",
        equivalent: "compact git",
        category: "Git",
        estimated_savings_pct: 75,
    },
    RewriteRule {
        pattern: r"^cargo\s+(build|check|clippy|test)",
        equivalent: "compact cargo",
        category: "Cargo",
        estimated_savings_pct: 85,
    },
    RewriteRule {
        pattern: r"^(python\s+-m\s+)?pytest(\s|$)",
        equivalent: "compact pytest",
        category: "Tests",
        estimated_savings_pct: 90,
    },
    RewriteRule {
        pattern: r"^(pnpm\s+|npm\s+(run\s+)?)test(\s|$)|^(npx\s+|pnpm\s+)?(vitest|jest)(\s|$)",
        equivalent: "compact test",
        category: "Tests",
        estimated_savings_pct: 90,
    },
    RewriteRule {
        pattern: r"^(cat|head|tail)\s+",
        equivalent: "compact read",
        category: "Files",
        estimated_savings_pct: 60,
    },
    RewriteRule {
        pattern: r"^(rg|grep)\s+",
        equivalent: "compact grep",
        category: "Files",
        estimated_savings_pct: 75,
    },
    RewriteRule {
        pattern: r"^ls(\s|$)|^find\s+",
        equivalent: "compact files",
        category: "Files",
        estimated_savings_pct: 65,
    },
    RewriteRule {
        pattern: r"^docker\s+(ps|logs|compose\s+logs)",
        equivalent: "compact docker",
        category: "Infra",
        estimated_savings_pct: 80,
    },
    RewriteRule {
        pattern: r"^kubectl\s+(get|logs|describe)",
        equivalent: "compact kubectl",
        category: "Infra",
        estimated_savings_pct: 80,
    },
    RewriteRule {
        pattern: r"^curl\s+",
        equivalent: "compact curl",
        category: "Network",
        estimated_savings_pct: 60,
    },
];

pub(super) static REWRITE_REGEXES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    REWRITE_RULES
        .iter()
        .map(|rule| Regex::new(rule.pattern).expect("invalid compact rule regex"))
        .collect()
});

pub(super) const FILTER_DEFS: &[OutputFilterDef] = &[
    OutputFilterDef {
        pattern: r"^git\s+(?:-[Cc]\s+\S+\s+)*status\b",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^On branch ",
            r"^Your branch is ",
            r"^nothing to commit, working tree clean$",
            r"^Changes to be committed:$",
            r"^Changes not staged for commit:$",
            r"^Untracked files:$",
            r"^\s+\(use ",
            r"^no changes added to commit",
        ],
        keep_lines_matching: &[
            r"^\s*(modified:|deleted:|new file:|renamed:|both modified:|both added:|\?\?)",
            r"^\s+\S.*$",
        ],
        max_lines: Some(40),
        on_empty: Some("git status: clean"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^cargo\s+(build|check|clippy|test)\b",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^Compiling ",
            r"^Checking ",
            r"^Finished ",
            r"^Running ",
            r"^Blocking waiting for file lock",
        ],
        keep_lines_matching: &[
            r"^error(\[.+\])?:",
            r"^warning:",
            r"^test result:",
            r"^failures:",
            r"^---- ",
            r"^FAILED$",
            r"^error: test failed",
        ],
        max_lines: Some(60),
        on_empty: Some("cargo: ok"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^(python\s+-m\s+)?pytest(\s|$)",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^=+ test session starts =+$",
            r"^platform ",
            r"^rootdir:",
            r"^plugins:",
            r"^collected \d+ items?$",
            r"^\s*$",
        ],
        keep_lines_matching: &[
            r"^=+ FAILURES =+$",
            r"^=+ ERRORS =+$",
            r"^FAILED ",
            r"^ERROR ",
            r"^short test summary info",
            r"^=+ .* in [0-9.]+s =+$",
        ],
        max_lines: Some(60),
        on_empty: Some("pytest: ok"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^(pnpm\s+|npm\s+(run\s+)?)test(\s|$)|^(npx\s+|pnpm\s+)?(vitest|jest)(\s|$)",
        strip_ansi: true,
        strip_lines_matching: &[
            r"^> ",
            r"^ RUN ",
            r"^ PASS ",
            r"^ ✓ ",
            r"^ Test Files ",
            r"^ Duration ",
        ],
        keep_lines_matching: &[
            r"^ FAIL ",
            r"^ ❯ ",
            r"^× ",
            r"^stderr ",
            r"^stdout ",
            r"^Tests?\s+",
            r"^Snapshots?\s+",
            r"^Time:\s+",
        ],
        max_lines: Some(60),
        on_empty: Some("tests: ok"),
        dedupe_repeats: false,
    },
    OutputFilterDef {
        pattern: r"^docker\s+(logs|compose\s+logs)\b|^kubectl\s+logs\b",
        strip_ansi: true,
        strip_lines_matching: &[],
        keep_lines_matching: &[],
        max_lines: Some(80),
        on_empty: None,
        dedupe_repeats: true,
    },
];

pub(super) static FILTERS: LazyLock<Vec<OutputFilter>> = LazyLock::new(|| {
    FILTER_DEFS
        .iter()
        .map(|def| OutputFilter {
            pattern: Regex::new(def.pattern).expect("invalid output filter regex"),
            strip_ansi: def.strip_ansi,
            strip_lines_matching: def
                .strip_lines_matching
                .iter()
                .map(|pattern| Regex::new(pattern).expect("invalid strip regex"))
                .collect(),
            keep_lines_matching: def
                .keep_lines_matching
                .iter()
                .map(|pattern| Regex::new(pattern).expect("invalid keep regex"))
                .collect(),
            max_lines: def.max_lines,
            on_empty: def.on_empty,
            dedupe_repeats: def.dedupe_repeats,
        })
        .collect()
});
