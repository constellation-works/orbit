use super::*;

pub(super) fn positive_mount_roots(
    profile: &ResolvedFsProfile,
    expanded: &GlobMatches<'_>,
) -> Result<Vec<PathBuf>, OrbitError> {
    let mut roots = BTreeSet::new();
    for rule in profile.modify.iter().filter(|rule| !rule.starts_with('!')) {
        for root in mount_paths_for_rule(rule, false, expanded)? {
            roots.insert(root);
        }
    }
    Ok(roots.into_iter().collect())
}

/// The mount sources for one rule body. A non-subtree rule reads the
/// compile's expansion, which must already cover it.
pub(super) fn mount_paths_for_rule(
    rule: &str,
    require_match: bool,
    expanded: &GlobMatches<'_>,
) -> Result<Vec<PathBuf>, OrbitError> {
    if is_exact_or_subtree(rule) {
        let root = rule.strip_suffix("/**").unwrap_or(rule);
        if !Path::new(root).exists() && !require_match {
            return Ok(Vec::new());
        }
        let path = canonical_existing(Path::new(root), "sandbox mount")?;
        return Ok(vec![path]);
    }
    let matches = expanded.get(rule).ok_or_else(|| {
        OrbitError::Execution(format!(
            "linux-bwrap modify rule `{rule}` was not expanded before mounting"
        ))
    })?;
    if require_match && matches.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "linux-bwrap modify rule `{rule}` has no existing path to mount"
        )));
    }
    Ok(matches.iter().cloned().collect())
}

pub(super) fn is_narrow_reallow(prior_rules: &[String], rule: &str) -> bool {
    let Some(root) = exact_or_subtree_root(rule) else {
        return false;
    };
    prior_rules
        .iter()
        .filter_map(|prior| prior.strip_prefix('!'))
        .filter_map(exact_or_subtree_root)
        .any(|denied| root != denied && root.starts_with(&denied))
}

pub(super) fn exact_or_subtree_root(rule: &str) -> Option<PathBuf> {
    is_exact_or_subtree(rule).then(|| PathBuf::from(rule.strip_suffix("/**").unwrap_or(rule)))
}

pub(super) fn is_exact_or_subtree(rule: &str) -> bool {
    let body = rule.strip_suffix("/**").unwrap_or(rule);
    !body.contains(['*', '?'])
}

pub(super) fn overlaps_writable_root(rule: &str, roots: &[PathBuf]) -> bool {
    let prefix = static_prefix(rule);
    roots
        .iter()
        .any(|root| root.starts_with(&prefix) || prefix.starts_with(root))
}

/// Deny rules that have nothing to `--ro-bind` at spawn: non-subtree globs,
/// and exact/subtree denies whose root is still absent.
///
/// An absent exact/subtree deny with a later nested re-allow is omitted.
/// Grant preparation materializes that re-allow (creating the deny root) so
/// `--ro-bind` can apply; a snapshot taken before that create, such as a
/// standalone [`LinuxBwrapPostRunGuard::capture`], would otherwise
/// false-positive on it. The spawn path snapshots from the argv compile,
/// after preparation, where a materialized root is simply an existing deny.
pub(super) fn post_run_deny_rules(profile: &ResolvedFsProfile) -> Vec<String> {
    profile
        .modify
        .iter()
        .enumerate()
        .filter_map(|(index, rule)| {
            let denied = rule.strip_prefix('!')?;
            if is_exact_or_subtree(denied) {
                let root = denied.strip_suffix("/**").unwrap_or(denied);
                if Path::new(root).exists() || deny_has_nested_reallow(&profile.modify, index) {
                    return None;
                }
            }
            Some(denied.to_string())
        })
        .collect()
}

fn deny_has_nested_reallow(modify: &[String], deny_index: usize) -> bool {
    let Some(denied_root) = modify
        .get(deny_index)
        .and_then(|rule| rule.strip_prefix('!'))
        .and_then(exact_or_subtree_root)
    else {
        return false;
    };
    modify.iter().skip(deny_index + 1).any(|rule| {
        !rule.starts_with('!')
            && exact_or_subtree_root(rule)
                .is_some_and(|root| root != denied_root && root.starts_with(&denied_root))
    })
}

/// Every existing path matched by any of `rules`, from one walk per search
/// root.
pub(super) fn expand_rules(rules: &[String]) -> Result<BTreeSet<PathBuf>, OrbitError> {
    Ok(expand_each_rule(rules.iter().map(String::as_str))?
        .into_values()
        .flatten()
        .collect())
}

/// The existing paths each glob rule body matches, keyed by that body.
pub(super) type GlobMatches<'a> = BTreeMap<&'a str, BTreeSet<PathBuf>>;

/// Every existing path matched by each of `rules`, keyed by rule.
///
/// Rules are grouped by the directory their static prefix resolves to and
/// each such directory is walked once, however many rules share it. The
/// shipped default policy carries four non-subtree denies (`**/.env` and
/// friends) whose prefix is the workspace root, and an argv compile plus the
/// post-run guard need them before and after every sandboxed invocation; one
/// walk per rule per phase made that a dozen full workspace traversals
/// (including `target/` and `.git/`) per agent step.
pub(super) fn expand_each_rule<'a>(
    rules: impl IntoIterator<Item = &'a str>,
) -> Result<GlobMatches<'a>, OrbitError> {
    let mut matches = GlobMatches::new();
    let mut by_root: BTreeMap<PathBuf, Vec<(&str, Regex, PathBuf)>> = BTreeMap::new();
    for rule in rules {
        if matches.contains_key(rule) {
            continue;
        }
        matches.insert(rule, BTreeSet::new());
        let regex = compile_rule_regex(rule)?;
        let prefix = static_prefix(rule);
        let display_root = existing_ancestor(&prefix)?;
        let root = canonical_existing(&display_root, "glob search root")?;
        by_root
            .entry(root)
            .or_default()
            .push((rule, regex, display_root));
    }
    for (root, matchers) in by_root {
        let mut candidates = Vec::new();
        walk_paths(&root, &mut candidates)?;
        for candidate in candidates {
            let rendered = render_glob_path(&candidate);
            let relative = candidate.strip_prefix(&root).map_err(|error| {
                OrbitError::Execution(format!(
                    "glob candidate `{}` must remain beneath search root `{}`: {error}",
                    candidate.display(),
                    root.display()
                ))
            })?;
            // Canonicalize a candidate at most once, however many rules it hits.
            let mut canonical: Option<PathBuf> = None;
            for (rule, regex, display_root) in &matchers {
                if regex.is_match(&rendered)
                    || regex.is_match(&render_glob_path(&display_root.join(relative)))
                {
                    let path = match &canonical {
                        Some(path) => path,
                        None => {
                            canonical.insert(canonical_existing(&candidate, "denyModify match")?)
                        }
                    };
                    matches.entry(rule).or_default().insert(path.clone());
                }
            }
        }
    }
    Ok(matches)
}

fn static_prefix(rule: &str) -> PathBuf {
    let wildcard = rule.find(['*', '?']).unwrap_or(rule.len());
    let literal = &rule[..wildcard];
    let boundary = literal.rfind('/').unwrap_or(0);
    let prefix = if wildcard == rule.len() {
        literal
    } else if boundary == 0 {
        "/"
    } else {
        &literal[..boundary]
    };
    PathBuf::from(prefix)
}

fn existing_ancestor(path: &Path) -> Result<PathBuf, OrbitError> {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if !current.pop() {
            return Err(OrbitError::InvalidInput(format!(
                "linux-bwrap glob root `{}` has no existing ancestor",
                path.display()
            )));
        }
    }
    Ok(current)
}

/// `root` itself and everything beneath it, each path once.
pub(super) fn walk_paths(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), OrbitError> {
    out.push(root.to_path_buf());
    walk_children(root, out)
}

/// Every path beneath `root` (not `root`), each once: a directory is pushed
/// by its parent's listing, never again when it is descended into.
fn walk_children(root: &Path, out: &mut Vec<PathBuf>) -> Result<(), OrbitError> {
    if !root.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(root).map_err(|error| {
        OrbitError::Execution(format!(
            "read linux-bwrap glob root `{}`: {error}",
            root.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            OrbitError::Execution(format!("read linux-bwrap glob entry: {error}"))
        })?;
        let path = entry.path();
        out.push(path.clone());
        if entry
            .file_type()
            .map_err(|error| {
                OrbitError::Execution(format!("inspect `{}`: {error}", path.display()))
            })?
            .is_dir()
        {
            walk_children(&path, out)?;
        }
    }
    Ok(())
}

pub(super) fn canonical_existing(path: &Path, label: &str) -> Result<PathBuf, OrbitError> {
    path.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "{label} `{}` must exist and resolve canonically: {error}",
            path.display()
        ))
    })
}
