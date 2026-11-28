//! [ORB-29076] Consolidated scenario rows: the rows a `scenario_table!` runs
//! inside one libtest. A row's Bubblewrap deferral names the row, not the
//! libtest, so a Cargo filter on the row runs no test. The runnable native
//! check is the enclosing libtest, and it validates the row only when it
//! prints the row's `qa-scenario-pass:` marker.
//!
//! The index is read from the candidate's Git tree, like [`CargoOwners`], and
//! a row yields a home only when exactly one libtest in the workspace runs it.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use git2::{ObjectType, Repository, Tree};
use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

use super::cargo_target::CargoOwners;
use super::evidence::deferred_test;

/// The libtest a scenario row runs in: its package, integration target and
/// the test path `cargo test` filters on (`module::table`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ScenarioHome {
    package: String,
    target: String,
    test: String,
}

/// Scenario rows keyed by row name. A row can be indexed without a home when
/// several libtests run it or when a `#[path]` module runs it, which makes the
/// row ambiguous.
#[derive(Default)]
pub(super) struct ScenarioRows {
    homes: BTreeMap<String, BTreeSet<ScenarioHome>>,
    ambiguous: BTreeSet<String>,
}

impl ScenarioRows {
    /// Whether `row` is a row some `scenario_table!` in the candidate runs.
    /// Such a deferral never names a runnable Cargo filter.
    pub(super) fn is_row(&self, row: &str) -> bool {
        self.homes.contains_key(row) || self.ambiguous.contains(row)
    }

    /// The one libtest that runs `row`, when the candidate proves exactly one.
    fn unique_home(&self, row: &str) -> Option<&ScenarioHome> {
        if self.ambiguous.contains(row) {
            return None;
        }
        let homes = self.homes.get(row)?;
        homes.iter().next().filter(|_| homes.len() == 1)
    }

    /// Index every row of the `tests/<target>` directory `dir` of `package`.
    /// Plain-`mod` modules resolve `package::target::module::table` homes even
    /// when other modules in the target use `#[path]`; rows inside
    /// `#[path]`-mounted files, modules declared twice or undeclared modules
    /// never resolve a unique home.
    pub(super) fn scan_target(
        &mut self,
        repo: &Repository,
        dir: &Tree<'_>,
        package: &str,
        target: &str,
    ) {
        let mut files = BTreeMap::new();
        if collect_modules(repo, dir, "", "", &mut files).is_none() {
            return;
        }
        let path_reached = path_reached_files(&files);
        for (module, file) in &files {
            if path_reached.contains(&file.fs_path) {
                for (_, rows) in &file.tables {
                    for row in rows {
                        self.ambiguous.insert(row.clone());
                    }
                }
                continue;
            }
            if module.is_empty() || !plain_declared_chain(&files, &path_reached, module) {
                continue;
            }
            for (table, rows) in &file.tables {
                let home = ScenarioHome {
                    package: package.to_string(),
                    target: target.to_string(),
                    test: format!("{module}::{table}"),
                };
                for row in rows {
                    self.homes
                        .entry(row.clone())
                        .or_default()
                        .insert(home.clone());
                }
            }
        }
    }
}

struct TargetFile {
    fs_path: String,
    plain_mods: BTreeSet<String>,
    path_targets: Vec<String>,
    tables: Vec<(String, Vec<String>)>,
}

enum Tok<'a> {
    Ident(&'a str),
    Str(Cow<'a, str>),
    Punct(char),
    Other,
}

/// Operator diagnostics for required records whose deferral notices name a
/// scenario row. A row with one home gets the native check for that libtest
/// and its marker; any other row names the uncovered command and the
/// fresh-review route. Neither offers the row as a Cargo filter.
pub(super) fn unreconciled_scenario_deferrals(
    records: &[ReviewValidation],
    owners: &CargoOwners,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    for record in records {
        if record.role != ValidationRole::Required
            || record.outcome != ValidationOutcome::Passed
            || record.deferred.is_empty()
        {
            continue;
        }
        let rows = owners.scenarios();
        let label = match record.record_id() {
            Some(id) => format!("`{id}` (`{}`)", record.command),
            None => format!("`{}`", record.command),
        };
        for row in record
            .deferred
            .iter()
            .filter_map(|notice| deferred_test(notice))
            .filter(|row| rows.is_row(row))
        {
            diagnostics.push(match rows.unique_home(row) {
                Some(home) => format!(
                    "unreconciled_scenario_deferral: required record {label} defers scenario row \
                     `{row}`, which runs only inside `{}` of package `{}` (`--test {}`). Run \
                     `cargo test -p {} --test {} {}` natively outside the sandbox without \
                     deferral and confirm its output prints `qa-scenario-pass: {row}`. That \
                     validates the row alone: a parent test pass without that marker, or a \
                     repeated deferral, does not settle it, and the broader required record \
                     `{}` still needs its own admissible validation, so admit a fresh review \
                     with matching required and host_sandbox_test records",
                    home.test,
                    home.package,
                    home.target,
                    home.package,
                    home.target,
                    home.test,
                    record.command,
                ),
                None => format!(
                    "unreconciled_scenario_deferral: required record {label} defers scenario row \
                     `{row}`, which no single libtest in the candidate runs, so no native check \
                     is named for it. Run `{}` natively outside the sandbox without deferral and \
                     admit a fresh review with that result",
                    record.command,
                ),
            });
        }
    }
    diagnostics
}

/// Module path to parsed target file for every `.rs` file of a target directory.
/// The root (`main.rs`) is keyed by the empty path. `None` when two files claim
/// one module or the tree holds an unreadable entry.
fn collect_modules(
    repo: &Repository,
    dir: &Tree<'_>,
    module: &str,
    fs_prefix: &str,
    files: &mut BTreeMap<String, TargetFile>,
) -> Option<()> {
    for item in dir {
        let name = item.name().ok()?;
        match item.kind()? {
            ObjectType::Blob => {
                let Some(stem) = name.strip_suffix(".rs") else {
                    continue;
                };
                let key = match (stem, module.is_empty()) {
                    ("main", true) => String::new(),
                    ("mod", _) => module.to_string(),
                    _ => join(module, stem),
                };
                if key.is_empty() && stem != "main" {
                    return None;
                }
                let blob = repo.find_blob(item.id()).ok()?;
                let text = String::from_utf8(blob.content().to_vec()).ok()?;
                let fs_path = format!("{fs_prefix}{name}");
                let parsed = parse_file(&fs_path, &text);
                if files.insert(key, parsed).is_some() {
                    return None;
                }
            }
            ObjectType::Tree => {
                let sub = item.to_object(repo).ok()?.peel_to_tree().ok()?;
                collect_modules(
                    repo,
                    &sub,
                    &join(module, name),
                    &format!("{fs_prefix}{name}/"),
                    files,
                )?;
            }
            _ => return None,
        }
    }
    Some(())
}

fn join(module: &str, segment: &str) -> String {
    if module.is_empty() {
        segment.to_string()
    } else {
        format!("{module}::{segment}")
    }
}

/// Files within `tests/<target>` reached through `#[path]` (directly, or as a
/// plain-`mod` descendant of a `#[path]`-loaded file).
fn path_reached_files(files: &BTreeMap<String, TargetFile>) -> BTreeSet<String> {
    let by_fs_path: BTreeMap<&str, &str> = files
        .iter()
        .map(|(key, file)| (file.fs_path.as_str(), key.as_str()))
        .collect();
    let mut visited = BTreeSet::new();
    let mut path_reached = BTreeSet::new();
    let mut pending = Vec::new();
    if let Some(root) = files.get("") {
        visited.insert((root.fs_path.clone(), false));
        pending.push((String::new(), false));
    }
    while let Some((module_key, via_path)) = pending.pop() {
        let Some(file) = files.get(&module_key) else {
            continue;
        };
        for child in &file.plain_mods {
            if !via_path {
                let child_key = join(&module_key, child);
                if let Some(child_file) = files.get(&child_key)
                    && visited.insert((child_file.fs_path.clone(), false))
                {
                    pending.push((child_key, false));
                }
            } else {
                let dir = parent_dir(&file.fs_path);
                let file_candidate = join_slash(dir, &format!("{child}.rs"));
                let mod_candidate = join_slash(dir, &format!("{child}/mod.rs"));
                for candidate in [file_candidate, mod_candidate] {
                    if let Some(&child_key) = by_fs_path.get(candidate.as_str()) {
                        path_reached.insert(candidate.clone());
                        if visited.insert((candidate, true)) {
                            pending.push((child_key.to_string(), true));
                        }
                    }
                }
            }
        }
        for target_fs in &file.path_targets {
            if let Some(&target_key) = by_fs_path.get(target_fs.as_str()) {
                path_reached.insert(target_fs.clone());
                if visited.insert((target_fs.clone(), true)) {
                    pending.push((target_key.to_string(), true));
                }
            }
        }
    }
    path_reached
}

/// Whether every segment of `module` is declared by a plain `mod` item in its
/// parent module and no file along the chain is mounted via `#[path]`.
fn plain_declared_chain(
    files: &BTreeMap<String, TargetFile>,
    path_reached: &BTreeSet<String>,
    module: &str,
) -> bool {
    let segments: Vec<&str> = module.split("::").collect();
    (0..segments.len()).all(|index| {
        let parent = segments[..index].join("::");
        files.get(&parent).is_some_and(|file| {
            !path_reached.contains(&file.fs_path) && file.plain_mods.contains(segments[index])
        })
    }) && files
        .get(module)
        .is_some_and(|file| !path_reached.contains(&file.fs_path))
}

fn parse_file(fs_path: &str, text: &str) -> TargetFile {
    let tokens = tokenize(text);
    let base_dir = parent_dir(fs_path);
    let (plain_mods, path_targets) = extract_mod_decls(base_dir, &tokens);
    let tables = scenario_tables_from_tokens(&tokens);
    TargetFile {
        fs_path: fs_path.to_string(),
        plain_mods,
        path_targets,
        tables,
    }
}

/// Top-level plain `mod <name>;` declarations and normalized `#[path]` targets
/// inside `tests/<target>`.
fn extract_mod_decls(base_dir: &str, tokens: &[Tok<'_>]) -> (BTreeSet<String>, Vec<String>) {
    let mut plain_mods = BTreeSet::new();
    let mut path_targets = Vec::new();
    let mut depth = 0_usize;
    let mut has_path_attr = false;
    let mut index = 0;
    while index < tokens.len() {
        match &tokens[index] {
            Tok::Punct('{') => {
                depth += 1;
                has_path_attr = false;
                index += 1;
            }
            Tok::Punct('}') => {
                depth = depth.saturating_sub(1);
                has_path_attr = false;
                index += 1;
            }
            Tok::Punct('#')
                if depth == 0 && matches!(tokens.get(index + 1), Some(Tok::Punct('['))) =>
            {
                if let Some(end) = skip_balanced(tokens, index + 1, '[', ']') {
                    let attr_tokens = &tokens[index + 2..end - 1];
                    if let Some(raw_path) = extract_path_attr(attr_tokens) {
                        has_path_attr = true;
                        if let Some(rel) = normalize_rel_path(base_dir, &raw_path) {
                            path_targets.push(rel);
                        }
                    }
                    index = end;
                } else {
                    // An unclosed attribute consumes the rest of this file; do
                    // not inspect a partial token range or infer a module path.
                    break;
                }
            }
            Tok::Ident("pub") if depth == 0 => {
                index += 1;
                if matches!(tokens.get(index), Some(Tok::Punct('('))) {
                    index = skip_balanced(tokens, index, '(', ')').unwrap_or(tokens.len());
                }
            }
            Tok::Ident("mod") if depth == 0 => {
                if let [Tok::Ident(name), Tok::Punct(';'), ..] = &tokens[index + 1..] {
                    if !has_path_attr {
                        plain_mods.insert((*name).to_string());
                    }
                    has_path_attr = false;
                    index += 3;
                } else {
                    has_path_attr = false;
                    index += 1;
                }
            }
            _ => {
                if depth == 0 {
                    has_path_attr = false;
                }
                index += 1;
            }
        }
    }
    (plain_mods, path_targets)
}

fn extract_path_attr(attr_tokens: &[Tok<'_>]) -> Option<String> {
    if let [Tok::Ident("path"), Tok::Punct('='), Tok::Str(path)] = attr_tokens {
        return Some(path.to_string());
    }
    if matches!(attr_tokens.first(), Some(Tok::Ident("cfg_attr"))) {
        for window in attr_tokens.windows(3) {
            if let [Tok::Ident("path"), Tok::Punct('='), Tok::Str(path)] = window {
                return Some(path.to_string());
            }
        }
    }
    None
}

fn skip_balanced(tokens: &[Tok<'_>], start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0_usize;
    let mut index = start;
    while index < tokens.len() {
        match tokens[index] {
            Tok::Punct(ch) if ch == open => depth += 1,
            Tok::Punct(ch) if ch == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index + 1);
                }
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn parent_dir(fs_path: &str) -> &str {
    fs_path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn join_slash(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn normalize_rel_path(base_dir: &str, raw_path: &str) -> Option<String> {
    if raw_path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for segment in raw_path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

/// The `(table, rows)` of every `scenario_table!(table, row, …)` in `tokens`.
fn scenario_tables_from_tokens(tokens: &[Tok<'_>]) -> Vec<(String, Vec<String>)> {
    let mut tables = Vec::new();
    let mut index = 0;
    while index + 3 < tokens.len() {
        let close = match (&tokens[index], &tokens[index + 1], &tokens[index + 2]) {
            (Tok::Ident("scenario_table"), Tok::Punct('!'), Tok::Punct('(')) => ')',
            (Tok::Ident("scenario_table"), Tok::Punct('!'), Tok::Punct('[')) => ']',
            (Tok::Ident("scenario_table"), Tok::Punct('!'), Tok::Punct('{')) => '}',
            _ => {
                index += 1;
                continue;
            }
        };
        let (args, next) = parse_table_args(tokens, index + 3, close);
        if let Some((table, rows)) = args.split_first()
            && !rows.is_empty()
        {
            tables.push((
                (*table).to_string(),
                rows.iter().map(|row| (*row).to_string()).collect(),
            ));
        }
        index = next;
    }
    tables
}

fn parse_table_args<'a>(tokens: &'a [Tok<'a>], start: usize, close: char) -> (Vec<&'a str>, usize) {
    let mut args = Vec::new();
    let mut cursor = start;
    while cursor < tokens.len() {
        match tokens.get(cursor) {
            Some(Tok::Punct(ch)) if *ch == close => return (args, cursor + 1),
            Some(Tok::Ident(name)) => {
                args.push(*name);
                cursor += 1;
                match tokens.get(cursor) {
                    Some(Tok::Punct(ch)) if *ch == close => return (args, cursor + 1),
                    Some(Tok::Punct(',')) => cursor += 1,
                    _ => return (Vec::new(), cursor),
                }
            }
            _ => return (Vec::new(), cursor + 1),
        }
    }
    (Vec::new(), cursor)
}

/// Splits Rust source into lexical tokens, dropping comments and character
/// literals while preserving strings as [`Tok::Str`].
fn tokenize(source: &str) -> Vec<Tok<'_>> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = skip_block_comment(bytes, index);
        } else if byte == b'"' {
            let (content, next) = read_quoted(source, index + 1);
            tokens.push(Tok::Str(content));
            index = next;
        } else if byte == b'\'' {
            let next = skip_char_literal_or_lifetime(source, index);
            if next == index + 1 {
                tokens.push(Tok::Punct('\''));
            }
            index = next;
        } else if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let ident = &source[start..index];
            let mut hashes = 0;
            while bytes.get(index + hashes) == Some(&b'#') {
                hashes += 1;
            }
            if matches!(ident, "r" | "br" | "cr") && bytes.get(index + hashes) == Some(&b'"') {
                let (content, next) = read_raw_quoted(source, index + hashes + 1, hashes);
                tokens.push(Tok::Str(Cow::Borrowed(content)));
                index = next;
            } else if matches!(ident, "b" | "c") && bytes.get(index) == Some(&b'"') {
                let (content, next) = read_quoted(source, index + 1);
                tokens.push(Tok::Str(content));
                index = next;
            } else if ident == "r"
                && hashes == 1
                && bytes
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_alphabetic() || *next == b'_')
            {
                let raw_start = index + 1;
                let mut raw_end = raw_start + 1;
                while raw_end < bytes.len()
                    && (bytes[raw_end].is_ascii_alphanumeric() || bytes[raw_end] == b'_')
                {
                    raw_end += 1;
                }
                tokens.push(Tok::Ident(&source[raw_start..raw_end]));
                index = raw_end;
            } else {
                tokens.push(Tok::Ident(ident));
            }
        } else if byte.is_ascii_digit() {
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            tokens.push(Tok::Other);
        } else if byte.is_ascii_punctuation() {
            tokens.push(Tok::Punct(byte as char));
            index += 1;
        } else {
            let step = source[index..].chars().next().map_or(1, char::len_utf8);
            tokens.push(Tok::Other);
            index += step;
        }
    }
    tokens
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0_usize;
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            depth += 1;
            index += 2;
        } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
            depth -= 1;
            index += 2;
            if depth == 0 {
                break;
            }
        } else {
            index += 1;
        }
    }
    index
}

fn skip_char_literal_or_lifetime(source: &str, start: usize) -> usize {
    let bytes = source.as_bytes();
    match bytes.get(start + 1) {
        Some(&b'\\') => {
            let mut index = start + 2;
            if index < bytes.len() {
                index += source[index..].chars().next().map_or(1, char::len_utf8);
            }
            while index < bytes.len() && !matches!(bytes[index], b'\'' | b'\n' | b'\r') {
                index += source[index..].chars().next().map_or(1, char::len_utf8);
            }
            if bytes.get(index) == Some(&b'\'') {
                index + 1
            } else {
                start + 1
            }
        }
        Some(&next) if !matches!(next, b'\'' | b'\n' | b'\r') => {
            let ch_len = source[start + 1..].chars().next().map_or(0, char::len_utf8);
            if ch_len > 0 && bytes.get(start + 1 + ch_len) == Some(&b'\'') {
                start + 1 + ch_len + 1
            } else {
                start + 1
            }
        }
        _ => start + 1,
    }
}

fn read_quoted(source: &str, start: usize) -> (Cow<'_, str>, usize) {
    let bytes = source.as_bytes();
    let mut index = start;
    let mut escaped = false;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let raw = &source[start..index];
                let value = if escaped {
                    Cow::Owned(unescape_basic(raw))
                } else {
                    Cow::Borrowed(raw)
                };
                return (value, index + 1);
            }
            b'\\' => {
                escaped = true;
                index += 1;
                if index < bytes.len() {
                    index += source[index..].chars().next().map_or(1, char::len_utf8);
                }
            }
            _ => {
                index += source[index..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    (Cow::Borrowed(&source[start..]), bytes.len())
}

fn unescape_basic(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('0') => out.push('\0'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn read_raw_quoted(source: &str, start: usize, hashes: usize) -> (&str, usize) {
    let bytes = source.as_bytes();
    let mut index = start;
    while index < bytes.len() {
        if bytes[index] == b'"'
            && bytes
                .get(index + 1..index + 1 + hashes)
                .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
        {
            return (&source[start..index], index + 1 + hashes);
        }
        index += 1;
    }
    (&source[start..], bytes.len())
}

#[cfg(test)]
mod tests {
    use super::parse_file;

    #[test]
    fn incomplete_attributes_are_ignored_and_complete_modules_still_resolve() {
        let incomplete = parse_file("tests/main.rs", "#[");
        assert!(incomplete.plain_mods.is_empty());
        assert!(incomplete.path_targets.is_empty());

        let incomplete_path =
            parse_file("tests/main.rs", "#[cfg_attr(any(), path = \"partial.rs\")");
        assert!(incomplete_path.plain_mods.is_empty());
        assert!(incomplete_path.path_targets.is_empty());

        let complete = parse_file(
            "tests/main.rs",
            "mod plain; #[path = \"nested.rs\"] mod mounted;",
        );
        assert!(complete.plain_mods.contains("plain"));
        assert_eq!(complete.path_targets, ["tests/nested.rs"]);
    }
}
