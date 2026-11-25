//! [`Host`]: [`Stored`] plus this machine as a place work runs — the PR
//! forge, the host signals, the load and resource probes, the admission
//! policy and the execution assets.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orbit_store::skill_store::SkillCatalog;
use orbit_tools::ToolRegistry;

use crate::base::Base;
use crate::context::OrbitExecutionAssets;
use crate::host_resource::HostResourceMonitor;
use crate::host_resource::forecast::HostLoadProbe;
use crate::host_resource::profile::ResourceProfileCache;
use crate::host_signal::HostSignalProbe;
use crate::orbit_runtime::admission_policy::{AdmissionPolicy, CachedPolicy};
use crate::plugin::host::PluginHostLoad;
use crate::stored::Stored;
use crate::task_pr_forge::TaskPrForge;

/// The last tree each run passed full required validation on, by run id.
pub(crate) type ValidatedTrees = Arc<Mutex<BTreeMap<String, orbit_engine::ValidatedTree>>>;
/// When each pull request was last checked for an out-of-run repair, by
/// pull request number.
pub(crate) type PrRepairChecks = Arc<Mutex<BTreeMap<u64, Instant>>>;

/// The layer an operation that starts, validates or lands work runs against.
#[derive(Clone)]
pub struct Host {
    /// The tool registry, the skill catalog and the host plugin load.
    pub(crate) execution: OrbitExecutionAssets,
    /// Host lifecycle signals unattended admission consults before starting
    /// new work [ORB-12968]. The platform probe by default; tests inject one.
    pub(crate) host_signals: Arc<dyn HostSignalProbe>,
    /// Forge that closes a task's Orbit-authored PRs on its terminal
    /// decision. `gh` by default; tests inject a fake.
    pub(crate) task_pr_forge: Arc<dyn TaskPrForge>,
    pub(crate) host_resources: Arc<HostResourceMonitor>,
    /// The host's resource profiles, shared by clones and kept across
    /// admission passes until a delivery finishes.
    pub(crate) resource_profiles: Arc<ResourceProfileCache>,
    /// The host's measured load for the forecast baseline. It keeps only the
    /// previous CPU tick reading that utilization is measured against.
    pub(crate) host_load: Arc<HostLoadProbe>,
    pub(crate) admission_policy: Arc<Mutex<Option<CachedPolicy>>>,
    pub(crate) admission_policy_snapshot: Option<Arc<AdmissionPolicy>>,
    /// The home whose headless browser kit admission reads for `ui` tasks.
    /// `None` is the current user's, read again on every admission pass; a
    /// fixture points it at a home it controls.
    pub(crate) browser_kit_home: Option<PathBuf>,
    /// One shared base-run budget per owner claim settlement. Leaves five
    /// minutes of the routed call's fifteen for observation, cleanup and reply.
    pub(crate) claim_known_red_budget: Duration,
    /// The last tree each run passed full required validation on
    /// [ORB-16784]. Held in this process only, where no agent can write it:
    /// a run resumed by another process validates its next candidate in
    /// full. Shared by clones.
    pub(crate) validated_trees: ValidatedTrees,
    /// When an `--auto-fix` drain last found each pull request it may repair
    /// out of run without a blocker, so a pass does not ask the forge again
    /// before the recheck interval. Held in this process; shared by clones.
    pub(crate) pr_repair_checks: PrRepairChecks,
    // Declared last so the probes stop before the stores below close.
    stored: Stored,
}

impl Host {
    /// A host as a runtime opens it: empty caches, no pinned admission
    /// policy, and the default claim settlement budget.
    pub(crate) fn new(
        stored: Stored,
        execution: OrbitExecutionAssets,
        host_resources: Arc<HostResourceMonitor>,
        host_signals: Arc<dyn HostSignalProbe>,
        task_pr_forge: Arc<dyn TaskPrForge>,
        browser_kit_home: Option<PathBuf>,
    ) -> Self {
        Self {
            execution,
            host_signals,
            task_pr_forge,
            host_resources,
            resource_profiles: Arc::default(),
            host_load: Arc::default(),
            admission_policy: Arc::default(),
            admission_policy_snapshot: None,
            browser_kit_home,
            claim_known_red_budget: Duration::from_secs(600),
            validated_trees: Arc::default(),
            pr_repair_checks: Arc::default(),
            stored,
        }
    }

    /// The layer below.
    pub fn stored(&self) -> &Stored {
        &self.stored
    }

    /// The lowest layer.
    pub fn base(&self) -> &Base {
        self.stored.base()
    }

    pub(crate) fn stored_mut(&mut self) -> &mut Stored {
        &mut self.stored
    }

    pub(crate) fn registry(&self) -> &ToolRegistry {
        self.execution.registry.as_ref()
    }

    pub(crate) fn skill_catalog(&self) -> &SkillCatalog {
        &self.execution.skill_catalog
    }

    pub(crate) fn plugin_load(&self) -> &PluginHostLoad {
        &self.execution.plugins
    }
}
