use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::run_input::managed_run_context_run_id_from_env;

pub(crate) fn build_orbit_tool_host(
    runtime: &OrbitRuntime,
    task_id: Option<String>,
    run_id: Option<String>,
    session_context: ToolSessionContext,
) -> Arc<dyn OrbitToolHost> {
    Arc::new(RuntimeOrbitToolHost {
        runtime: runtime.clone(),
        task_scope: OrbitTaskScope {
            orbit_root: Some(runtime.data_root_path().to_path_buf()),
            task_id,
            run_id: run_id.or_else(trusted_env_run_id),
        },
        session_context,
    })
}

#[derive(Clone)]
struct RuntimeOrbitToolHost {
    runtime: OrbitRuntime,
    task_scope: OrbitTaskScope,
    /// The calling session's asserted grants, carried so a handler whose
    /// decision depends on *who is calling* can reach them [ORB-11354].
    ///
    /// The tool chokepoint resolves capabilities before dispatch, but its
    /// answer is a yes/no it does not pass on. `orbit.agent.invoke` needs the
    /// caller itself, because it records the authorizing operator on a durable
    /// admission and verifies the operation-specific identity and workspace
    /// scope of a remote caller the ordinary registry would otherwise allow.
    session_context: ToolSessionContext,
}

impl OrbitToolHost for RuntimeOrbitToolHost {
    fn prepare_operation_input(
        &self,
        action: OrbitBuiltinAction,
        input: &mut Value,
    ) -> Result<(), OrbitError> {
        use orbit_common::governance::workspace_claim::WorkspaceClaimVerb;
        match action {
            OrbitBuiltinAction::Drain(verb) => super::drain_tools::prepare_input(verb, input),
            OrbitBuiltinAction::WorkspaceClaim(verb) => match verb {
                WorkspaceClaimVerb::Acquire
                | WorkspaceClaimVerb::Release
                | WorkspaceClaimVerb::Show => Ok(()),
            },
            _ => Ok(()),
        }
    }

    fn execute(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
        reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        let (agent, model) = self
            .runtime
            .try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let mut session = self.session_context.clone();
        self.runtime.bind_worker_session(&mut session)?;
        super::dispatch::execute(
            &self.runtime,
            &self.task_scope,
            super::dispatch::ToolCaller {
                session_context: &session,
                agent,
                model,
                reservation_owner,
            },
            action,
            input,
        )
    }

    fn task_scope(&self) -> OrbitTaskScope {
        self.task_scope.clone()
    }

    fn record_unknown_input_fields(
        &self,
        tool_name: &str,
        fields: &[String],
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let actor = actor.unwrap_or_else(|| self.runtime.actor_label());
        for field in fields {
            record_system_audit_event(
                &self.runtime,
                SystemAuditEvent {
                    execution_id_prefix: "audit-tool-input",
                    command: "tool_input",
                    subcommand: Some("unknown_field"),
                    tool_name: Some(tool_name),
                    target_type: "tool",
                    target_id: Some(tool_name),
                    role: actor,
                    status: AuditEventStatus::Success,
                    arguments_json: json!({
                        "tool_name": tool_name,
                        "field": field,
                        "actor": actor,
                    })
                    .to_string(),
                    workspace_id: None,
                    task_id: self.task_scope.task_id.as_deref(),
                    attribution: AuditRunAttribution::Environment {
                        run_id: self.task_scope.run_id.clone(),
                    },
                },
            )?;
        }
        Ok(())
    }

    fn execute_with_trusted_actor(
        &self,
        action: OrbitBuiltinAction,
        input: Value,
        actor_label: String,
        reservation_owner: Option<ReservationOwnerContext>,
    ) -> Result<Value, OrbitError> {
        let mut session = self.session_context.clone();
        self.runtime.bind_worker_session(&mut session)?;
        super::dispatch::execute(
            &self.runtime,
            &self.task_scope,
            super::dispatch::ToolCaller {
                session_context: &session,
                agent: None,
                model: Some(actor_label),
                reservation_owner,
            },
            action,
            input,
        )
    }
}

fn trusted_env_run_id() -> Option<String> {
    managed_run_context_run_id_from_env()
}
