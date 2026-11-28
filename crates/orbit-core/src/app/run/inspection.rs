use super::{Command, Operation};
use crate::OrbitError;
use crate::application::distributed::LeafSettlement;
use crate::application::distributed::{
    FollowerRunObserver, LeafHydrationReport, LeafHydrationRequest,
};
use crate::runtime::host_resource::accuracy::{AccuracySelection, ForecastAccuracy};
use crate::runtime::host_resource::profile::ProfileSet;
use chrono::{DateTime, Utc};
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use std::sync::Arc;

impl<'a> Command<'a, Arc<ProfileSet>> {
    /// Read resource profiles.
    pub fn profiles() -> Self {
        Self::new(
            Operation::command("run", "profiles")
                .target(Some("host"), Some("resource_profiles".into())),
            move |core| core.runtime().resource_profiles(),
        )
    }
}

impl<'a> Command<'a, ForecastAccuracy> {
    /// Read forecast accuracy for a selection.
    pub fn accuracy(selection: AccuracySelection, at: DateTime<Utc>) -> Self {
        Self::new(
            Operation::command("run", "profiles")
                .target(Some("host"), Some("resource_profiles".into())),
            move |core| {
                crate::unit::workspace_auto::forecast_accuracy_selected(
                    core.runtime(),
                    selection,
                    at,
                )
            },
        )
    }
}

impl<'a> Command<'a, Vec<LeafSettlement>> {
    /// Read claim settlements.
    pub fn settlements(since: Option<DateTime<Utc>>, reconcile: bool) -> Self {
        Self::new(
            Operation::command("run", "settlements").target(Some("execution_claim"), None),
            move |core| core.runtime().leaf_settlements(since, reconcile),
        )
    }
}

impl<'a> Command<'a, (serde_json::Value, super::WaitingTasks)> {
    /// Read the backlog admission report and its task statuses.
    pub fn readiness(
        ids: &'a [String],
        concurrency: Option<u32>,
        limit: usize,
        crews: &'a [String],
    ) -> Self {
        Self::new(
            Operation::command("run", "readiness")
                .target(Some("workspace"), Some("auto_readiness".into())),
            move |core| {
                let rt = core.runtime();
                let payload = crate::unit::workspace_auto::workspace_auto_readiness(
                    rt,
                    ids,
                    concurrency,
                    limit,
                    crews,
                )?;
                Ok((payload, super::WaitingTasks::read(rt, std::iter::empty())))
            },
        )
    }
}

impl<'a> Command<'a, LeafHydrationReport> {
    /// Hydrate selected settlement evidence through an operator observer.
    pub fn hydrate<O: FollowerRunObserver>(
        make_observer: impl FnOnce(
            std::path::PathBuf,
            String,
        ) -> Result<(LeafHydrationRequest, O), OrbitError>
        + 'a,
    ) -> Self {
        Self::new(
            Operation::command("run", "settlements-hydrate")
                .target(Some("execution_claim"), None)
                .require_native_operator(
                    "hydrating settlement evidence reads follower run records and requires the \
                 operator; an agent or managed scan reads `orbit run settlements --no-reconcile` \
                 instead",
                ),
            move |core| {
                let rt = core.runtime();
                let root = rt.global_root();
                let caller =
                    match inspect_machine_identity(&root)? {
                        MachineIdentityState::Present(identity) => identity.id,
                        MachineIdentityState::Absent => return Err(OrbitError::InvalidInput(
                            "this machine has no [machine] identity yet; run `orbit init` first"
                                .into(),
                        )),
                    };
                let (request, observer) = make_observer(root, caller)?;
                rt.hydrate_leaf_run_evidence(&request, &observer)
            },
        )
    }
}
