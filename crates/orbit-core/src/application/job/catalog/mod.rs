#[allow(unused_imports)]
use std::borrow::Cow;
#[allow(unused_imports)]
use std::cell::{OnceCell, RefCell};
#[allow(unused_imports)]
use std::collections::{BTreeSet, HashMap};
#[allow(unused_imports)]
use std::path::{Path, PathBuf};

#[allow(unused_imports)]
use orbit_common::fs::git::{GIT_LOCAL_TIMEOUT, GIT_OUTPUT_LIMIT};
#[allow(unused_imports)]
use orbit_common::process::run_bounded_capped;
#[allow(unused_imports)]
use orbit_common::{NotFoundKind, OrbitError};
#[allow(unused_imports)]
use orbit_engine::activity_job::{
    CatalogDirectory, CatalogDirectoryList, CatalogError, V2JobCatalog, catalog_error_to_orbit,
};
#[allow(unused_imports)]
use orbit_types::workflow::{JobKind, JobRun, JobScheduleState, JobV2, JobV2Step, JobV2StepBody};
#[allow(unused_imports)]
use serde_json::{Value, json};

#[allow(unused_imports)]
use crate::OrbitRuntime;
#[allow(unused_imports)]
use crate::application::managed_assets::{
    ManagedAssetLayout, ManagedAssetReconciliation, reconcile_managed_assets,
};

/// Shippable default workflow assets. The list lives beside the shipped
/// activities in `runtime::assets` so the runtime kernel can answer "is this a
/// job Orbit ships" — a plugin routine may target one — without reaching up
/// into the application layer.
#[allow(unused_imports)]
pub(crate) use crate::runtime::assets::DEFAULT_JOB_FILES;

mod layers;
mod loading;
mod projection;

pub use layers::CatalogReferenceLayer;
#[allow(unused_imports)]
pub(crate) use layers::*;
#[allow(unused_imports)]
pub(crate) use loading::*;
#[allow(unused_imports)]
pub(crate) use projection::*;
pub use projection::{JobCatalogEntry, JobCatalogFilter, job_catalog_to_json_with_last_run};
