use std::path::{Path, PathBuf};

/// Dashboard asset root: `crates/orbit-web/assets/dashboard`.
fn dashboard_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/dashboard")
}

fn read_dashboard_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("dashboard test cannot read {}: {error}", path.display()))
}

/// First-party dashboard modules (`app.js` plus `js/*.js`) as
/// `(dashboard-relative name, source)`. Vendor bundles are third-party
/// (`marked`, `purify`) and out of scope: the real server serves them as
/// opaque scripts, never as relative ES-module imports.
fn first_party_dashboard_sources() -> Vec<(String, String)> {
    let dashboard = dashboard_dir();
    let mut sources = vec![(
        "app.js".to_owned(),
        read_dashboard_file(&dashboard.join("app.js")),
    )];
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dashboard.join("js"))
        .expect("dashboard test cannot list js/")
        .map(|entry| entry.expect("dashboard test cannot read js/ entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "js"))
        .collect();
    entries.sort();
    for path in entries {
        let name = format!(
            "js/{}",
            path.file_name()
                .expect("js file has a name")
                .to_string_lossy()
        );
        sources.push((name, read_dashboard_file(&path)));
    }
    sources
}

fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$'
}

fn starts_word_at(bytes: &[u8], at: usize, word: &[u8]) -> bool {
    bytes.len() >= at + word.len()
        && bytes[at..at + word.len()] == *word
        && (at == 0 || !is_word_byte(bytes[at - 1]))
        && (at + word.len() == bytes.len() || !is_word_byte(bytes[at + word.len()]))
}

/// True when `source` invokes the raw `marked` parser instead of the
/// sanitizing wrapper: a direct `marked(…)` call or a method call
/// `marked.parse(…)` / `marked.parseInline(…)`. Word-boundary matching keeps
/// helpers like `markedRuntime(` and the wrapper's own indirect dispatch
/// `marked[methodName](…)` from matching.
fn calls_raw_marked(source: &str) -> bool {
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if starts_word_at(bytes, i, b"marked") {
            let mut j = i + b"marked".len();
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                return true;
            }
            if j < bytes.len() && bytes[j] == b'.' {
                let mut k = j + 1;
                while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                    k += 1;
                }
                let method_len = if k + 11 <= bytes.len() && &bytes[k..k + 11] == b"parseInline" {
                    11
                } else if k + 5 <= bytes.len() && &bytes[k..k + 5] == b"parse" {
                    5
                } else {
                    0
                };
                if method_len > 0
                    && (k + method_len == bytes.len() || !is_word_byte(bytes[k + method_len]))
                {
                    let mut m = k + method_len;
                    while m < bytes.len() && bytes[m].is_ascii_whitespace() {
                        m += 1;
                    }
                    if m < bytes.len() && bytes[m] == b'(' {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

#[test]
fn dashboard_markdown_call_sites_use_sanitizing_wrapper() {
    let sources = first_party_dashboard_sources();
    let by_name = |wanted: &str| {
        sources
            .iter()
            .find(|(name, _)| name == wanted)
            .unwrap_or_else(|| panic!("dashboard test cannot find {wanted}"))
            .1
            .as_str()
    };
    assert!(
        by_name("js/markdown.js").contains("purifier.sanitize("),
        "markdown wrapper must sanitize rendered HTML before DOM insertion"
    );
    for (name, source) in &sources {
        assert!(
            !calls_raw_marked(source),
            "{name} must not bypass the sanitizing markdown wrapper (raw marked output is unsanitized HTML: use renderMarkdown(...) instead of marked(...)/marked.parse(...))"
        );
    }
    for wanted in ["app.js", "js/task-actions.js", "js/plugins.js"] {
        assert!(
            by_name(wanted).contains("renderMarkdown("),
            "{wanted} must call the sanitizing markdown wrapper renderMarkdown("
        );
    }
    assert!(
        !by_name("js/plugins.js").contains("innerHTML = source"),
        "plugin source must not be assigned as raw innerHTML"
    );
}
