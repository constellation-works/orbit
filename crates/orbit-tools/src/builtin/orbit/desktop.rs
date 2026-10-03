//! Narrow desktop client operations. The application owns authority and mutations.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

type InputField = (&'static str, &'static str, bool, &'static str);

pub enum DesktopTool {
    Read,
    Drain,
    Automation,
    Snapshot,
    Write,
}

impl Tool for DesktopTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        match self {
            Self::Write | Self::Drain | Self::Automation => ToolExecutionKind::Mutating,
            _ => ToolExecutionKind::ReadOnly,
        }
    }
    fn schema(&self) -> ToolSchema {
        let (name, description, fields): (&str, &str, &[InputField]) = match self {
            Self::Read => (
                "orbit.desktop.read",
                "Read a bounded desktop view in one explicit workspace. Task search matches public key/title only; history search uses orbit.search. Run reads require the same operator authority as workflow run observation. This never reconciles or starts execution.",
                &[
                    (
                        "scope",
                        "string",
                        true,
                        "tasks, task, runs, run, drain, routines, auto_tasks or jobs",
                    ),
                    (
                        "id",
                        "string",
                        false,
                        "Public task key or run ID for detail scope",
                    ),
                    (
                        "offset",
                        "integer",
                        false,
                        "Zero-based list offset; default 0",
                    ),
                    (
                        "limit",
                        "integer",
                        false,
                        "Page size, 1 through 50; default 25",
                    ),
                    (
                        "search",
                        "string",
                        false,
                        "Case-insensitive public task key/title substring",
                    ),
                    ("status", "string", false, "Task status or run state filter"),
                    ("priority", "string", false, "Task priority filter"),
                    ("comments_offset", "integer", false, "Comment page offset"),
                    (
                        "history_offset",
                        "integer",
                        false,
                        "Task history page offset",
                    ),
                    (
                        "artifacts_offset",
                        "integer",
                        false,
                        "Artifact metadata page offset",
                    ),
                    ("log_offset", "integer", false, "Run log record offset"),
                ],
            ),
            Self::Drain => (
                "orbit.desktop.drain",
                "Start a bounded auto-drain window or stop admissions and deliver recorded settlements in one workspace. Requires an existing operator session. Stop preserves admitted workers. Start is not retry-safe: after a lost reply inspect drain readiness and runs before another submission. Never retries automatically.",
                &[
                    ("action", "string", true, "start or stop"),
                    (
                        "for_seconds",
                        "integer",
                        false,
                        "Required for start: window length in seconds, 1 through 604800",
                    ),
                    (
                        "concurrency",
                        "integer",
                        false,
                        "Optional start worker limit, positive u32; omitted uses runtime default",
                    ),
                    (
                        "complete",
                        "boolean",
                        false,
                        "Start only: explicitly authorize automatic completion for all tasks admitted by this window; default false keeps review",
                    ),
                    (
                        "claim_token",
                        "string",
                        false,
                        "Workspace claim token when held by another operator",
                    ),
                ],
            ),
            Self::Automation => (
                "orbit.desktop.automation",
                "Operator-only automation actions in one selected workspace. Toggle uses the observed enabled state; routine toggles also require the observed target. Mint ignores schedule and dedupe and needs explicit acknowledgement. Run submits a no-input catalog job; delivery jobs require auto-drain. Never replay after a lost response; inspect authoritative state first.",
                &[
                    ("action", "string", true, "toggle, mint or run"),
                    ("kind", "string", true, "routine, auto_task or job"),
                    (
                        "name",
                        "string",
                        true,
                        "Exact definition or job name from the desktop projection",
                    ),
                    (
                        "expected_enabled",
                        "boolean",
                        false,
                        "Required for toggle: observed enabled flag",
                    ),
                    (
                        "enabled",
                        "boolean",
                        false,
                        "Required for toggle: desired enabled flag",
                    ),
                    (
                        "target",
                        "string",
                        false,
                        "Required for routine toggle: observed target",
                    ),
                    (
                        "acknowledge_unconditional",
                        "boolean",
                        false,
                        "Required true for mint: ignores enabled, schedule and dedupe, creates a task without dispatch",
                    ),
                ],
            ),
            Self::Snapshot => (
                "orbit.desktop.task.snapshot",
                "Read a versioned task snapshot with opaque revision and server-computed available actions. A snapshot grants no authority.",
                &[("id", "string", true, "Public task key")],
            ),
            Self::Write => (
                "orbit.desktop.task.write",
                "Apply one revision-guarded, retry-safe proposed-task create, field edit, comment or evidence-bound review. Reuse request_id with identical payload to reconcile a lost response. Changes requested stays in review. Completion requires existing trusted operator authority; it never merges, publishes or dispatches work.",
                &[
                    (
                        "request_id",
                        "string",
                        true,
                        "Unique request identity; retain until outcome is reconciled",
                    ),
                    (
                        "operation",
                        "object",
                        true,
                        "Typed create, edit, comment or review operation",
                    ),
                ],
            ),
        };
        let mut parameters = fields
            .iter()
            .map(|(name, ty, required, description)| ToolParam {
                name: (*name).into(),
                param_type: (*ty).into(),
                required: *required,
                description: (*description).into(),
            })
            .collect::<Vec<_>>();
        parameters.push(ToolParam {
            name: "workspace".into(),
            param_type: "string".into(),
            required: true,
            description: "Exact selector returned by workspace discovery. No implicit destination."
                .into(),
        });
        parameters.extend(super::model_identity_params());
        ToolSchema {
            name: name.into(),
            description: description.into(),
            parameters,
            builtin: true,
        }
    }
    fn input_schema(&self) -> Option<Value> {
        if !matches!(self, Self::Write) {
            return None;
        }
        let string = json!({"type":"string"});
        let strings = json!({"type":"array","items":{"type":"string"}});
        let fields = json!({"type":"object","additionalProperties":false,"properties":{
            "title":string,"description":string,"acceptance_criteria":strings,"priority":string,"crew":string
        }});
        let verdict = json!({"type":"object","additionalProperties":false,"required":["decision","rationale","criteria","evidence"],"properties":{
            "decision":{"enum":["accept","changes_requested"]},"rationale":string,"evidence":strings,
            "expected_run_id":{"type":["string","null"]},"expected_head":{"type":["string","null"]},
            "criteria":{"type":"array","items":{"type":"object","additionalProperties":false,"required":["criterion","met","evidence"],"properties":{"criterion":string,"met":{"type":"boolean"},"evidence":strings}}}
        }});
        let operations = [
            json!({"type":"object","additionalProperties":false,"required":["kind","title","description","acceptance_criteria"],"properties":{"kind":{"const":"create"},"title":string,"description":string,"acceptance_criteria":strings,"priority":string,"crew":{"type":["string","null"]}}}),
            json!({"type":"object","additionalProperties":false,"required":["kind","id","expected_revision","fields"],"properties":{"kind":{"const":"edit"},"id":string,"expected_revision":string,"fields":fields}}),
            json!({"type":"object","additionalProperties":false,"required":["kind","id","expected_revision","comment"],"properties":{"kind":{"const":"comment"},"id":string,"expected_revision":string,"comment":string}}),
            json!({"type":"object","additionalProperties":false,"required":["kind","id","expected_revision","verdict"],"properties":{"kind":{"const":"review"},"id":string,"expected_revision":string,"verdict":verdict,"complete":{"type":"boolean","default":false}}}),
        ];
        Some(
            json!({"type":"object","additionalProperties":false,"required":["workspace","request_id","operation"],"properties":{
                "workspace":{"type":"string","minLength":1},"model":string,"request_id":{"type":"string","minLength":1},"operation":{"oneOf":operations}
            }}),
        )
    }
    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::reject_unknown_tool_arguments(&input, &self.schema())?;
        orbit_common::protocol::tool_input::required_string(&input, &["workspace"], "workspace")?;
        if matches!(self, Self::Drain | Self::Automation)
            && ctx
                .orbit_host
                .as_ref()
                .is_some_and(|host| host.task_scope().run_id.is_some())
        {
            return Err(OrbitError::CapabilityDenied(
                "managed runs cannot control desktop automation".into(),
            ));
        }
        super::execute_host_action(
            ctx,
            input,
            match self {
                Self::Read => OrbitBuiltinAction::DesktopRead,
                Self::Drain => OrbitBuiltinAction::DesktopDrain,
                Self::Automation => OrbitBuiltinAction::DesktopAutomation,
                Self::Snapshot => OrbitBuiltinAction::DesktopTaskSnapshot,
                Self::Write => OrbitBuiltinAction::DesktopTaskWrite,
            },
        )
    }
}
