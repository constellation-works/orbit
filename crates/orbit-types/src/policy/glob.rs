//! Glob-matching helpers shared by policy evaluation and artifact scopes.
//!
//! Originally lived as private helpers inside
//! [`crate::types::policy_def`]; promoted here so `orbit-store`'s
//! artifact stores can reuse the same matcher without taking a dependency
//! on `orbit-policy` (which the architecture diagram forbids). The original
//! call sites in `policy_def` now route through these public functions, so
//! existing behavior is unchanged.
//!
//! # Semantics
//! The grammar supports the three glob operators agents already see in
//! `denyRead`/`denyModify` rules and `fsProfile` patterns:
//! - `**` — any number of path segments (including zero).
//! - `*` — any sequence of non-separator characters.
//! - `?` — any single non-separator character.
//!
//! All inputs are normalized to forward-slash separators and stripped of
//! leading `./`. Paths that escape the workspace (`..`, `~`, absolute) are
//! rejected as `PolicyError::Invalid`.

use std::path::{Component, Path};

use regex::{Regex, RegexBuilder};

use super::PolicyError;

/// Normalize a workspace-relative path for glob matching.
///
/// Trims whitespace, replaces backslashes with forward slashes, strips a
/// leading `./`, and rejects paths that try to escape the workspace
/// (`..`, `~`, absolute). Returns an empty string for the workspace root
/// (`.`).
///
/// [ORB-10009] The result is rebuilt from the path's normal components, so
/// redundant separators collapse to a canonical spelling: `a/./b`, `a//b`,
/// and `a/b/` all normalize to `a/b`. Without this, a deny rule such as
/// `secret/key.txt` could be dodged by spelling the same file
/// `secret/./key.txt` while a broad allow rule (`**`) still matched.
pub fn normalize_glob_path(path: &str) -> Result<String, PolicyError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::Invalid(
            "filesystem path must not be empty".to_string(),
        ));
    }

    let mut normalized = trimmed.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    if normalized == "." {
        normalized.clear();
    }
    if normalized == "~"
        || normalized.starts_with("~/")
        || normalized.starts_with("../")
        || normalized == ".."
    {
        return Err(PolicyError::Invalid(format!(
            "filesystem path `{path}` must stay inside the workspace root"
        )));
    }

    let path_ref = Path::new(&normalized);
    if path_ref.is_absolute() {
        return Err(PolicyError::Invalid(format!(
            "filesystem path `{path}` must stay inside the workspace root"
        )));
    }

    for component in path_ref.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PolicyError::Invalid(format!(
                    "filesystem path `{path}` must stay inside the workspace root"
                )));
            }
        }
    }

    // [ORB-10009] Canonical spelling: drop `.` segments, duplicate and
    // trailing separators, so rule matching cannot be dodged by an
    // equivalent-but-differently-spelled path.
    Ok(join_normal_components(path_ref))
}

/// Rebuild a validated relative path from its `Normal` components, dropping
/// `.` segments and redundant separators. The input must already be checked
/// for `..` / root / prefix components. [ORB-10009]
pub fn join_normal_components(path: &Path) -> String {
    let mut rebuilt = String::new();
    for component in path.components() {
        if let Component::Normal(part) = component {
            if !rebuilt.is_empty() {
                rebuilt.push('/');
            }
            rebuilt.push_str(&part.to_string_lossy());
        }
    }
    rebuilt
}

/// Test whether `rule` (a glob pattern) matches `path` (already normalized
/// via [`normalize_glob_path`]).
pub fn match_glob(rule: &str, path: &str) -> Result<bool, PolicyError> {
    let regex = compile_glob_regex(rule).map_err(|error| {
        PolicyError::Invalid(format!("invalid filesystem glob `{rule}`: {error}"))
    })?;
    Ok(regex.is_match(path))
}

/// Compile a glob pattern into an anchored regex.
///
/// Exposed as a building block for callers that want to cache compiled
/// regexes across many path checks (e.g. evaluating a single rule against
/// hundreds of candidate paths in a hot path).
pub fn compile_glob_regex(rule: &str) -> Result<Regex, regex::Error> {
    if rule == "." {
        return compile_filesystem_regex(r"^$");
    }

    if let Some(prefix) = rule.strip_suffix("/**") {
        if prefix.is_empty() {
            return compile_filesystem_regex(r"^.*$");
        }
        // Route the prefix through the same segment-aware translation as the
        // general path below. Escaping the whole prefix would turn any `*`/`?`
        // in it into a literal, so `**/secrets/**` would compile to
        // `^\*\*/secrets(?:/.*)?$` and match nothing — silently voiding a deny
        // rule (H1). Translating instead yields `^(?:.*/)?secrets(?:/.*)?$`.
        let body = translate_glob_body(prefix);
        return compile_filesystem_regex(&format!("^{body}(?:/.*)?$"));
    }

    let body = translate_glob_body(rule);
    compile_filesystem_regex(&format!("^{body}$"))
}

/// Translate a glob pattern into an *unanchored* regex body, honoring the
/// segment-aware operators (`**/`, `**`, `*`, `?`) and escaping every other
/// character. Callers wrap the result in anchors (`^`…`$`) and any suffix.
fn translate_glob_body(rule: &str) -> String {
    let chars: Vec<char> = rule.chars().collect();
    let mut index = 0usize;
    let mut pattern = String::new();
    while index < chars.len() {
        if chars[index] == '*' {
            if index + 2 < chars.len() && chars[index + 1] == '*' && chars[index + 2] == '/' {
                pattern.push_str("(?:.*/)?");
                index += 3;
                continue;
            }
            if index + 1 < chars.len() && chars[index + 1] == '*' {
                pattern.push_str(".*");
                index += 2;
                continue;
            }
            pattern.push_str("[^/]*");
            index += 1;
            continue;
        }

        if chars[index] == '?' {
            pattern.push_str("[^/]");
            index += 1;
            continue;
        }

        pattern.push_str(&regex::escape(&chars[index].to_string()));
        index += 1;
    }
    pattern
}

fn compile_filesystem_regex(pattern: &str) -> Result<Regex, regex::Error> {
    // L-0062: Match policy globs using the target filesystem's case identity.
    RegexBuilder::new(pattern)
        .case_insensitive(filesystem_globs_are_case_insensitive())
        .build()
}

fn filesystem_globs_are_case_insensitive() -> bool {
    cfg!(any(target_os = "macos", windows))
}

/// Which directories a glob rule can name a not-yet-existing path into.
///
/// [`match_glob`] answers "does this rule match this path", which is the only
/// question a request-time check needs. A caller compiling an inode-based
/// kernel ruleset needs the other direction: a rule that matches nothing today
/// may still name a path tomorrow, and the ruleset is fixed before that
/// happens. Asking per directory is what lets such a caller decide which
/// directories it may hand out wholesale.
///
/// # Bounded and unbounded reach
/// A rule's own segments normally bound the directories it can name into:
/// `secrets/**` and `.env` can only produce a path beneath the workspace root,
/// `config/*.key` beneath `config`. A directory-crossing `**` removes that
/// bound — `**/.env` names `.env` in every directory, including directories
/// that do not exist yet — which callers with a per-directory cost need to
/// distinguish. See [`GlobReach::is_bounded`].
#[derive(Debug)]
pub struct GlobReach {
    segments: Vec<ReachSegment>,
    bounded: bool,
    /// `.` names the workspace root itself and nothing beneath it.
    root_only: bool,
}

#[derive(Debug)]
enum ReachSegment {
    /// A trailing `**`: everything below this point, at any depth.
    Subtree,
    /// One path segment, matched with the segment-local operators (`*`, `?`).
    Name(Regex),
}

impl GlobReach {
    /// Compile `rule` — a pattern already stripped of any `!` prefix and
    /// normalized to forward slashes — for per-directory reach questions.
    pub fn compile(rule: &str) -> Result<Self, regex::Error> {
        let raw: Vec<&str> = rule.split('/').filter(|part| !part.is_empty()).collect();
        let bounded = raw
            .iter()
            .enumerate()
            .all(|(index, part)| !part.contains("**") || (*part == "**" && index + 1 == raw.len()));

        let mut segments = Vec::with_capacity(raw.len());
        for part in &raw {
            if *part == "**" {
                segments.push(ReachSegment::Subtree);
            } else {
                segments.push(ReachSegment::Name(compile_filesystem_regex(&format!(
                    "^{}$",
                    translate_glob_body(part)
                ))?));
            }
        }

        Ok(Self {
            segments,
            bounded,
            root_only: rule == ".",
        })
    }

    /// Whether the rule's reach is bounded by its own segments.
    ///
    /// False when a `**` crosses directory boundaries, which lets the rule name
    /// a path beneath a directory the rule text never mentions. A caller that
    /// pays a cost per reached directory cannot enumerate that set.
    pub fn is_bounded(&self) -> bool {
        self.bounded
    }

    /// Whether the rule can name a path *strictly beneath* the directory
    /// `dir`, expressed workspace-relative (`""` or `"."` for the root).
    ///
    /// This asks about names, not about what exists: `secrets/**` names a path
    /// beneath the root whether or not `secrets` has ever been created. A rule
    /// that matches `dir` itself and nothing below it does not reach beneath
    /// it.
    pub fn names_beneath(&self, dir: &str) -> bool {
        if self.root_only {
            return false;
        }
        let dir: Vec<&str> = dir
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
            .collect();
        names_beneath(&self.segments, &dir)
    }
}

fn names_beneath(pattern: &[ReachSegment], dir: &[&str]) -> bool {
    match (pattern.split_first(), dir.split_first()) {
        // The rule ran out of segments at or above `dir`, so everything it can
        // name lies outside this directory.
        (None, _) => false,
        // The rule still has segments to spend below `dir`.
        (Some(_), None) => true,
        (Some((ReachSegment::Subtree, _)), Some(_)) => true,
        (Some((ReachSegment::Name(matcher), rest)), Some((name, below))) => {
            matcher.is_match(name) && names_beneath(rest, below)
        }
    }
}
