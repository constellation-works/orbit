use std::path::Path;

pub(super) fn sbpl_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(super) fn sbpl_filter_for_deny_rule(rule: &str) -> String {
    if rule_can_use_subpath(rule) {
        let path = subpath_root(rule);
        format!("(subpath \"{}\")", sbpl_escape(&path))
    } else {
        let regex = glob_rule_to_regex(rule);
        format!("(regex \"{}\")", sbpl_escape(&regex))
    }
}

pub(super) fn sbpl_filter_for_allow_rule(rule: &str) -> String {
    if rule_can_use_subpath(rule) {
        let path = subpath_root(rule);
        format!("(subpath \"{}\")", sbpl_escape(&path))
    } else {
        let regex = glob_rule_to_regex(rule);
        format!("(regex \"{}\")", sbpl_escape(&regex))
    }
}

/// Whether the deny clause compiled from `rule` reaches `path` itself — that
/// is, whether the rule denies `path` or an ancestor directory of it.
///
/// Both clause shapes [`sbpl_filter_for_deny_rule`] can emit are bounded above
/// by the rule's longest non-glob prefix: `(subpath ROOT)` matches exactly
/// `ROOT` and below, and a `(regex ...)` built from the same rule can only
/// match paths under that prefix. Comparing against [`subpath_root`] therefore
/// answers the question for literal and trailing-`**` rules exactly, and
/// over-approximates for interior globs (`~/Library/*`), where the regex may
/// only match part of the subtree. Callers use this to decide whether a
/// credential path was denied, so over-approximating is the safe direction.
/// [ORB-10931]
pub(super) fn deny_rule_reaches_path(rule: &str, path: &Path) -> bool {
    path.starts_with(subpath_root(rule))
}

fn rule_can_use_subpath(rule: &str) -> bool {
    let trimmed = rule.trim_end_matches('/');
    if !contains_glob(trimmed) {
        return true;
    }
    let Some(prefix) = trimmed.strip_suffix("/**") else {
        return false;
    };
    !contains_glob(prefix)
}

fn contains_glob(value: &str) -> bool {
    value.contains('*') || value.contains('?')
}

fn glob_rule_to_regex(rule: &str) -> String {
    // L-0062: SBPL filters run on macOS, where default volumes resolve paths
    // case-insensitively, so regex filters must follow that identity model.
    //
    // We express that case-insensitivity with per-letter character classes
    // (`[Aa]`) rather than the Perl/PCRE inline flag `(?i)`. `sandbox-exec`'s
    // SBPL regex engine does NOT support `(?i)`: it parses the `(?i)` token and
    // then rejects the following `^` anchor with "unexpected ^ operator in
    // middle of expression", failing the whole profile with exit 65 before the
    // agent CLI starts (ORB-00372). Character classes are the SBPL-compatible
    // way to fold case.
    let mut out = String::from("^");
    let chars: Vec<char> = rule.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    out.push_str(".*");
                    i += 2;
                }
            }
            '*' => {
                out.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            c => {
                push_regex_literal_case_insensitive(&mut out, c);
                i += 1;
            }
        }
    }
    out.push('$');
    out
}

/// Emit a single literal path character into an SBPL regex with macOS-style
/// case-insensitivity (L-0062). ASCII letters become a two-element character
/// class (`a` -> `[Aa]`) so the filter matches case-variant paths on
/// case-insensitive volumes; every other character is metachar-escaped exactly
/// as [`push_regex_escaped`] would. This deliberately avoids the `(?i)` inline
/// flag, which `sandbox-exec` rejects (see [`glob_rule_to_regex`]).
fn push_regex_literal_case_insensitive(out: &mut String, c: char) {
    if c.is_ascii_alphabetic() {
        out.push('[');
        out.push(c.to_ascii_uppercase());
        out.push(c.to_ascii_lowercase());
        out.push(']');
    } else {
        push_regex_escaped(out, c);
    }
}

pub(super) fn push_regex_escaped(out: &mut String, c: char) {
    if matches!(
        c,
        '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\'
    ) {
        out.push('\\');
    }
    out.push(c);
}

pub(super) fn push_regex_escaped_str(out: &mut String, value: &str) {
    for c in value.chars() {
        push_regex_escaped(out, c);
    }
}

/// Strip glob suffixes from a rule so it can be used as a `subpath` root.
/// `subpath` matches a directory and everything beneath, so `**` wildcards
/// are redundant and `*` segments cannot be expressed in SBPL — we collapse
/// them to the deepest directory above the first glob. Cutting at the glob
/// itself would leave a partial component (`Key` for `Key*`) that no path
/// under `Keychains` starts with.
fn subpath_root(rule: &str) -> String {
    let trimmed = rule.trim_end_matches('/');
    let trimmed = trimmed.trim_end_matches("/**");
    if let Some(idx) = trimmed.find(['*', '?']) {
        let prefix = &trimmed[..idx];
        let prefix = prefix.rfind('/').map_or("", |slash| &prefix[..slash]);
        if prefix.is_empty() {
            "/".to_string()
        } else {
            prefix.to_string()
        }
    } else if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}
