//! Language-agnostic path, `.gitattributes`, binary and generated-header
//! exclusions for `comment-hygiene` batch selection.

/// Prefix bytes inspected per blob for binary NUL detection.
const BINARY_SNIFF_BYTES: usize = 8192;
/// Leading lines scanned for generated-file header markers.
const GENERATED_HEADER_LINES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PathExclusion {
    Vendored,
    FixtureOrTestData,
    Generated,
    ArchiveOrBinary,
}

impl PathExclusion {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Vendored => "vendored",
            Self::FixtureOrTestData => "fixture_or_test_data",
            Self::Generated => "generated",
            Self::ArchiveOrBinary => "archive_or_binary",
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct GitAttributeRules {
    rules: Vec<GitAttributeRule>,
}

#[derive(Debug)]
struct GitAttributeRule {
    pattern: String,
    binary: Option<bool>,
    generated: Option<bool>,
    vendored: Option<bool>,
}

impl GitAttributeRules {
    pub(super) fn parse(raw: &str) -> Self {
        let mut rules = Vec::new();
        for line in raw.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let mut parts = trimmed.split_whitespace();
            let Some(pattern) = parts.next() else {
                continue;
            };
            let mut binary = None;
            let mut generated = None;
            let mut vendored = None;
            for attr in parts {
                match attr {
                    "binary" | "-text" => binary = Some(true),
                    "-binary" | "text" => binary = Some(false),
                    "linguist-generated" | "linguist-generated=true" => generated = Some(true),
                    "-linguist-generated" | "linguist-generated=false" => generated = Some(false),
                    "linguist-vendored" | "linguist-vendored=true" => vendored = Some(true),
                    "-linguist-vendored" | "linguist-vendored=false" => vendored = Some(false),
                    _ => {}
                }
            }
            if binary.is_some() || generated.is_some() || vendored.is_some() {
                rules.push(GitAttributeRule {
                    pattern: pattern.to_string(),
                    binary,
                    generated,
                    vendored,
                });
            }
        }
        Self { rules }
    }

    fn exclusion_for(&self, path: &str) -> Option<PathExclusion> {
        let mut binary = false;
        let mut generated = false;
        let mut vendored = false;
        for rule in &self.rules {
            if gitattributes_pattern_matches(&rule.pattern, path) {
                if let Some(value) = rule.binary {
                    binary = value;
                }
                if let Some(value) = rule.generated {
                    generated = value;
                }
                if let Some(value) = rule.vendored {
                    vendored = value;
                }
            }
        }
        if vendored {
            Some(PathExclusion::Vendored)
        } else if generated {
            Some(PathExclusion::Generated)
        } else if binary {
            Some(PathExclusion::ArchiveOrBinary)
        } else {
            None
        }
    }
}

pub(super) fn classify_path_exclusion(
    path: &str,
    git_attrs: &GitAttributeRules,
) -> Option<PathExclusion> {
    if let Some(attr_exclusion) = git_attrs.exclusion_for(path) {
        return Some(attr_exclusion);
    }
    let components: Vec<&str> = path.split('/').collect();
    let filename = components.last().copied().unwrap_or(path);
    let filename_lower = filename.to_ascii_lowercase();

    for component in &components[..components.len().saturating_sub(1)] {
        let lower = component.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "vendor"
                | "vendored"
                | "third_party"
                | "third-party"
                | "node_modules"
                | "bower_components"
                | "extern"
                | "external"
        ) {
            return Some(PathExclusion::Vendored);
        }
        if matches!(
            lower.as_str(),
            "fixtures"
                | "fixture"
                | "testdata"
                | "test_data"
                | "test-data"
                | "snapshots"
                | "__snapshots__"
                | "goldens"
                | "golden"
        ) {
            return Some(PathExclusion::FixtureOrTestData);
        }
        if matches!(
            lower.as_str(),
            "generated" | "gen" | "dist" | "build" | "target" | "out" | "__pycache__" | "_site"
        ) {
            return Some(PathExclusion::Generated);
        }
    }

    if matches!(
        filename,
        "INDEX.md"
            | "CHANGELOG.md"
            | "Cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "poetry.lock"
            | "uv.lock"
            | "Gemfile.lock"
            | "composer.lock"
            | "go.sum"
            | "flake.lock"
            | "body-history.json"
    ) || filename_lower.ends_with(".min.js")
        || filename_lower.ends_with(".min.css")
        || filename_lower.ends_with(".map")
        || filename_lower.ends_with(".snap")
        || filename_lower.ends_with(".pb.go")
        || filename_lower.ends_with(".pb.rs")
        || filename_lower.ends_with("_pb2.py")
        || filename_lower.contains(".generated.")
    {
        return Some(PathExclusion::Generated);
    }

    if let Some((_, ext)) = filename_lower.rsplit_once('.')
        && matches!(
            ext,
            "zip"
                | "tar"
                | "gz"
                | "tgz"
                | "bz2"
                | "xz"
                | "zst"
                | "7z"
                | "rar"
                | "jar"
                | "war"
                | "whl"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "bmp"
                | "tiff"
                | "pdf"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
                | "mp3"
                | "mp4"
                | "wav"
                | "ogg"
                | "webm"
                | "wasm"
                | "bin"
                | "exe"
                | "dll"
                | "so"
                | "dylib"
                | "a"
                | "o"
                | "pyc"
                | "class"
                | "sqlite"
                | "db"
                | "sig"
                | "pack"
                | "idx"
        )
    {
        return Some(PathExclusion::ArchiveOrBinary);
    }
    None
}

fn gitattributes_pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
    if pattern.contains('/') {
        wildcard_match(pattern, path, true)
    } else {
        let basename = path.rsplit('/').next().unwrap_or(path);
        wildcard_match(pattern, basename, false)
    }
}

fn wildcard_match(pattern: &str, text: &str, slash_sensitive: bool) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let mut dp = vec![false; t.len() + 1];
    dp[0] = true;
    let mut i = 0;
    while i < p.len() {
        if slash_sensitive && i + 1 < p.len() && p[i] == b'*' && p[i + 1] == b'*' {
            let mut next = vec![false; t.len() + 1];
            let mut seen = false;
            for j in 0..=t.len() {
                if dp[j] {
                    seen = true;
                }
                next[j] = seen;
            }
            dp = next;
            i += 2;
            continue;
        }
        let mut next = vec![false; t.len() + 1];
        match p[i] {
            b'*' => {
                for j in 0..=t.len() {
                    next[j] =
                        dp[j] || (j > 0 && next[j - 1] && (!slash_sensitive || t[j - 1] != b'/'));
                }
            }
            b'?' => {
                for j in 1..=t.len() {
                    if dp[j - 1] && (!slash_sensitive || t[j - 1] != b'/') {
                        next[j] = true;
                    }
                }
            }
            byte => {
                for j in 1..=t.len() {
                    if dp[j - 1] && t[j - 1] == byte {
                        next[j] = true;
                    }
                }
            }
        }
        dp = next;
        i += 1;
    }
    dp[t.len()]
}

pub(super) fn is_binary_bytes(bytes: &[u8]) -> bool {
    let end = bytes.len().min(BINARY_SNIFF_BYTES);
    bytes[..end].contains(&0)
}

pub(super) fn has_generated_header_marker(bytes: &[u8]) -> bool {
    let prefix = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(prefix);
    for line in text.lines().take(GENERATED_HEADER_LINES) {
        let lower = line.to_ascii_lowercase();
        if lower.contains("@generated")
            || lower.contains("do not edit")
            || lower.contains("do not modify")
            || lower.contains("auto-generated")
            || lower.contains("autogenerated")
            || lower.contains("generated by")
            || lower.contains("automatically generated")
            || lower.contains("code generated by")
            || lower.contains("this file is generated")
            || lower.contains("generated from")
        {
            return true;
        }
    }
    false
}

pub(super) fn count_lines(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|&&byte| byte == b'\n').count();
    if bytes.last() == Some(&b'\n') {
        newlines
    } else {
        newlines + 1
    }
}
