//! [`Operation`]: a command's identity, what it requires of its caller, and
//! how its audit row is attributed. `admit` reads the first two parts and
//! `audit` the first and third.

use orbit_common::governance::authorization::GovernedOperation;
use orbit_common::governance::operation::OperationVerb;
use serde_json::Value;

/// One thing a command requires of its caller.
#[derive(Clone)]
pub(super) enum Requirement {
    /// The caller holds what a governed operation requires.
    Governed(&'static GovernedOperation),
    /// The caller is a native operator; the text is the refusal.
    NativeOperator(&'static str),
}

/// Where a dashboard request came from, as its audit row records it.
#[derive(Clone, Default)]
pub struct DashboardOrigin {
    /// The serving host's name.
    pub host: Option<String>,
    /// The workspace selector the request addressed. Without one the row
    /// names the bound workspace's own id.
    pub workspace: Option<String>,
    /// The machine the browser acted from; empty when the request names none.
    pub machine_name: String,
    /// The web session the request arrived on, when the row records one.
    pub session: Option<String>,
}

/// The `dashboard.operations` row a governed dashboard write declares.
#[derive(Clone)]
pub(super) struct DashboardRowSpec {
    pub(super) governed: &'static GovernedOperation,
    pub(super) target: String,
    pub(super) arguments: Value,
    pub(super) origin: DashboardOrigin,
    pub(super) recorded_as: Option<&'static str>,
}

/// How an operation's command-line invocation audit `role` is resolved.
#[derive(Clone)]
pub(super) enum AuditRole {
    /// A fixed static role label (such as `"admin"`).
    Fixed(&'static str),
    /// Resolved with `audit_role_label` precedence: with `ORBIT_AGENT_*` set,
    /// flags > envelope > tool-JSON `model`; otherwise tool-JSON `model` >
    /// flags.
    ToolRun {
        input: Value,
        agent: Option<String>,
        model: Option<String>,
    },
}

/// One command's identity in the operation registry: the names its governance
/// rows and its audit row are keyed on.
#[derive(Clone)]
pub(crate) struct Operation {
    pub(super) command: &'static str,
    pub(super) subcommand: String,
    /// Whether the invocation row records `subcommand`; a bare command
    /// records none.
    pub(super) recorded_subcommand: bool,
    /// The tool whose governance row this chokepoint decides, if any.
    pub(super) tool_name: Option<&'static str>,
    /// The tool name recorded on the command-line invocation row, if any.
    pub(super) recorded_tool_name: Option<String>,
    pub(super) target_type: Option<&'static str>,
    pub(super) target_id: Option<String>,
    pub(super) job_run_id: Option<String>,
    pub(super) audit_role: Option<AuditRole>,
    /// The arguments a command-line invocation row records, as JSON text.
    pub(super) arguments_json: Option<String>,
    /// Whether a command-line caller is held to the command's governance row.
    pub(super) command_row: bool,
    /// Whether a command-line caller writes an invocation audit row.
    pub(super) invocation_audit: bool,
    /// What the caller must hold, decided in this order by `admit`.
    pub(super) requirements: Vec<Requirement>,
    /// The `dashboard.operations` row a session is owed, when one is declared.
    pub(super) dashboard_row: Option<DashboardRowSpec>,
}

impl Operation {
    /// The operation of `verb`, called with `fields`. The audit names are the
    /// ones the CLI derives from the same spec: the noun, the CLI path joined
    /// by `-`, and the noun's target parameter.
    pub(crate) fn of<V: OperationVerb>(verb: V, fields: &Value) -> Self {
        let noun = V::noun();
        let spec = verb.spec();
        Self {
            command: noun.cli_name,
            subcommand: spec.cli_path.join("-"),
            recorded_subcommand: true,
            tool_name: Some(spec.tool_name),
            recorded_tool_name: None,
            target_type: Some(noun.audit_target_type),
            target_id: fields
                .get(noun.audit_target_param)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            job_run_id: None,
            audit_role: None,
            arguments_json: None,
            command_row: true,
            invocation_audit: true,
            requirements: Vec::new(),
            dashboard_row: None,
        }
    }

    /// A command with no verb in the operation registry: it has no tool row
    /// and no target until [`Self::target`] names one.
    pub(crate) fn command(command: &'static str, subcommand: impl Into<String>) -> Self {
        Self {
            command,
            subcommand: subcommand.into(),
            recorded_subcommand: true,
            tool_name: None,
            recorded_tool_name: None,
            target_type: None,
            target_id: None,
            job_run_id: None,
            audit_role: None,
            arguments_json: None,
            command_row: true,
            invocation_audit: true,
            requirements: Vec::new(),
            dashboard_row: None,
        }
    }

    /// A read model only the dashboard asks for: no governance row decides
    /// it and no invocation row records it.
    pub(crate) fn dashboard_read(command: &'static str, subcommand: &'static str) -> Self {
        Self::command(command, subcommand)
            .without_command_row()
            .without_invocation_audit()
    }

    /// Require a native operator: one who holds operator capability and
    /// declares no agent. `refusal` is the text a refused caller reads.
    pub(crate) fn require_native_operator(mut self, refusal: &'static str) -> Self {
        self.requirements.push(Requirement::NativeOperator(refusal));
        self
    }

    /// Require the caller to hold what `operation` governs.
    pub(crate) fn requires(mut self, operation: &'static GovernedOperation) -> Self {
        self.requirements.push(Requirement::Governed(operation));
        self
    }

    /// Require `operation` only when the request asks for it. Requirements
    /// are decided in the order declared, so the first refusal is the one
    /// reported.
    pub(crate) fn requires_if(
        self,
        requested: bool,
        operation: &'static GovernedOperation,
    ) -> Self {
        if requested {
            self.requires(operation)
        } else {
            self
        }
    }

    /// Declare the `dashboard.operations` row a session is owed for this
    /// write. A row is declared, never inferred from a requirement.
    pub(crate) fn dashboard_row(
        mut self,
        governed: &'static GovernedOperation,
        target: impl Into<String>,
        arguments: Value,
        origin: DashboardOrigin,
    ) -> Self {
        self.dashboard_row = Some(DashboardRowSpec {
            governed,
            target: target.into(),
            arguments,
            origin,
            recorded_as: None,
        });
        self
    }

    /// Record the declared dashboard row under `name` instead of the governed
    /// operation's id, where one governed operation covers several recorded
    /// ones.
    pub(crate) fn dashboard_row_recorded_as(mut self, name: &'static str) -> Self {
        if let Some(row) = self.dashboard_row.as_mut() {
            row.recorded_as = Some(name);
        }
        self
    }

    /// Attribute an invocation to its named source run.
    pub(crate) fn job_run(mut self, id: &str) -> Self {
        self.job_run_id = Some(id.to_owned());
        self
    }

    /// Name the audit row's target where the noun's own is not the one.
    pub(crate) fn target(
        mut self,
        target_type: Option<&'static str>,
        target_id: Option<String>,
    ) -> Self {
        self.target_type = target_type;
        self.target_id = target_id;
        self
    }

    /// Record the audit row under `subcommand` where the CLI path is not the
    /// label the command has always been audited under.
    pub(crate) fn subcommand(mut self, subcommand: impl Into<String>) -> Self {
        self.subcommand = subcommand.into();
        self
    }

    /// Record no subcommand on the invocation row: the command ran bare, as
    /// the command line has always recorded it.
    pub(crate) fn without_recorded_subcommand(mut self) -> Self {
        self.recorded_subcommand = false;
        self
    }

    /// Record `name` as the `tool_name` column on the command-line invocation row.
    pub(crate) fn recorded_tool_name(mut self, name: Option<String>) -> Self {
        self.recorded_tool_name = name;
        self
    }

    /// Preserve the command's explicit audit attribution.
    pub(crate) fn audit_role(mut self, role: &'static str) -> Self {
        self.audit_role = Some(AuditRole::Fixed(role));
        self
    }

    /// Resolve the command's invocation audit `role` with `audit_role_label`
    /// precedence, the same rule a plain `orbit tool run` uses.
    pub(crate) fn tool_run_audit_role(
        mut self,
        input: Value,
        agent: Option<String>,
        model: Option<String>,
    ) -> Self {
        self.audit_role = Some(AuditRole::ToolRun {
            input,
            agent,
            model,
        });
        self
    }

    /// Record `arguments` on the command-line invocation row.
    pub(crate) fn arguments(mut self, arguments: &Value) -> Self {
        self.arguments_json = Some(arguments.to_string());
        self
    }

    /// Hold no caller to the command's governance row: the invocation is the
    /// non-destructive form of a command whose destruction is flag-gated.
    pub(crate) fn without_command_row(mut self) -> Self {
        self.command_row = false;
        self
    }

    /// Decide no tool row here: the command either runs through the generic
    /// tool entry, which decides the row, or never was a tool call. Deciding
    /// it here too would record one decision twice.
    pub(crate) fn without_tool_row(mut self) -> Self {
        self.tool_name = None;
        self
    }

    /// Write no command-line invocation audit row: used by `orbit audit`
    /// itself so reading or pruning the audit log does not emit a command row
    /// (authorization refusals still record their own decision row).
    pub(crate) fn without_invocation_audit(mut self) -> Self {
        self.invocation_audit = false;
        self
    }
}
