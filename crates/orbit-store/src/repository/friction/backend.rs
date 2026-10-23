//! FrictionStoreBackend delegation to the inherent FrictionStore surface.

use crate::contracts::FrictionLandingOutcome;
use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::Task;
use serde_json::Value;

use super::{
    FrictionAddParams, FrictionListFilter, FrictionRehomeOutcome, FrictionRehomeParams,
    FrictionStore, FrictionUpdateParams, StoredFrictionRecord,
};

impl crate::contracts::FrictionStoreBackend for FrictionStore {
    fn add(&self, params: FrictionAddParams) -> Result<StoredFrictionRecord, OrbitError> {
        Self::add(self, params)
    }

    fn add_or_reuse(
        &self,
        dedupe_key: &str,
        params: FrictionAddParams,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::add_or_reuse(self, dedupe_key, params)
    }

    fn list(&self, filter: &FrictionListFilter) -> Result<Vec<StoredFrictionRecord>, OrbitError> {
        Self::list(self, filter)
    }

    fn show(&self, id: &str) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        Self::show(self, id)
    }

    fn foreign_owners_of(&self, id: &str) -> Result<Vec<String>, OrbitError> {
        Self::foreign_owners_of(self, id)
    }

    fn update(
        &self,
        id: &str,
        params: FrictionUpdateParams,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::update(self, id, params)
    }

    fn preflight_rehome(
        &self,
        id: &str,
        params: &FrictionRehomeParams,
        edits: &FrictionUpdateParams,
    ) -> Result<(), OrbitError> {
        Self::preflight_rehome(self, id, params, edits)
    }

    fn rehome(
        &self,
        id: &str,
        params: FrictionRehomeParams,
    ) -> Result<FrictionRehomeOutcome, OrbitError> {
        Self::rehome(self, id, params)
    }

    fn resolve(
        &self,
        id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::resolve(self, id, resolved_at)
    }

    fn resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<StoredFrictionRecord, OrbitError> {
        Self::resolve_by_task(self, id, task_id, resolved_at)
    }

    fn auto_resolve_by_task(
        &self,
        id: &str,
        task_id: &str,
        resolved_at: DateTime<Utc>,
    ) -> Result<Option<StoredFrictionRecord>, OrbitError> {
        Self::auto_resolve_by_task(self, id, task_id, resolved_at)
    }

    fn auto_resolve_by_landing(
        &self,
        id: &str,
        task_id: &str,
        commit: &str,
        pr_number: Option<u64>,
        resolved_at: DateTime<Utc>,
    ) -> Result<FrictionLandingOutcome, OrbitError> {
        Self::auto_resolve_by_landing(self, id, task_id, commit, pr_number, resolved_at)
    }

    fn tags(&self) -> Result<Vec<String>, OrbitError> {
        Self::tags(self)
    }

    fn tag_taxonomy(&self) -> Result<Vec<(String, String)>, OrbitError> {
        Self::tag_taxonomy(self)
    }

    fn stats(&self, tasks: &[Task]) -> Result<Value, OrbitError> {
        Self::stats(self, tasks)
    }
}
