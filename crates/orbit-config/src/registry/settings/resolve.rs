use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::{
    Crew, resolve_crew, validate_machine_id, validate_machine_name, validate_stored_task_prefix,
};
use orbit_types::workflow::automation::members::MaterialField;
use orbit_types::workflow::{ConventionTemplate, PrBodyTemplateSource, Provider, TemplateSurface};

use super::super::{
    CONSTELLATION_DEFAULT_PROVIDER_ENV, ConfigAllowedValues, DEFAULT_WORKFLOW_CREW,
    LEGACY_DEFAULT_WORKFLOW_CREW, describe,
};
use crate::memory_limit::MemoryLimit;

/// Add the physical layer to value-admission errors while preserving other
/// error kinds for callers that distinguish I/O and store failures.
pub(super) fn resolve_at<T>(
    result: Result<T, OrbitError>,
    config_path: &Path,
) -> Result<T, OrbitError> {
    result.map_err(|error| match error {
        OrbitError::InvalidInput(message) => OrbitError::InvalidInput(format!(
            "{}: {message}",
            redact_home_dir(&config_path.display().to_string())
        )),
        other => other,
    })
}

pub(super) fn resolve_reclaim(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    // Cargo output, and the build directories and base checkouts agents leave
    // under run scratch [ORB-15613].
    let patterns = raw.unwrap_or_else(|| {
        ["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]
            .map(String::from)
            .to_vec()
    });
    for pattern in &patterns {
        orbit_common::fs::path_glob::RelativePathGlob::new(pattern)
            .map_err(|error| OrbitError::InvalidInput(format!("worktree.reclaim: {error}")))?;
    }
    Ok(patterns)
}

/// `workflow.unlocked_context_paths`: unset is Markdown anywhere in the
/// workspace; `[]` locks every declared entry.
pub(super) fn resolve_unlocked_context_paths(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    let patterns = raw.unwrap_or_else(|| vec!["**/*.md".to_string()]);
    for pattern in &patterns {
        orbit_common::fs::path_glob::RelativePathGlob::new(pattern).map_err(|error| {
            OrbitError::InvalidInput(format!("workflow.unlocked_context_paths: {error}"))
        })?;
    }
    Ok(patterns)
}

/// `workflow.light_delivery_paths`: unset is Orbit's documentation and CI
/// default; `[]` lets no candidate keep a light-delivery tag [ORB-16855].
pub(super) fn resolve_light_delivery_paths(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    let patterns = raw.unwrap_or_else(|| {
        orbit_types::task::DEFAULT_LIGHT_DELIVERY_PATHS
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect()
    });
    orbit_common::fs::path_filter::PathFilter::new(
        orbit_types::task::LIGHT_DELIVERY_PATHS_KEY,
        &patterns,
    )?;
    Ok(patterns)
}

/// `workflow.test_paths` default: test directories and test-file naming
/// conventions shared across languages, not one toolchain's layout.
const DEFAULT_TEST_PATHS: &[&str] = &[
    "**/test/**",
    "**/tests/**",
    "**/__tests__/**",
    "**/spec/**",
    "**/testdata/**",
    "**/*_test.*",
    "**/*.test.*",
    "**/test_*.*",
    "**/*_spec.*",
    "**/*.spec.*",
];

/// `workflow.non_code_paths` default: prose, configuration, data and media.
const DEFAULT_NON_CODE_PATHS: &[&str] = &[
    "docs/**",
    "**/*.md",
    "**/*.mdx",
    "**/*.rst",
    "**/*.txt",
    "**/*.json",
    "**/*.yaml",
    "**/*.yml",
    "**/*.toml",
    "**/*.lock",
    "**/*.csv",
    "**/*.golden",
    "**/*.png",
    "**/*.jpg",
    "**/*.jpeg",
    "**/*.gif",
    "**/*.webp",
    "**/*.svg",
    "**/*.ico",
    "**/LICENSE*",
];

fn resolve_path_filter_default(
    key: &str,
    raw: Option<Vec<String>>,
    default: &[&str],
) -> Result<Vec<String>, OrbitError> {
    let patterns = raw.unwrap_or_else(|| {
        default
            .iter()
            .map(|pattern| (*pattern).to_string())
            .collect()
    });
    orbit_common::fs::path_filter::PathFilter::new(key, &patterns)?;
    Ok(patterns)
}

/// `workflow.test_paths`: unset is [`DEFAULT_TEST_PATHS`]; `[]` counts no
/// path as a test.
pub(super) fn resolve_test_paths(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    resolve_path_filter_default("workflow.test_paths", raw, DEFAULT_TEST_PATHS)
}

/// `workflow.non_code_paths`: unset is [`DEFAULT_NON_CODE_PATHS`]; `[]`
/// counts every path that is not a test as code.
pub(super) fn resolve_non_code_paths(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    resolve_path_filter_default("workflow.non_code_paths", raw, DEFAULT_NON_CODE_PATHS)
}

/// `workflow.non_reviewable_paths`: unset and `[]` exclude nothing. The
/// default is empty so Orbit does not hard-code a toolchain's generated
/// files; docs and config stay reviewable until a workspace lists them.
pub(super) fn resolve_non_reviewable_paths(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    resolve_path_filter_default("workflow.non_reviewable_paths", raw, &[])
}

/// `workflow.unlocked_path_validation_commands`: blank entries are dropped,
/// so a list of only blanks keeps full validation like an unset key.
pub(super) fn resolve_unlocked_path_validation_commands(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    Ok(raw
        .unwrap_or_default()
        .into_iter()
        .map(|command| command.trim().to_string())
        .filter(|command| !command.is_empty())
        .collect())
}

/// `workflow.generated_paths`: each key a relative path glob, each value a
/// non-empty command. Unset regenerates nothing.
pub(super) fn resolve_generated_paths(
    raw: Option<BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let mut resolved = BTreeMap::new();
    for (pattern, command) in raw.unwrap_or_default() {
        orbit_common::fs::path_glob::RelativePathGlob::new(&pattern).map_err(|error| {
            OrbitError::InvalidInput(format!("workflow.generated_paths: {error}"))
        })?;
        let command = command.trim();
        if command.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "workflow.generated_paths: '{pattern}' names an empty command"
            )));
        }
        resolved.insert(pattern, command.to_string());
    }
    Ok(resolved)
}

/// Built-in `workflow.purpose_crews.final_recovery`: the strongest reasoning crews, so
/// the last automated look at a failed task is the most capable one.
pub(crate) const DEFAULT_FINAL_RECOVERY_CREWS: &[&str] = &["sol:100", "opus:20"];

/// Trim forge logins, drop blanks and duplicates, and keep a stable order.
/// Forge logins are case-insensitive, so they are compared lowercased.
pub(super) fn normalize_logins(raw: Vec<String>) -> Vec<String> {
    let mut logins = raw
        .iter()
        .map(|login| login.trim().to_ascii_lowercase())
        .filter(|login| !login.is_empty())
        .collect::<Vec<_>>();
    logins.sort();
    logins.dedup();
    logins
}

/// Default `retention.audit_days` and `retention.runs_days`.
pub(super) const DEFAULT_RETENTION_DAYS: u32 = 60;

pub(super) fn resolve_retention_days(
    raw: Option<u32>,
    key: &str,
    default: u32,
) -> Result<u32, OrbitError> {
    let (min, _) = declared_integer_bounds(key)?;
    match raw {
        Some(value) if key == "retention.runs_days" && u64::from(value) < min => {
            // Older releases recommended 14 days. Keep those persisted
            // files loadable so operators can repair them through config set.
            // ConfigStore refuses new writes below the floor separately.
            let effective_days = u32::try_from(min).unwrap_or(u32::MAX);
            tracing::warn!(
                key,
                stored_days = value,
                effective_days,
                "stored run retention is below the supported minimum; using {effective_days} days; \
                 persist the correction with orbit config set retention.runs_days {effective_days} \
                 (add --global for the global config)"
            );
            Ok(effective_days)
        }
        _ => resolve_bounded_range(raw, default, key),
    }
}

/// Default `machine.worker_memory_high`: throttle one run well before it can
/// crowd out the host (2026-09-23 OOM outage, ORB-12903).
pub(super) const DEFAULT_WORKER_MEMORY_HIGH: MemoryLimit = MemoryLimit::Percent(40);
/// Default `machine.worker_memory_max`: one runaway run keeps at most half of
/// physical RAM, leaving the rest for the host and sibling runs.
pub(super) const DEFAULT_WORKER_MEMORY_MAX: MemoryLimit = MemoryLimit::Percent(50);
const DEFAULT_WORKER_TASKS_MAX: u32 = 4096;

/// Admit a systemd memory size through [`MemoryLimit::parse`].
///
/// The value later becomes one `systemd-run --property=` argument, so
/// anything outside the grammar is refused here instead of failing every
/// worker launch.
pub(super) fn resolve_memory_limit(
    raw: Option<String>,
    default: MemoryLimit,
    key: &str,
) -> Result<MemoryLimit, OrbitError> {
    let Some(value) = raw else {
        return Ok(default);
    };
    MemoryLimit::parse(&value).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "{key} has invalid value '{}'; expected a size such as 8G or 512M, \
             a percentage of physical memory such as 50%, or infinity",
            value.trim()
        ))
    })
}

pub(super) fn resolve_worker_tasks_max(raw: Option<u32>) -> Result<u32, OrbitError> {
    resolve_bounded_range(raw, DEFAULT_WORKER_TASKS_MAX, "machine.worker_tasks_max")
}

/// Admit `machine.labels`: opaque capability labels, normalized the way a
/// task's `requires_labels` are, so the two compare exactly.
pub(super) fn resolve_machine_labels(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    orbit_types::task::normalize_host_labels(raw.unwrap_or_default(), "machine.labels")
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

/// Default `workflow.mid_run_steering.max_rounds`.
const DEFAULT_MID_RUN_STEERING_MAX_ROUNDS: u8 = 2;

/// Each steering round re-dispatches an agent step, so the key's declared
/// maximum bounds how many a run may spend before it holds instead.
pub(super) fn resolve_mid_run_steering_max_rounds(raw: Option<u8>) -> Result<u8, OrbitError> {
    resolve_bounded_range(
        raw,
        DEFAULT_MID_RUN_STEERING_MAX_ROUNDS,
        "workflow.mid_run_steering.max_rounds",
    )
}

pub(super) fn resolve_machine_id(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.id")? else {
        return Ok(None);
    };
    validate_machine_id(&value)
        .map_err(|error| OrbitError::InvalidInput(format!("machine.id is invalid: {error}")))?;
    Ok(Some(value))
}

pub(super) fn resolve_machine_name(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.name")? else {
        return Ok(None);
    };
    validate_machine_name(&value)
        .map_err(|error| OrbitError::InvalidInput(format!("machine.name is invalid: {error}")))?;
    Ok(Some(value))
}

pub(super) fn resolve_task_prefix(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(value) = resolve_optional_non_empty(raw, "machine.task_prefix")? else {
        return Ok(None);
    };
    validate_stored_task_prefix(&value).map_err(|error| {
        OrbitError::InvalidInput(format!("machine.task_prefix is invalid: {error}"))
    })?;
    Ok(Some(value))
}

/// One of the key's declared [`ConfigAllowedValues::OneOf`] choices,
/// `default` when unset.
pub(super) fn resolve_choice(
    raw: Option<String>,
    default: &str,
    key: &str,
) -> Result<String, OrbitError> {
    let choices = declared_choices(key)?;
    let value = raw.as_deref().unwrap_or(default).trim();
    if choices.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value '{value}'; expected one of: {}",
            choices.join(", ")
        )))
    }
}

pub(super) fn resolve_optional_choice(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    raw.map(|value| resolve_choice(Some(value), "", key))
        .transpose()
}

pub(super) fn resolve_non_empty(
    raw: Option<String>,
    default: &str,
    key: &str,
) -> Result<String, OrbitError> {
    let value = raw.as_deref().unwrap_or(default).trim();
    if value.is_empty() {
        Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
    } else {
        Ok(value.to_string())
    }
}

/// The default material set when unset; an explicit list must name a field.
pub(super) fn resolve_material_fields(
    raw: Option<Vec<MaterialField>>,
) -> Result<Vec<MaterialField>, OrbitError> {
    match raw {
        None => Ok(MaterialField::DEFAULT.to_vec()),
        Some(fields) if fields.is_empty() => Err(OrbitError::InvalidInput(
            "workflow.task_pilot_freshness.material_fields must name at least one field"
                .to_string(),
        )),
        Some(fields) => Ok(fields),
    }
}

pub(super) fn resolve_distributed_completion(raw: Option<String>) -> Result<String, OrbitError> {
    resolve_choice(raw, "review", "workflow.distributed_completion")
}

pub(super) fn resolve_optional_non_empty(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    raw.map(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
        } else {
            Ok(trimmed.to_string())
        }
    })
    .transpose()
}

pub(super) fn resolve_default_crew(
    configured: Option<String>,
    crews: &BTreeMap<String, Crew>,
    env_default: Option<&str>,
    require_selection: bool,
) -> Result<Option<String>, OrbitError> {
    let selected = if let Some(configured) = configured.filter(|value| !value.trim().is_empty()) {
        Some(configured)
    } else if let Some(raw_env) = env_default.filter(|value| !value.trim().is_empty()) {
        let provider = Provider::parse(raw_env).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{CONSTELLATION_DEFAULT_PROVIDER_ENV} has invalid value: {error}"
            ))
        })?;
        let preferred = match provider.as_str() {
            "claude" => "opus",
            "codex" => "sol",
            "antigravity" => "gemini-flash",
            "muse" => "muse-spark",
            provider => provider,
        };
        Some(if crews.contains_key(preferred) {
            preferred.to_string()
        } else {
            provider.as_str().to_string()
        })
    } else {
        None
    };
    if let Some(selected) = selected {
        resolve_crew(&selected, crews)?;
        return Ok(Some(selected));
    }
    if crews.contains_key(DEFAULT_WORKFLOW_CREW) {
        return Ok(Some(DEFAULT_WORKFLOW_CREW.to_string()));
    }
    if crews.contains_key(LEGACY_DEFAULT_WORKFLOW_CREW) {
        return Ok(Some(LEGACY_DEFAULT_WORKFLOW_CREW.to_string()));
    }
    if crews.is_empty() || !require_selection {
        return Ok(None);
    }
    Err(OrbitError::InvalidInput(format!(
        "[workflow].default_crew must be set when defining [crews.*]; choose one of: {}",
        crews.keys().cloned().collect::<Vec<_>>().join(", ")
    )))
}

pub(super) fn default_pass_list() -> Vec<String> {
    #[allow(unused_mut)]
    let mut vars = vec!["HOME", "PATH", "CODEX_HOME", "TMPDIR", "USER"];
    #[cfg(target_os = "macos")]
    vars.push("__CF_USER_TEXT_ENCODING");
    vars.into_iter().map(ToString::to_string).collect()
}

pub(super) fn normalize_pass_list(pass: Vec<String>) -> Result<Vec<String>, OrbitError> {
    let mut normalized = BTreeSet::new();
    for entry in pass {
        let value = entry.trim();
        let mut chars = value.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
        if !valid {
            return Err(OrbitError::InvalidInput(format!(
                "execution.env.pass contains invalid variable name '{value}'"
            )));
        }
        normalized.insert(value.to_string());
    }
    Ok(normalized.into_iter().collect())
}

pub(crate) fn resolve_percent(raw: Option<u8>, default: u8, key: &str) -> Result<u8, OrbitError> {
    resolve_bounded_range(raw, default, key)
}

/// A bounded whole-number setting: `default` when unset, else a value within
/// the key's declared [`ConfigAllowedValues::Integer`] bounds, so what
/// `orbit config keys` shows is what load accepts [ORB-22809].
pub(super) fn resolve_bounded_range<T>(
    raw: Option<T>,
    default: T,
    key: &str,
) -> Result<T, OrbitError>
where
    T: Copy + Into<u64> + std::fmt::Display,
{
    let (min, max) = declared_integer_bounds(key)?;
    let value = raw.unwrap_or(default);
    if value.into() >= min && max.is_none_or(|max| value.into() <= max) {
        Ok(value)
    } else {
        Err(OrbitError::InvalidInput(format!(
            "{key} has invalid value {value}; expected {}",
            expected_range(min, max)
        )))
    }
}

/// The declared integer bounds of registry key `key`.
fn declared_integer_bounds(key: &str) -> Result<(u64, Option<u64>), OrbitError> {
    match describe(key).map(|descriptor| descriptor.allowed) {
        Some(ConfigAllowedValues::Integer { min, max, .. }) => Ok((min, max)),
        _ => Err(undeclared(key, "an integer range")),
    }
}

/// The declared choices of registry key `key`.
fn declared_choices(key: &str) -> Result<&'static [&'static str], OrbitError> {
    describe(key)
        .and_then(|descriptor| descriptor.allowed.choices())
        .ok_or_else(|| undeclared(key, "a choice list"))
}

/// A resolver asked for allowed values its registry row does not declare: a
/// registry defect, reported on every load so no build ships it.
fn undeclared(key: &str, shape: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "{key}: the config registry declares no {shape} for this key; report this Orbit defect"
    ))
}

fn expected_range<T: std::fmt::Display>(min: T, max: Option<T>) -> String {
    match max {
        Some(max) => format!("{min}..={max}"),
        None => format!("at least {min}"),
    }
}

/// Admit output locators without allowing them to replace execution identity.
pub(super) fn resolve_build_output_env(
    raw: Option<Vec<String>>,
) -> Result<Vec<String>, OrbitError> {
    let mut names = BTreeSet::new();
    for raw in raw.unwrap_or_default() {
        let name = raw.trim();
        let mut chars = name.chars();
        let valid = chars
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
            && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric());
        let identity = name.to_ascii_uppercase();
        let reserved = identity.starts_with("ORBIT_")
            || matches!(
                identity.as_str(),
                "PATH"
                    | "HOME"
                    | "USER"
                    | "LOGNAME"
                    | "SHELL"
                    | "TMPDIR"
                    | "TMP"
                    | "TEMP"
                    | "PWD"
                    | "OLDPWD"
            );
        if !valid || reserved {
            return Err(OrbitError::InvalidInput(format!(
                "workflow.validation_env.build_output_env contains invalid or reserved output variable '{name}'"
            )));
        }
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

fn convention_error(error: orbit_types::workflow::ConventionTemplateError) -> OrbitError {
    OrbitError::InvalidInput(error.to_string())
}

/// Admit an optional `{{ }}` text template (`pr.title_template`,
/// `commit.body_template`), refusing an unknown placeholder by name.
pub(super) fn resolve_text_template(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(raw) = resolve_optional_non_empty_raw(raw, key)? else {
        return Ok(None);
    };
    ConventionTemplate::parse(key, &raw, TemplateSurface::Text).map_err(convention_error)?;
    Ok(Some(raw))
}

/// Admit `commit.subject_template`, which must carry `[{{task_id}}]`.
pub(super) fn resolve_commit_subject_template(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(raw) = resolve_optional_non_empty(raw, key)? else {
        return Ok(None);
    };
    ConventionTemplate::parse_commit_subject(key, &raw).map_err(convention_error)?;
    Ok(Some(raw))
}

/// Admit `branch.name_template`, which must carry `{{task_id}}` and
/// `{{hash}}`.
pub(super) fn resolve_branch_template(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(raw) = resolve_optional_non_empty(raw, key)? else {
        return Ok(None);
    };
    ConventionTemplate::parse_branch(key, &raw).map_err(convention_error)?;
    Ok(Some(raw))
}

/// Admit `pr.body_template`: `"repo"` or a repository-relative path.
pub(super) fn resolve_pr_body_template(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    let Some(raw) = resolve_optional_non_empty(raw, key)? else {
        return Ok(None);
    };
    PrBodyTemplateSource::parse(key, &raw).map_err(convention_error)?;
    Ok(Some(raw))
}

/// Trimmed, non-empty entries in first-seen order without duplicates.
/// `machine.mcp.toolsets`: every entry names a toolset (or `all`). The
/// retired `agent-invoke` entry is ignored with a warning so hosts that
/// opted in before its removal keep resolving.
pub(super) fn resolve_mcp_toolsets(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    let names = raw.unwrap_or_default();
    orbit_types::tool::McpToolsetSelection::from_names(names.iter().map(String::as_str))
        .map_err(|error| OrbitError::InvalidInput(format!("machine.mcp.toolsets: {error}")))?;
    let mut kept = Vec::new();
    for name in &names {
        let trimmed = name.trim();
        if orbit_types::tool::McpToolset::is_retired_toolset_name(trimmed) {
            tracing::warn!(
                key = "machine.mcp.toolsets",
                value = trimmed,
                note = orbit_types::tool::McpToolset::RETIRED_AGENT_INVOKE_NOTE,
                "machine.mcp.toolsets entry 'agent-invoke' was removed and is ignored; remove it from config.toml",
            );
            continue;
        }
        kept.push(trimmed.to_string());
    }
    Ok(kept)
}

/// `machine.mcp.role`: unset, or one role name.
pub(super) fn resolve_mcp_role(raw: Option<String>) -> Result<Option<String>, OrbitError> {
    let Some(raw) = resolve_optional_non_empty(raw, "machine.mcp.role")? else {
        return Ok(None);
    };
    let role = raw
        .parse::<orbit_types::tool::McpRole>()
        .map_err(|error| OrbitError::InvalidInput(format!("machine.mcp.role: {error}")))?;
    Ok(Some(role.as_str().to_string()))
}

pub(super) fn resolve_name_list(
    raw: Option<Vec<String>>,
    key: &str,
) -> Result<Vec<String>, OrbitError> {
    let mut names: Vec<String> = Vec::new();
    for name in raw.unwrap_or_default() {
        let name = name.trim();
        if name.is_empty() {
            return Err(OrbitError::InvalidInput(format!(
                "{key} must not contain an empty entry"
            )));
        }
        // Labels and reviewers reach `gh` as option values.
        if name.starts_with('-') {
            return Err(OrbitError::InvalidInput(format!(
                "{key} entry '{name}' must not start with '-'"
            )));
        }
        if !names.iter().any(|known| known.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

/// Tag matching is exact, including case: unlike static forge names, keep
/// differently cased entries so neither configured tag is lost.
pub(super) fn resolve_task_tag_labels(raw: Option<Vec<String>>) -> Result<Vec<String>, OrbitError> {
    let mut labels = BTreeSet::new();
    for label in raw.unwrap_or_default() {
        labels.extend(resolve_name_list(Some(vec![label]), "pr.task_tag_labels")?);
    }
    Ok(labels.into_iter().collect())
}

/// Only canonical task types may map to non-empty, option-safe label names.
pub(super) fn resolve_task_type_labels(
    raw: Option<BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let mut labels = BTreeMap::new();
    for (task_type, label) in raw.unwrap_or_default() {
        task_type
            .parse::<orbit_types::task::TaskType>()
            .map_err(|error| OrbitError::InvalidInput(format!("pr.task_type_labels: {error}")))?;
        let key = format!("pr.task_type_labels.{task_type}");
        let label = resolve_name_list(Some(vec![label]), &key)?;
        labels.insert(task_type, label[0].clone());
    }
    Ok(labels)
}

/// A text template keeps its whitespace: a body template may begin or end
/// with blank lines on purpose. Only an all-blank value is refused.
fn resolve_optional_non_empty_raw(
    raw: Option<String>,
    key: &str,
) -> Result<Option<String>, OrbitError> {
    match raw {
        Some(value) if value.trim().is_empty() => {
            Err(OrbitError::InvalidInput(format!("{key} must not be empty")))
        }
        other => Ok(other),
    }
}
