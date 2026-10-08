use std::collections::{HashMap, HashSet};
use std::path::{Component, Path};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::policy::PolicyError;
use crate::policy::fs_rules::CompiledFsRules;
use crate::policy::glob::validate_glob_rule;
use crate::resource::validate_resource_name;

pub const DEFAULT_POLICY_NAME: &str = "default";
pub const UNRESTRICTED_FS_PROFILE: &str = "unrestricted";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "denyRead", default, skip_serializing_if = "Vec::is_empty")]
    pub deny_read: Vec<String>,
    #[serde(rename = "denyModify", default, skip_serializing_if = "Vec::is_empty")]
    pub deny_modify: Vec<String>,
    #[serde(
        rename = "fsProfiles",
        default,
        skip_serializing_if = "HashMap::is_empty"
    )]
    pub fs_profiles: HashMap<String, FsProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct FsProfile {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub modify: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFsProfile {
    pub name: String,
    pub read: Vec<String>,
    pub modify: Vec<String>,
}

impl ResolvedFsProfile {
    /// Compile this profile's rules for `operation` so a caller can decide
    /// many paths against one rule set.
    pub fn compile(&self, operation: FsOperation) -> Result<CompiledFsRules, PolicyError> {
        let rules = match operation {
            FsOperation::Read => &self.read,
            FsOperation::Modify => &self.modify,
        };
        CompiledFsRules::compile(
            rules,
            &format!("fsProfile `{}` {}", self.name, operation.as_str()),
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FsOperation {
    Read,
    Modify,
}

impl FsOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Modify => "modify",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsCheckResult {
    pub allowed: bool,
    pub matched_rule: String,
}

impl PolicyDef {
    pub fn validate(&self) -> Result<(), PolicyError> {
        validate_resource_name(&self.name)
            .map_err(|error| PolicyError::Invalid(error.to_string()))?;

        let deny_read = normalize_rule_set(&self.deny_read, "spec.denyRead")?;
        let deny_modify = normalize_rule_set(&self.deny_modify, "spec.denyModify")?;
        reject_exceptions(&deny_read, "spec.denyRead")?;
        validate_modify_exceptions(&deny_modify)?;

        for (profile_name, profile) in &self.fs_profiles {
            if profile_name.trim().is_empty() {
                return Err(PolicyError::Invalid(format!(
                    "policy `{}` has an empty fsProfile name",
                    self.name
                )));
            }

            let read = normalize_rule_set(
                &profile.read,
                &format!("spec.fsProfiles.{profile_name}.read"),
            )?;
            let modify = normalize_rule_set(
                &profile.modify,
                &format!("spec.fsProfiles.{profile_name}.modify"),
            )?;

            for rule in positive_rules(&read) {
                reject_explicit_global_deny(
                    &self.name,
                    profile_name,
                    "read",
                    rule,
                    &deny_read,
                    "denyRead",
                )?;
            }

            for rule in positive_rules(&modify) {
                reject_explicit_global_deny(
                    &self.name,
                    profile_name,
                    "modify",
                    rule,
                    &deny_modify,
                    "denyModify",
                )?;
                reject_explicit_global_deny(
                    &self.name,
                    profile_name,
                    "modify",
                    rule,
                    &deny_read,
                    "denyRead",
                )?;

                if !positive_rules(&read).any(|read_rule| rule_covers_path_rule(read_rule, rule)) {
                    return Err(PolicyError::Invalid(format!(
                        "policy `{}` fsProfile `{}` has modify rule `{}` that is not covered by any read rule",
                        self.name, profile_name, rule
                    )));
                }
            }
        }

        Ok(())
    }

    pub fn merged(global: &Self, workspace: &Self) -> Result<Self, PolicyError> {
        validate_workspace_modify_exceptions(global, workspace)?;

        let mut fs_profiles = global.fs_profiles.clone();
        for (name, profile) in &workspace.fs_profiles {
            fs_profiles.insert(name.clone(), profile.clone());
        }

        let mut deny_read = global.deny_read.clone();
        extend_unique(&mut deny_read, &workspace.deny_read);

        let mut deny_modify = normalize_rule_set(&global.deny_modify, "global spec.denyModify")?;
        let host_len = deny_modify.len();
        let workspace_modify =
            normalize_rule_set(&workspace.deny_modify, "workspace spec.denyModify")?;
        extend_ordered_modify_rules(&mut deny_modify, host_len, &workspace_modify);

        let merged = Self {
            name: workspace.name.clone(),
            description: workspace
                .description
                .clone()
                .or_else(|| global.description.clone()),
            deny_read,
            deny_modify,
            fs_profiles,
            created_at: global.created_at.or(workspace.created_at),
            updated_at: workspace.updated_at.max(global.updated_at),
        };
        merged.validate()?;
        Ok(merged)
    }

    pub fn effective_profile(&self, profile_name: &str) -> Result<ResolvedFsProfile, PolicyError> {
        let base = match self.fs_profiles.get(profile_name) {
            Some(profile) => profile.clone(),
            None if profile_name == UNRESTRICTED_FS_PROFILE => FsProfile {
                read: vec!["./**".to_string()],
                modify: vec!["./**".to_string()],
            },
            None => {
                return Err(PolicyError::Invalid(format!(
                    "policy `{}` does not define fsProfile `{profile_name}`",
                    self.name
                )));
            }
        };

        let mut read = normalize_rule_set(&base.read, &format!("fsProfile `{profile_name}` read"))?;
        let base_modify =
            normalize_rule_set(&base.modify, &format!("fsProfile `{profile_name}` modify"))?;
        let mut modify = base_modify.clone();
        let deny_read = normalize_rule_set(&self.deny_read, "spec.denyRead")?;
        let deny_modify = normalize_rule_set(&self.deny_modify, "spec.denyModify")?;

        read.extend(deny_read.into_iter().map(negate_rule));
        for rule in deny_modify {
            if let Some(exception) = rule.strip_prefix('!') {
                modify.extend(profile_rules_within_exception(&base_modify, exception));
            } else {
                modify.push(negate_rule(rule));
            }
        }

        Ok(ResolvedFsProfile {
            name: profile_name.to_string(),
            read,
            modify,
        })
    }

    pub fn check_path(
        &self,
        profile_name: &str,
        operation: FsOperation,
        path: &str,
    ) -> Result<FsCheckResult, PolicyError> {
        let profile = self.effective_profile(profile_name)?;
        profile.compile(operation)?.evaluate(path)
    }
}

fn extend_unique(target: &mut Vec<String>, extra: &[String]) {
    for value in extra {
        if !target.iter().any(|existing| existing == value) {
            target.push(value.clone());
        }
    }
}

/// Append workspace `denyModify` rules after the host rules.
///
/// `host_len` is the number of leading host rules in `target`. An intervening
/// exception can reopen a deny, and an intervening deny can close an
/// exception, so a repeated rule is deduplicated only within the trailing run
/// of rules with the same polarity.
///
/// A kept workspace exception replays every host rule after the last host
/// exception covering it. Otherwise the exception would reopen a host deny
/// that follows the host exception, which the host policy forbids.
fn extend_ordered_modify_rules(target: &mut Vec<String>, host_len: usize, extra: &[String]) {
    for rule in extra {
        let exception = rule.strip_prefix('!');
        let redundant = target
            .iter()
            .rev()
            .take_while(|existing| existing.starts_with('!') == exception.is_some())
            .any(|existing| existing == rule);
        if redundant {
            continue;
        }
        target.push(rule.clone());
        let Some(exception) = exception else {
            continue;
        };
        let covering = target[..host_len].iter().rposition(|host| {
            host.strip_prefix('!')
                .is_some_and(|host| rule_covers_path_rule(host, exception))
        });
        if let Some(covering) = covering {
            let replay = target[covering + 1..host_len].to_vec();
            target.extend(replay);
        }
    }
}

fn reject_exceptions(rules: &[String], label: &str) -> Result<(), PolicyError> {
    if let Some(rule) = rules.iter().find(|rule| rule.starts_with('!')) {
        return Err(PolicyError::Invalid(format!(
            "{label} rule `{rule}` cannot be an exception"
        )));
    }
    Ok(())
}

fn validate_modify_exceptions(rules: &[String]) -> Result<(), PolicyError> {
    let mut enclosing_denies: Vec<&str> = Vec::new();
    for rule in rules {
        let Some(exception) = rule.strip_prefix('!') else {
            enclosing_denies.push(rule);
            continue;
        };
        if !is_exact_or_subtree_rule(exception) {
            return Err(PolicyError::Invalid(format!(
                "spec.denyModify exception `{rule}` must name an exact path or `<path>/**` subtree"
            )));
        }
        if !enclosing_denies
            .iter()
            .any(|deny| *deny != exception && rule_covers_path_rule(deny, exception))
        {
            return Err(PolicyError::Invalid(format!(
                "spec.denyModify exception `{rule}` must be strictly contained by an earlier denyModify rule"
            )));
        }
    }
    Ok(())
}

fn validate_workspace_modify_exceptions(
    global: &PolicyDef,
    workspace: &PolicyDef,
) -> Result<(), PolicyError> {
    let global_rules = normalize_rule_set(&global.deny_modify, "global spec.denyModify")?;
    let global_has_denies = global_rules.iter().any(|rule| !rule.starts_with('!'));
    let global_exceptions: Vec<&str> = global_rules
        .iter()
        .filter_map(|rule| rule.strip_prefix('!'))
        .collect();
    if !global_has_denies {
        return Ok(());
    }

    let workspace_rules = normalize_rule_set(&workspace.deny_modify, "workspace spec.denyModify")?;
    for exception in workspace_rules
        .iter()
        .filter_map(|rule| rule.strip_prefix('!'))
    {
        if !global_exceptions
            .iter()
            .any(|global| rule_covers_path_rule(global, exception))
        {
            return Err(PolicyError::Invalid(format!(
                "workspace policy `{}` denyModify exception `!{exception}` is outside the host policy exception surface",
                workspace.name
            )));
        }
    }
    Ok(())
}

fn is_exact_or_subtree_rule(rule: &str) -> bool {
    let body = rule.strip_suffix("/**").unwrap_or(rule);
    !body.contains(['*', '?'])
}

/// Intersect a host-policy exception with the selected profile instead of
/// treating the exception itself as new write authority.
///
/// `rule_covers_path_rule` only sees an exact path, `**`, or a literal
/// `<prefix>/**`. A profile negation such as `!**/config.toml` still matches
/// paths inside the exception. Those overlaps are replayed after the
/// exception, scoped to it, so the profile keeps narrowing the grant and
/// paths outside the exception keep the profile's own decision.
fn profile_rules_within_exception(profile_rules: &[String], exception: &str) -> Vec<String> {
    if is_exact_path_rule(exception) {
        return if profile_last_match_allows(profile_rules, exception) {
            vec![exception.to_string()]
        } else {
            Vec::new()
        };
    }

    let Some(prefix) = exception.strip_suffix("/**") else {
        // Validated exceptions are an exact path or `<prefix>/**`. Any other
        // shape grants nothing.
        return Vec::new();
    };

    let last_cover = profile_rules.iter().rposition(|rule| {
        let (_, body) = split_rule(rule);
        rule_covers_path_rule(body, exception)
    });
    let ancestor_allows = last_cover.is_some_and(|index| {
        let (negated, _) = split_rule(&profile_rules[index]);
        !negated
    });

    let mut resolved = Vec::new();
    if ancestor_allows {
        resolved.push(exception.to_string());
    }
    for (index, rule) in profile_rules.iter().enumerate() {
        // The last rule covering the whole subtree settled every earlier
        // rule, including nested ones. Only later rules can carve it back.
        // With no such cover, every overlapping rule still applies.
        if last_cover.is_some_and(|cover| index <= cover) {
            continue;
        }
        let (negated, body) = split_rule(rule);
        if rule_covers_path_rule(exception, body) {
            resolved.push(scoped_rule(negated, body));
            continue;
        }
        match glob_patterns_inside_subtree(body, prefix) {
            Some(patterns) => {
                for pattern in patterns {
                    resolved.push(scoped_rule(negated, &pattern));
                }
            }
            // The negation may overlap, and its intersection could not be
            // written as a glob. Deny the subtree rather than replay a
            // pattern that also matches paths outside the exception.
            None if negated => resolved.push(scoped_rule(true, exception)),
            None => {}
        }
    }
    resolved
}

fn scoped_rule(negated: bool, body: &str) -> String {
    if negated {
        format!("!{body}")
    } else {
        body.to_string()
    }
}

fn is_exact_path_rule(rule: &str) -> bool {
    !rule.contains(['*', '?'])
}

fn profile_last_match_allows(profile_rules: &[String], path: &str) -> bool {
    let Ok(compiled) = CompiledFsRules::compile(profile_rules, "profile modify") else {
        return false;
    };
    compiled.allows(path).unwrap_or(false)
}

/// Patterns that match exactly the paths under `prefix/**` which `glob` also
/// matches. `Some([])` is a proven empty intersection. `None` means the glob
/// uses a `**` that is not its own segment, so the intersection is not a glob
/// this walker can build.
fn glob_patterns_inside_subtree(glob: &str, prefix: &str) -> Option<Vec<String>> {
    if prefix.is_empty() {
        return Some(vec![glob.to_string()]);
    }
    let segments = parse_glob_segments(glob)?;
    let prefix_segments: Vec<&str> = prefix.split('/').filter(|part| !part.is_empty()).collect();
    let mut patterns = Vec::new();
    let mut seen = HashSet::new();
    collect_subtree_patterns(
        &segments,
        &prefix_segments,
        0,
        0,
        prefix,
        &mut seen,
        &mut patterns,
    );
    if crate::policy::glob::match_glob(glob, prefix).unwrap_or(false) {
        push_unique(&mut patterns, prefix.to_string());
    }
    Some(patterns)
}

enum GlobSeg {
    /// A full-segment `**`.
    Any,
    /// A literal segment or one `*` / `?` pattern, never containing `**`.
    Fixed(String),
}

fn parse_glob_segments(glob: &str) -> Option<Vec<GlobSeg>> {
    if glob.is_empty() {
        return Some(Vec::new());
    }
    let mut segments = Vec::new();
    for part in glob.split('/') {
        if part.is_empty() {
            return None;
        }
        if part == "**" {
            segments.push(GlobSeg::Any);
        } else if part.contains("**") {
            return None;
        } else {
            segments.push(GlobSeg::Fixed(part.to_string()));
        }
    }
    Some(segments)
}

fn collect_subtree_patterns(
    glob: &[GlobSeg],
    prefix: &[&str],
    glob_index: usize,
    prefix_index: usize,
    prefix_text: &str,
    seen: &mut HashSet<(usize, usize)>,
    patterns: &mut Vec<String>,
) {
    if !seen.insert((glob_index, prefix_index)) {
        return;
    }
    if prefix_index == prefix.len() {
        let rest = render_glob_segments(&glob[glob_index..]);
        if !rest.is_empty() {
            push_unique(patterns, format!("{prefix_text}/{rest}"));
        }
        return;
    }
    if glob_index >= glob.len() {
        return;
    }
    match &glob[glob_index] {
        GlobSeg::Any => {
            collect_subtree_patterns(
                glob,
                prefix,
                glob_index + 1,
                prefix_index,
                prefix_text,
                seen,
                patterns,
            );
            collect_subtree_patterns(
                glob,
                prefix,
                glob_index,
                prefix_index + 1,
                prefix_text,
                seen,
                patterns,
            );
        }
        GlobSeg::Fixed(text) => {
            if segment_matches(text, prefix[prefix_index]) {
                collect_subtree_patterns(
                    glob,
                    prefix,
                    glob_index + 1,
                    prefix_index + 1,
                    prefix_text,
                    seen,
                    patterns,
                );
            }
        }
    }
}

fn render_glob_segments(segments: &[GlobSeg]) -> String {
    let mut rendered = String::new();
    for segment in segments {
        if !rendered.is_empty() {
            rendered.push('/');
        }
        rendered.push_str(match segment {
            GlobSeg::Any => "**",
            GlobSeg::Fixed(text) => text,
        });
    }
    rendered
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    crate::policy::glob::match_glob(pattern, segment).unwrap_or(false)
}

fn push_unique(patterns: &mut Vec<String>, pattern: String) {
    if !patterns.iter().any(|existing| existing == &pattern) {
        patterns.push(pattern);
    }
}

fn reject_explicit_global_deny(
    policy_name: &str,
    profile_name: &str,
    section: &str,
    rule: &str,
    deny_rules: &[String],
    deny_label: &str,
) -> Result<(), PolicyError> {
    if deny_rules.iter().any(|deny_rule| deny_rule == rule) {
        return Err(PolicyError::Invalid(format!(
            "policy `{}` fsProfile `{}` {} rule `{}` duplicates global {} entry",
            policy_name, profile_name, section, rule, deny_label
        )));
    }
    Ok(())
}

fn rule_covers_path_rule(read_rule: &str, path_rule: &str) -> bool {
    if read_rule == path_rule || read_rule == "**" {
        return true;
    }

    if let Some(prefix) = read_rule.strip_suffix("/**") {
        if prefix.is_empty() {
            return true;
        }

        if path_rule == prefix || path_rule.starts_with(&format!("{prefix}/")) {
            return true;
        }

        if let Some(path_prefix) = path_rule.strip_suffix("/**") {
            return path_prefix == prefix || path_prefix.starts_with(&format!("{prefix}/"));
        }
    }

    false
}

fn positive_rules(rules: &[String]) -> impl Iterator<Item = &str> {
    rules
        .iter()
        .map(String::as_str)
        .filter(|rule| !rule.starts_with('!'))
}

fn normalize_rule_set(rules: &[String], label: &str) -> Result<Vec<String>, PolicyError> {
    rules
        .iter()
        .map(|rule| normalize_rule(rule, label))
        .collect()
}

fn normalize_rule(rule: &str, label: &str) -> Result<String, PolicyError> {
    let trimmed = rule.trim();
    if trimmed.is_empty() {
        return Err(PolicyError::Invalid(format!(
            "{label} contains an empty path rule"
        )));
    }

    let (negated, body) = split_rule(trimmed);
    let mut normalized = body.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    if normalized.is_empty() {
        normalized = ".".to_string();
    }

    if normalized == "~"
        || normalized.starts_with("~/")
        || normalized.starts_with("../")
        || normalized == ".."
    {
        return Err(PolicyError::Invalid(format!(
            "{label} rule `{trimmed}` must stay inside the workspace root"
        )));
    }

    let path = Path::new(&normalized);
    if path.is_absolute() {
        return Err(PolicyError::Invalid(format!(
            "{label} rule `{trimmed}` must stay inside the workspace root"
        )));
    }

    for component in path.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(PolicyError::Invalid(format!(
                    "{label} rule `{trimmed}` must stay inside the workspace root"
                )));
            }
        }
    }

    // [ORB-10009] Rules share the path-side canonical spelling (no `.`
    // segments, no duplicate/trailing separators) so a rule like `src/` or
    // `src/./foo` matches the paths it visibly refers to instead of
    // silently matching nothing.
    let mut normalized = crate::policy::glob::join_normal_components(path);
    if normalized.is_empty() {
        normalized = ".".to_string();
    }

    validate_glob_rule(&normalized).map_err(|error| {
        PolicyError::Invalid(format!(
            "{label} rule `{trimmed}` is not a valid filesystem glob: {error}"
        ))
    })?;

    Ok(if negated {
        negate_rule(normalized)
    } else {
        normalized
    })
}

fn split_rule(rule: &str) -> (bool, &str) {
    rule.strip_prefix('!')
        .map(|rest| (true, rest))
        .unwrap_or((false, rule))
}

fn negate_rule(rule: String) -> String {
    format!("!{rule}")
}
