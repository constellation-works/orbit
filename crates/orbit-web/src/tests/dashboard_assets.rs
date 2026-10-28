use std::path::{Path, PathBuf};

/// Dashboard asset root: `crates/orbit-web/assets/dashboard`.
fn dashboard_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/dashboard")
}

fn read_dashboard_file(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("dashboard test cannot read {}: {error}", path.display()))
}

/// Every route served by the real dashboard server.
fn dashboard_routes() -> std::collections::BTreeSet<String> {
    crate::assets::DASHBOARD_FILES
        .iter()
        .map(|&(route, _, _)| route.to_owned())
        .collect()
}

/// Every `.js` file under `assets/dashboard/`, as a `/`-separated path
/// relative to that directory (e.g. `app.js`, `js/tasks.js`,
/// `vendor/marked.umd.js`), sorted for deterministic failures.
fn all_dashboard_js_files() -> Vec<String> {
    fn visit(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("dashboard test cannot list {}: {error}", dir.display()))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| panic!("dashboard test cannot read entry: {error}"))
                    .path()
            })
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                visit(&path, root, out);
            } else if path.extension().is_some_and(|ext| ext == "js") {
                out.push(
                    path.strip_prefix(root)
                        .expect("dashboard file under dashboard root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }

    let root = dashboard_dir();
    let mut files = Vec::new();
    visit(&root, &root, &mut files);
    files
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

/// `(specifier, line)` for every relative module specifier in `source`:
/// each `from '…'` / `from "…"` (covers `import … from`, including
/// multi-line imports whose `from` sits on the closing-brace line, and
/// `export … from`) plus side-effect `import '…'` statements. Only
/// specifiers starting with `.` are reported, so prose like `"Sampled from"`
/// or relation keys like `"spawned_from"` never match.
fn relative_specifiers(source: &str) -> Vec<(String, usize)> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let is_from = starts_word_at(bytes, i, b"from");
        let is_import = !is_from && starts_word_at(bytes, i, b"import");
        if !is_from && !is_import {
            i += 1;
            continue;
        }
        let mut j = i + if is_from { 4 } else { 6 };
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < bytes.len() && (bytes[j] == b'\'' || bytes[j] == b'"') {
            let quote = bytes[j];
            let mut spec = String::new();
            let mut k = j + 1;
            let mut closed = false;
            while k < bytes.len() {
                let byte = bytes[k];
                if byte == b'\\' && k + 1 < bytes.len() {
                    spec.push(bytes[k + 1] as char);
                    k += 2;
                    continue;
                }
                if byte == quote {
                    closed = true;
                    break;
                }
                if byte == b'\n' {
                    break;
                }
                spec.push(byte as char);
                k += 1;
            }
            if closed && spec.starts_with('.') {
                let line = source.as_bytes()[..i]
                    .iter()
                    .filter(|&&byte| byte == b'\n')
                    .count()
                    + 1;
                found.push((spec, line));
            }
            i = if closed { k + 1 } else { j + 1 };
        } else {
            i += 1;
        }
    }
    found
}

/// Resolve `spec` (e.g. `./common.js`, `../js/x.js`) against the
/// dashboard-relative path of the importing file (e.g. `js/tasks.js`,
/// `app.js`) into the `/static/…` route the real server must serve.
fn resolve_route(importer: &str, spec: &str) -> String {
    let mut segments: Vec<&str> = match importer.rfind('/') {
        Some(index) => importer[..index].split('/').collect(),
        None => Vec::new(),
    };
    for part in spec.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            name => segments.push(name),
        }
    }
    format!("/static/{}", segments.join("/"))
}

#[test]
fn dashboard_imported_modules_are_registered_routes() {
    let routes = dashboard_routes();
    // Every relative import or re-export must resolve to a registered route.
    // The browser-test fixture server reads the assets directory directly, so
    // an unregistered module passes every other test and then 404s in the
    // real server.
    for (name, source) in first_party_dashboard_sources() {
        for (spec, line) in relative_specifiers(&source) {
            let route = resolve_route(&name, &spec);
            assert!(
                routes.contains(&route),
                "{name}:{line} imports '{spec}', which resolves to '{route}' with no \
                 DASHBOARD_FILES entry; register it or the real dashboard server 404s it"
            );
        }
    }
    // The reverse direction: every `.js` file under `assets/dashboard/` must
    // be registered, catching an orphan no module imports yet.
    for file in all_dashboard_js_files() {
        let route = format!("/static/{file}");
        assert!(
            routes.contains(&route),
            "dashboard file '{file}' has no DASHBOARD_FILES route; register it or the \
             real dashboard server 404s it"
        );
    }
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
    for wanted in ["app.js", "js/tasks.js", "js/plugins.js"] {
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
