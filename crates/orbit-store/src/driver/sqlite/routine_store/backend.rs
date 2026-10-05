//! RoutineStoreBackend delegation to the inherent Store routine surface.

use std::collections::BTreeMap;

use orbit_common::OrbitError;

use super::{
    RoutineCursor, RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutinePauseRecord,
};
use crate::Store;

impl crate::contracts::RoutineStoreBackend for Store {
    fn routine_cursor(&self, routine_name: &str) -> Result<Option<RoutineCursor>, OrbitError> {
        Self::routine_cursor(self, routine_name)
    }

    fn routine_record_baseline(
        &self,
        routine_name: &str,
        baseline_at: &str,
    ) -> Result<bool, OrbitError> {
        Self::routine_record_baseline(self, routine_name, baseline_at)
    }

    fn routine_record_fire_intent(
        &self,
        intent: &RoutineFireIntentParams,
    ) -> Result<bool, OrbitError> {
        Self::routine_record_fire_intent(self, intent)
    }

    fn routine_mark_fire_dispatched(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        Self::routine_mark_fire_dispatched(self, routine_name, slot, attempt, run_id)
    }

    fn routine_mark_fire_outcome(
        &self,
        routine_name: &str,
        slot: &str,
        attempt: u32,
        state: RoutineFireState,
        detail: Option<&str>,
    ) -> Result<(), OrbitError> {
        Self::routine_mark_fire_outcome(self, routine_name, slot, attempt, state, detail)
    }

    fn routine_latest_fire(
        &self,
        routine_name: &str,
    ) -> Result<Option<RoutineFireRecord>, OrbitError> {
        Self::routine_latest_fire(self, routine_name)
    }

    fn routine_unresolved_fires(&self) -> Result<Vec<RoutineFireRecord>, OrbitError> {
        Self::routine_unresolved_fires(self)
    }

    fn routine_recent_fires(
        &self,
        routine_name: &str,
        limit: usize,
    ) -> Result<Vec<RoutineFireRecord>, OrbitError> {
        Self::routine_recent_fires(self, routine_name, limit)
    }

    fn routine_pause(&self, routine_name: &str, actor: &str) -> Result<bool, OrbitError> {
        Self::routine_pause(self, routine_name, actor)
    }

    fn routine_resume(&self, routine_name: &str) -> Result<bool, OrbitError> {
        Self::routine_resume(self, routine_name)
    }

    fn routine_pauses(&self) -> Result<BTreeMap<String, RoutinePauseRecord>, OrbitError> {
        Self::routine_pauses(self)
    }
}
