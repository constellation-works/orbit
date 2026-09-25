//! What a plugin's `spec.definitions` contribute, and the rules they obey
//! (design `docs/design/plugins/1_scope.md` §4.5).
//!
//! Activities and jobs become the `plugin:<ns>` catalog layer, resolved below
//! the workspace so a workspace file of the same name shadows the plugin's.
//! Routines and auto-tasks are seeded as `enabled: false` templates
//! ([`super::seed`]) and carry a provenance header, which is what lets a
//! disabled plugin's schedules be skipped with a warning rather than failing
//! the clock tick.
//!
//! Every refusal here is scoped to one plugin (§4.9): the caller reports it as
//! that plugin's diagnostic and leaves every other plugin untouched.

use std::path::{Path, PathBuf};

use orbit_common::protocol::yaml::{parse_auto_task_yaml, parse_routine_yaml};
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_tools::plugin::LoadedPlugin;
use orbit_types::plugin::plugin_provenance_label;
use orbit_types::workflow::{
    AutoTaskDefinition, JobV2, JobV2Step, JobV2StepBody, RoutineDefinition,
};

/// The comment key a seeded definition carries, so a later pass can tell which
/// plugin wrote it.
///
/// It is a comment rather than a field because `RoutineDefinition` and
/// `AutoTaskDefinition` are `deny_unknown_fields`: an unknown `provenance:`
/// key would make every seeded file fail to load.
pub(crate) const PROVENANCE_COMMENT_KEY: &str = "provenance:";

/// One definition file a plugin ships, parsed.
#[derive(Debug, Clone)]
pub struct PluginDefinition<T> {
    /// Name the plugin's own file declares.
    pub name: String,
    /// The file inside the plugin root.
    pub path: PathBuf,
    pub definition: T,
}

/// Every definition one plugin contributes, after the §4.5 rules held.
#[derive(Debug, Clone, Default)]
pub struct PluginDefinitionSet {
    /// `(activity name, file)` pairs for the catalog layer.
    pub activities: Vec<(String, PathBuf)>,
    pub jobs: Vec<(String, PathBuf)>,
    pub routines: Vec<PluginDefinition<RoutineDefinition>>,
    pub auto_tasks: Vec<PluginDefinition<AutoTaskDefinition>>,
}

/// Parse and check every definition file a plugin ships.
///
/// `shipped_jobs` are the job names this binary ships, which a plugin routine
/// may target beside the plugin's own jobs. The error names the offending file
/// and the rule it broke, because that is the whole of what an operator can
/// act on.
pub fn load_plugin_definitions(
    plugin: &LoadedPlugin,
    shipped_jobs: &[&str],
) -> Result<PluginDefinitionSet, String> {
    let namespace = plugin.namespace();
    let mut set = PluginDefinitionSet::default();

    for path in &plugin.definitions.activities {
        let yaml = read_definition(path)?;
        let asset = load_activity_asset(&yaml)
            .map_err(|error| refusal(path, format!("is not a valid activity asset: {error}")))?;
        set.activities.push((asset.name, path.clone()));
    }
    for path in &plugin.definitions.jobs {
        let yaml = read_definition(path)?;
        let asset = load_job_asset(&yaml)
            .map_err(|error| refusal(path, format!("is not a valid job asset: {error}")))?;
        validate_job_activity_references(
            path,
            &asset.spec,
            namespace,
            set.activities.iter().map(|(name, _)| name.as_str()),
            shipped_activity_names().into_iter(),
        )?;
        set.jobs.push((asset.name, path.clone()));
    }

    for path in &plugin.definitions.routines {
        let yaml = read_definition(path)?;
        let definition = parse_routine_yaml(&yaml)
            .map_err(|error| refusal(path, format!("is not a valid routine: {error}")))?;
        if definition.enabled {
            return Err(refusal(
                path,
                format!(
                    "declares `enabled: true`; a plugin ships schedules switched off and only a \
                     reviewed edit of the seeded copy in `.orbit/routines/` turns one on \
                     (plugin '{namespace}')"
                ),
            ));
        }
        let target = definition.target.job_name().to_string();
        let ships_target = set.jobs.iter().any(|(name, _)| *name == target)
            || shipped_jobs.contains(&target.as_str());
        if !ships_target {
            return Err(refusal(
                path,
                format!(
                    "targets 'job:{target}', which plugin '{namespace}' does not ship and Orbit \
                     does not ship either; a plugin routine may target only its own job or a \
                     shipped default"
                ),
            ));
        }
        set.routines.push(PluginDefinition {
            name: definition.name.clone(),
            path: path.clone(),
            definition,
        });
    }

    for path in &plugin.definitions.auto_tasks {
        let yaml = read_definition(path)?;
        let definition = parse_auto_task_yaml(&yaml)
            .map_err(|error| refusal(path, format!("is not a valid auto-task: {error}")))?;
        if definition.enabled {
            return Err(refusal(
                path,
                format!(
                    "declares `enabled: true`; a plugin ships schedules switched off and only a \
                     reviewed edit of the seeded copy in `.orbit/auto_tasks/` turns one on \
                     (plugin '{namespace}')"
                ),
            ));
        }
        set.auto_tasks.push(PluginDefinition {
            name: definition.name.clone(),
            path: path.clone(),
            definition,
        });
    }

    Ok(set)
}

fn validate_job_activity_references<'a>(
    path: &Path,
    job: &JobV2,
    namespace: &str,
    own_activities: impl Iterator<Item = &'a str>,
    shipped_activities: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    let allowed: std::collections::BTreeSet<&str> =
        own_activities.chain(shipped_activities).collect();
    let mut references = Vec::new();
    if let Some(name) = &job.recovery_activity {
        references.push(name.as_str());
    }
    if let Some(name) = &job.failure_activity {
        references.push(name.as_str());
    }
    for step in &job.steps {
        collect_step_activity_references(step, &mut references)
            .map_err(|message| refusal(path, message))?;
    }
    for name in references {
        if !allowed.contains(name) {
            return Err(refusal(
                path,
                format!(
                    "references activity '{name}', which plugin '{namespace}' does not ship and \
                     Orbit does not ship either; a plugin job may reference only its own \
                     activity or a shipped default"
                ),
            ));
        }
    }
    Ok(())
}

fn collect_step_activity_references<'a>(
    step: &'a JobV2Step,
    references: &mut Vec<&'a str>,
) -> Result<(), String> {
    if let Some(name) = &step.recovery_activity {
        references.push(name.as_str());
    }
    match &step.body {
        JobV2StepBody::TargetRef(reference) => {
            let name = reference.target.strip_prefix("activity:").ok_or_else(|| {
                format!(
                    "step '{}' target '{}' does not use the required `activity:<name>` form",
                    step.id, reference.target
                )
            })?;
            references.push(name);
        }
        JobV2StepBody::Target(target) => {
            if let Some(name) = &target.activity_name {
                references.push(name.as_str());
            }
        }
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_step_activity_references(branch, references)?;
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => {
            collect_step_activity_references(&fan_out.worker, references)?;
        }
        JobV2StepBody::Loop { loop_ } => {
            for nested in &loop_.steps {
                collect_step_activity_references(nested, references)?;
            }
        }
    }
    Ok(())
}

fn read_definition(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| refusal(path, format!("cannot be read: {error}")))
}

fn refusal(path: &Path, message: String) -> String {
    format!("{} {message}", path.display())
}

/// The job names this binary ships, which a plugin routine may target beside
/// the jobs its own plugin ships (§4.5).
pub(crate) fn shipped_job_names() -> Vec<&'static str> {
    crate::runtime::assets::DEFAULT_JOB_FILES
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

/// The activity names this binary ships, which a plugin job may reference
/// beside the activities its own plugin ships (§4.5).
pub(crate) fn shipped_activity_names() -> Vec<&'static str> {
    crate::runtime::assets::DEFAULT_ACTIVITY_FILES
        .iter()
        .map(|(name, _)| *name)
        .collect()
}

/// The seeded file name for one plugin definition: `<ns>-<name>.yaml` (§3).
pub fn seeded_definition_name(namespace: &str, name: &str) -> String {
    format!("{namespace}-{name}")
}

/// The provenance header a seeded definition carries.
pub(crate) fn provenance_header(namespace: &str, version: &str, kind: &str) -> String {
    let label = plugin_provenance_label(namespace, version);
    format!(
        "# Seeded by Orbit from {label}. This {kind} is switched off: review it,\n\
         # then set `enabled: true` to let the clock tick fire it.\n\
         # An upgrade re-seeds this file only while it still matches what the plugin\n\
         # shipped; a customised file is left alone unless you pass `--force`.\n\
         # {PROVENANCE_COMMENT_KEY} {label}\n"
    )
}

/// Read the `plugin:<ns>@<version>` provenance from a seeded definition's
/// header comment. `None` for any file Orbit's plugin seeding did not write.
pub fn read_definition_provenance(path: &Path) -> Option<(String, String)> {
    let raw = std::fs::read_to_string(path).ok()?;
    parse_provenance_header(&raw)
}

/// The header parser, split out so it can be exercised without a file.
pub(crate) fn parse_provenance_header(raw: &str) -> Option<(String, String)> {
    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with('#') {
            // Provenance is a header: once the document starts, a later
            // comment naming a plugin is the author's prose, not a claim.
            break;
        }
        let Some(rest) = line
            .trim_start_matches('#')
            .trim()
            .strip_prefix(PROVENANCE_COMMENT_KEY)
        else {
            continue;
        };
        let Some(label) = rest.trim().strip_prefix("plugin:") else {
            continue;
        };
        let (namespace, version) = label.split_once('@')?;
        if namespace.is_empty() || version.is_empty() {
            return None;
        }
        return Some((namespace.to_string(), version.to_string()));
    }
    None
}
