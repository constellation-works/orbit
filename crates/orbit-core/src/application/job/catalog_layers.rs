//! Which catalog layer answered for each `job:` / `activity:` reference.
//!
//! Layering is the part of the plugin standard an operator is most likely to
//! be surprised by (design `docs/design/plugins/1_scope.md` §8): a workspace
//! file silently shadows a plugin's activity of the same name. `orbit run
//! show` therefore names the layer that resolved every reference in a run's
//! job, and names the layers it shadowed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::workflow::{JobV2, JobV2Step, JobV2StepBody};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::catalog::DEFAULT_JOB_FILES;

/// Layer label for the definitions this binary ships.
pub const SHIPPED_LAYER: &str = "shipped";
/// Layer label for the workspace's own `.orbit/resources` definitions.
pub const WORKSPACE_LAYER: &str = "workspace";
/// Layer label for an `ORBIT_ACTIVITY_DIR` / `ORBIT_JOB_DIR` override.
pub const EXPLICIT_LAYER: &str = "explicit";

/// One `job:` or `activity:` reference and the layer that resolved it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogReferenceLayer {
    /// `job:<name>` or `activity:<name>`.
    pub reference: String,
    /// `workspace`, `shipped`, `plugin:<ns>`, or `explicit`.
    pub layer: String,
    /// The file that answered, when the reference resolved.
    pub path: Option<PathBuf>,
    /// Layers that define the same name without supplying the definition —
    /// the plugin a workspace file shadows.
    pub shadows: Vec<String>,
}

impl CatalogReferenceLayer {
    pub fn to_json(&self) -> Value {
        json!({
            "reference": self.reference,
            "layer": self.layer,
            "path": self.path.as_ref().map(|path| path.display().to_string()),
            "shadows": self.shadows,
        })
    }

    /// The operator-facing line: the reference, the layer, and what it shadows.
    pub fn to_line(&self) -> String {
        let shadows = if self.shadows.is_empty() {
            String::new()
        } else {
            format!(" (shadows {})", self.shadows.join(", "))
        };
        format!("{} layer={}{shadows}", self.reference, self.layer)
    }
}

impl OrbitRuntime {
    /// Every catalog reference one job makes, with the layer that resolved it.
    ///
    /// A job that no longer resolves answers with a single unresolved row
    /// rather than an error: this is a reporting surface for a run that has
    /// already happened, and "the job is gone" is the answer to print.
    pub fn catalog_reference_layers(
        &self,
        job_id: &str,
    ) -> Result<Vec<CatalogReferenceLayer>, OrbitError> {
        let plugin_names = self.plugin_catalog_names();
        let Ok(entry) = self.show_job_catalog_entry(job_id) else {
            return Ok(vec![CatalogReferenceLayer {
                reference: format!("job:{job_id}"),
                layer: "unresolved".to_string(),
                path: None,
                shadows: plugin_names.jobs.get(job_id).cloned().unwrap_or_default(),
            }]);
        };

        let mut rows = vec![CatalogReferenceLayer {
            reference: format!("job:{job_id}"),
            layer: self.catalog_layer_of(&entry.path),
            shadows: shadowed_by(
                plugin_names.jobs.get(job_id),
                &self.catalog_layer_of(&entry.path),
            ),
            path: Some(entry.path.clone()),
        }];

        let catalog = self.v2_activity_catalog().ok();
        for name in activity_references(&entry.spec) {
            let path = catalog
                .as_ref()
                .and_then(|catalog| catalog.source(&name))
                .map(Path::to_path_buf);
            let layer = path
                .as_ref()
                .map(|path| self.catalog_layer_of(path))
                .unwrap_or_else(|| "unresolved".to_string());
            rows.push(CatalogReferenceLayer {
                reference: format!("activity:{name}"),
                shadows: shadowed_by(plugin_names.activities.get(&name), &layer),
                layer,
                path,
            });
        }
        Ok(rows)
    }

    /// The layer a resolved catalog file belongs to.
    fn catalog_layer_of(&self, path: &Path) -> String {
        for plugin in self.plugin_load().active() {
            if path.starts_with(&plugin.root) {
                return format!("plugin:{}", plugin.namespace());
            }
        }
        let paths = self.paths();
        if path.starts_with(&paths.activities_dir) || path.starts_with(&paths.jobs_dir) {
            return WORKSPACE_LAYER.to_string();
        }
        if path.starts_with(paths.global_dir.join("resources")) {
            return SHIPPED_LAYER.to_string();
        }
        // A file this binary ships but that no root claims can only come from
        // an explicit catalog override.
        if DEFAULT_JOB_FILES
            .iter()
            .any(|(name, _)| path.file_stem().and_then(|stem| stem.to_str()) == Some(*name))
        {
            return SHIPPED_LAYER.to_string();
        }
        EXPLICIT_LAYER.to_string()
    }

    /// Names each active plugin contributes, so a shadowed layer can be named
    /// even though its definition never entered the catalog.
    fn plugin_catalog_names(&self) -> PluginCatalogNames {
        let mut names = PluginCatalogNames::default();
        for plugin in self.plugin_load().active() {
            let layer = format!("plugin:{}", plugin.namespace());
            for (files, index) in [
                (&plugin.definitions.activities, &mut names.activities),
                (&plugin.definitions.jobs, &mut names.jobs),
            ] {
                for file in files {
                    let Some(name) = asset_name(file) else {
                        continue;
                    };
                    index.entry(name).or_default().push(layer.clone());
                }
            }
        }
        names
    }
}

#[derive(Debug, Default)]
struct PluginCatalogNames {
    activities: BTreeMap<String, Vec<String>>,
    jobs: BTreeMap<String, Vec<String>>,
}

/// The `metadata.name` of a catalog asset, read without a full parse so a
/// reporting surface never fails on a file the catalog already rejected.
fn asset_name(path: &Path) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct Envelope {
        metadata: Metadata,
    }
    #[derive(serde::Deserialize)]
    struct Metadata {
        name: String,
    }
    let raw = std::fs::read_to_string(path).ok()?;
    serde_yaml::from_str::<Envelope>(&raw)
        .ok()
        .map(|envelope| envelope.metadata.name)
}

fn shadowed_by(layers: Option<&Vec<String>>, resolved: &str) -> Vec<String> {
    layers
        .map(|layers| {
            layers
                .iter()
                .filter(|layer| *layer != resolved)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Every `activity:<name>` a job references, in step order and deduplicated.
fn activity_references(job: &JobV2) -> Vec<String> {
    let mut names = Vec::new();
    let mut push = |name: Option<&String>| {
        if let Some(name) = name
            && !names.contains(name)
        {
            names.push(name.clone());
        }
    };
    push(job.recovery_activity.as_ref());
    push(job.failure_activity.as_ref());
    let mut collected = Vec::new();
    for step in &job.steps {
        collect_step_references(step, &mut collected);
    }
    for name in collected {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

fn collect_step_references(step: &JobV2Step, names: &mut Vec<String>) {
    if let Some(recovery) = &step.recovery_activity {
        names.push(recovery.clone());
    }
    match &step.body {
        JobV2StepBody::TargetRef(reference) => {
            if let Some(name) = reference
                .target
                .strip_prefix(orbit_types::workflow::activity_job::ACTIVITY_REF_PREFIX)
            {
                names.push(name.to_string());
            }
        }
        JobV2StepBody::Target(target) => {
            if let Some(name) = &target.activity_name {
                names.push(name.clone());
            }
        }
        JobV2StepBody::Parallel { parallel } => {
            for branch in &parallel.branches {
                collect_step_references(branch, names);
            }
        }
        JobV2StepBody::FanOut { fan_out, .. } => collect_step_references(&fan_out.worker, names),
        JobV2StepBody::Loop { loop_ } => {
            for inner in &loop_.steps {
                collect_step_references(inner, names);
            }
        }
    }
}
