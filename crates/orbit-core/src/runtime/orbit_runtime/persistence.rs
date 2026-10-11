//! Roots, persistence readiness, and audit stores.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AuditEventInsertParams, AuditInvocationFields, V2AuditEventFilter, V2AuditEventRow,
};
use orbit_store::{Store, workspace_id_for_orbit_dir};
use orbit_types::record::{Audit, OrbitEvent};
use orbit_types::workflow::ShipMode;
use serde_json::Value;

use super::OrbitRuntime;
use crate::runtime::builder;
use crate::runtime::config_path::validated_runtime_config_path;
use crate::runtime::workspace::binding::WorkspaceRuntimeBinding;

impl OrbitRuntime {
    /// Returns at most `limit` recent in-process session events, newest first.
    /// The log retains only the newest [`event_bus::SESSION_EVENT_CAPACITY`](crate::runtime::event_bus::SESSION_EVENT_CAPACITY) events;
    /// session IDs remain stable when older events are discarded. Only returned events are cloned.
    /// The log is empty at startup and discarded on exit. For the persistent CLI audit log,
    /// see [`OrbitRuntime::list_audit_events`].
    pub fn list_session_events(&self, limit: usize) -> Result<Vec<Audit>, OrbitError> {
        let audits = self
            .event_log
            .recent(limit)
            .into_iter()
            .map(|(id, event)| orbit_event_to_audit(id, event))
            .collect();
        Ok(audits)
    }

    pub fn shared_root(&self) -> PathBuf {
        self.context.shared_root().to_path_buf()
    }

    pub fn local_root(&self) -> PathBuf {
        self.context.local_root().to_path_buf()
    }

    pub fn data_root(&self) -> PathBuf {
        self.shared_root()
    }

    pub fn global_root(&self) -> PathBuf {
        self.context.global_root().to_path_buf()
    }

    /// Higher-level workspace metadata used to construct this runtime, when
    /// the caller supplied an authoritative binding.
    pub fn workspace_runtime_binding(&self) -> Option<&WorkspaceRuntimeBinding> {
        self.workspace_binding.as_deref()
    }

    /// Ship mode an unattended drain delivers in.
    ///
    /// The bound workspace's mode, or [`ShipMode::Local`] when this runtime
    /// has no workspace binding. Automatic admission, readiness and
    /// `orbit doctor` share this so they cannot disagree with the drain about
    /// a local-only route [ORB-14168].
    pub fn automatic_delivery_ship_mode(&self) -> ShipMode {
        self.workspace_runtime_binding()
            .map_or(ShipMode::Local, |binding| binding.ship_mode)
    }

    /// Short label naming this runtime's workspace in a refusal: the
    /// checkout directory's name, never an internal id.
    pub(crate) fn workspace_label(&self) -> String {
        let repo_root = self
            .workspace_binding
            .as_deref()
            .map_or(self.paths().repo_root.as_path(), |binding| {
                binding.repo_root.as_path()
            });
        repo_root.file_name().map_or_else(
            || repo_root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Returns the effective `config.toml` path.
    ///
    /// Workspace config replaces global if present; a genuinely missing
    /// workspace config falls back to global. Rejected workspace entries and
    /// inspection failures remain visible to the caller instead of silently
    /// changing precedence. This pathname records selection only; readers must
    /// still open the file through a race-safe boundary.
    pub fn config_path(&self) -> Result<PathBuf, OrbitError> {
        validated_runtime_config_path(self)
    }

    pub fn persistence_config_json(&self) -> Value {
        self.context.persistence().as_json_value()
    }

    pub fn automation_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::AutomationStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.automation))
    }

    /// Before-PR review ledgers, certificates, and landings [ORB-11333].
    pub fn review_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::ReviewStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.review))
    }

    pub fn sqlite_store(&self) -> Result<Store, OrbitError> {
        Ok(self.context.stores().host.sqlite.clone())
    }

    /// Persist dispatch telemetry without widening a foreign-generation
    /// reader's operational state access or running database migrations.
    pub(crate) fn record_tool_dispatch_audit(
        &self,
        params: &AuditEventInsertParams,
        invocation: AuditInvocationFields<'_>,
    ) -> Result<(), OrbitError> {
        if self.is_write_free() {
            Store::append_audit_event_at_path(
                &self.context.persistence().audit_db,
                params,
                invocation,
            )
        } else {
            self.context
                .stores()
                .host
                .sqlite
                .insert_audit_event_record_with_invocation(params, invocation)
        }
    }

    /// Probe the configured database path independently of cached runtime
    /// handles. Diagnostics must observe missing or replaced files and must
    /// not repair or migrate them as a side effect.
    pub fn sqlite_store_for_diagnostics(&self) -> Result<Store, OrbitError> {
        Store::open_read_only(&self.context.persistence().audit_db)
    }

    /// Check write readiness at the configured path without recreating or
    /// migrating the database, independently of cached runtime connections.
    pub fn check_sqlite_store_writable(&self) -> Result<(), OrbitError> {
        Store::check_path_writable(&self.context.persistence().audit_db)
    }

    pub fn v2_audit_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::V2AuditStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.v2_audit))
    }

    pub fn ensure_persistence_ready(&self) -> Result<(), OrbitError> {
        orbit_store::compose::ensure_sqlite_store_ready(&self.context.persistence().audit_db)
    }

    /// Identity used by this runtime's workspace-partitioned stores. A
    /// selected explicit root uses its logical workspace binding; a checkout
    /// uses its persisted partition ID, which may differ from the logical ID.
    pub fn workspace_id(&self) -> Result<String, OrbitError> {
        if let Some(id) = builder::selected_explicit_root_workspace_id(
            self.context.global_root(),
            &self.context.paths().orbit_dir,
            self.workspace_runtime_binding(),
        ) {
            return Ok(id.to_owned());
        }
        workspace_id_for_orbit_dir(&self.context.paths().orbit_dir)
    }

    pub fn list_v2_audit_events(
        &self,
        mut filter: V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        if filter.workspace_id.trim().is_empty() {
            filter.workspace_id = self.workspace_id()?;
        }
        self.v2_audit_store()?.list_v2_audit_events(&filter)
    }

    pub fn insert_v2_audit_event(
        &self,
        params: &orbit_store::contracts::V2AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        self.v2_audit_store()?.insert_v2_audit_event(params)
    }
}

fn orbit_event_to_audit(id: i64, event: OrbitEvent) -> Audit {
    let payload = serde_json::to_value(&event).unwrap_or(Value::Null);
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();

    Audit {
        id,
        event_type: event_type.clone(),
        payload,
        message: event_type,
        created_at: Utc::now(),
    }
}
