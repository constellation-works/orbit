use crate::app::resident;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::{OrbitBuiltinAction, OrbitTaskScope, OrbitToolHost, ReservationOwnerContext};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::Core;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::run_input::managed_run_context_run_id_from_env;

/// Source of the run scope. Only the resident broker constructs the authenticated variant.
pub(crate) enum ToolHostRun {
    InProcess(Option<String>),
    #[cfg(unix)]
    BrokeredResident(String),
}

struct CoreOrbitToolHost {
    core: Core,
    task_scope: OrbitTaskScope,
    brokered_resident: bool,
    /// The calling session's asserted grants, carried so a handler whose
    /// decision depends on *who is calling* can reach them [ORB-11354].
    ///
    /// Session identity and worker binding for operation-specific admission.
    session_context: ToolSessionContext,
}

impl OrbitToolHost for CoreOrbitToolHost {
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
        let runtime = self.core.runtime();
        let (agent, model) =
            runtime.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let mut session = self.session_context.clone();
        runtime.bind_worker_session(&mut session)?;
        super::dispatch::execute(
            &self.core,
            &self.task_scope,
            self.brokered_resident,
            crate::app::task::ToolCaller {
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

    fn allows_resident_drain_status(&self) -> bool {
        let runtime = self.core.runtime();
        self.task_scope
            .run_id
            .as_deref()
            .is_some_and(|run_id| resident::is_live_resident_cycle_run(runtime, run_id))
    }

    /// A routed decision-channel callback runs here, outside the plugin
    /// sandbox, through the same `orbit tool run` dispatch the child would
    /// have used. The caller is the routed callback session the dispatch
    /// resolves; the spawning caller's grants stay out of it, and only its
    /// worker binding is kept, so a claimed run's plugin is still refused as
    /// an agent (design `docs/design/human-decision-channel/2_design.md` §1).
    fn execute_host_routed_callback(&self, tool: &str, input: Value) -> Result<Value, OrbitError> {
        if !orbit_tools::plugin::is_host_routed_callback_tool(tool) {
            return Err(OrbitError::PolicyDenied(format!(
                "'{tool}' is not a host-routed plugin callback"
            )));
        }
        let session = ToolSessionContext {
            workspace_id: self.session_context.workspace_id.clone(),
            worker_invocation: self.session_context.worker_invocation.clone(),
            ..Default::default()
        };
        // The route carries the operation; a family operation is dispatched
        // as its family call, the only name the dispatcher accepts for it.
        let (tool, input) = orbit_types::tool::tool_family_call(tool, input);
        crate::app::tool::exec(
            &self.core,
            crate::app::tool::Command::Run {
                name: tool,
                input,
                agent: None,
                model: None,
                entry_point: crate::adapter::command::ToolEntryPoint::Cli,
                session,
            },
        )?
        .into_value()
    }

    fn record_unknown_input_fields(
        &self,
        tool_name: &str,
        fields: &[String],
        actor: Option<&str>,
    ) -> Result<(), OrbitError> {
        let runtime = self.core.runtime();
        let actor = actor.unwrap_or_else(|| runtime.actor_label());
        for field in fields {
            record_system_audit_event(
                runtime,
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
                    error_message: None,
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
        self.core.runtime().bind_worker_session(&mut session)?;
        super::dispatch::execute(
            &self.core,
            &self.task_scope,
            self.brokered_resident,
            crate::app::task::ToolCaller {
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

pub(crate) fn build_orbit_tool_host(
    core: Core,
    task_id: Option<String>,
    run: ToolHostRun,
    session_context: ToolSessionContext,
) -> Arc<dyn OrbitToolHost> {
    let (run_id, brokered_resident) = match run {
        ToolHostRun::InProcess(run_id) => (
            run_id.or_else(|| managed_run_context_run_id_from_env(core.runtime().env())),
            false,
        ),
        #[cfg(unix)]
        ToolHostRun::BrokeredResident(run_id) => (Some(run_id), true),
    };
    let orbit_root = Some(core.runtime().data_root_path().to_path_buf());
    Arc::new(CoreOrbitToolHost {
        core,
        task_scope: OrbitTaskScope {
            orbit_root,
            task_id,
            run_id,
        },
        brokered_resident,
        session_context,
    })
}
