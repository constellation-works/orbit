/// Shippable default workflow assets. The list lives beside the shipped
/// activities in `runtime::assets` so the runtime kernel can answer "is this a
/// job Orbit ships" — a plugin routine may target one — without reaching up
/// into the application layer.
pub(crate) use crate::runtime::assets::DEFAULT_JOB_FILES;

mod layers;
mod loading;
mod projection;

pub use layers::CatalogReferenceLayer;

pub(crate) use loading::{V2JobExecutionMembership, V2JobNameCache, seed_default_jobs};
pub(in crate::application::job) use loading::{is_default_job_name, shipped_job_spec};

pub use projection::{JobCatalogEntry, JobCatalogFilter, job_catalog_to_json_with_last_run};
