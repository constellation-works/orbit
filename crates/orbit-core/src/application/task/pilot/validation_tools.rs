//! Feasibility check for the tools a task's acceptance criteria name [ORB-11980].
//!
//! Acceptance criteria routinely name the exact tool and transport a
//! validation must use. Nothing compared that wording against the lane that
//! would run the work, so a criterion could demand an MCP call for a CLI-only
//! tool (F2026-09-065) or an operator-reserved tool the implementation
//! allowlist never grants (F2026-09-066). Both contradictions surfaced only
//! after an implementing agent had spent most of a run discovering them, and
//! `required_tools` — the one field that could have granted the tool — is
//! immutable after creation, so by then nothing could repair it in place.
//!
//! This check reads the same canonical data the runtime enforces with: the
//! registered tool surface, the MCP exposure set from
//! [`orbit_tools::canonical_builtin_mcp_tool_definitions`], the implementation
//! activity's tool allowlist, and the governed-operation registry.
//!
//! Preparation findings remain advisory; admission separately holds a current
//! operator-reserved requirement for an explicit operator decision:
//!
//! - Prose never grants. A tool named in a criterion stays ungranted;
//!   `required_tools` remains the only grant, and this check merely reports
//!   when the two disagree.
//! - A mention never rejects preparation. A criterion that
//!   quotes an expected denial, or a fixture that names a tool it expects to
//!   be refused, cannot fail preparation.
//!
//! Nothing here proposes widening the MCP surface, relaxing an allowlist, or
//! setting an authority environment variable. The actionable repair for an
//! operator-reserved validation is an operator handoff, not a wider lane.

use std::collections::BTreeSet;

use orbit_common::governance::authorization::governed_tool;
use orbit_types::task::Task;
use orbit_types::tool::McpCapability;
use orbit_types::workflow::activity_job::{
    ActivityV2Spec, tool_allowed, tools_allowed_by_disallow_list,
};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::validation_tools::{names_tool, positive_validation_sentences};
use crate::application::task::{OperatorValidationRequirement, operator_validation_requirements};

/// Activity that implements a task. Its tool allowlist is the baseline a
/// task's `required_tools` extends, so it is the lane a validation criterion
/// has to be feasible in.
const IMPLEMENTATION_ACTIVITY: &str = "agent_implement";

/// Owner-mode attachment tool. Claimed mode does not need it: the implement
/// handoff attaches declared `task_artifacts` either way.
const ARTIFACT_ATTACHMENT_TOOL: &str = "orbit.task.artifact.put";

/// Cap on findings reported for one task. A criterion set that contradicts the
/// lane this many times needs rewriting, not a longer list.
const MAX_FINDINGS_PER_TASK: usize = 8;

/// Tool families whose execution depends on credentials Orbit neither holds
/// nor can grant, paired with the precondition to state.
const EXTERNAL_CREDENTIALS: &[(&str, &str)] = &[("github.", "GitHub authentication")];

const MCP_TRANSPORT_MARKERS: &[&str] = &["mcp", "tools/call", "tools/list"];

/// The execution lane a task's validation criteria have to be feasible in.
///
/// Each source is optional because none of them may fail preparation: when a
/// catalog or the registry cannot be read, the dimension it feeds reports
/// nothing rather than guessing.
pub(super) struct ImplementationLane {
    /// Active, agent-facing registered tool names, sorted so findings are
    /// reported in a stable order.
    registered: Vec<String>,
    /// Canonical MCP-advertised names, or `None` when the definition set is
    /// invalid.
    mcp_exposed: Option<BTreeSet<String>>,
    /// The implementation activity's declared allowlist — in deny mode, every
    /// registered tool its disallow list leaves callable — or `None` when the
    /// activity catalog is unavailable.
    baseline: Option<Vec<String>>,
    /// A deny-mode implementation activity's disallow list, which a task's
    /// `required_tools` cannot override.
    disallow_list: Vec<String>,
}

impl ImplementationLane {
    pub(super) fn resolve(runtime: &OrbitRuntime) -> Self {
        let mut registered = runtime.allowlist_known_tool_names();
        registered.sort();

        let mcp_exposed = orbit_tools::canonical_builtin_mcp_tool_definitions()
            .ok()
            .map(|definitions| {
                definitions
                    .into_iter()
                    .map(|definition| definition.schema.name)
                    .collect()
            });

        let policy = runtime.v2_activity_catalog().ok().and_then(|catalog| {
            match &catalog.get(IMPLEMENTATION_ACTIVITY)?.spec {
                ActivityV2Spec::AgentLoop(spec) => Some(match &spec.tool_disallow_list {
                    Some(disallow_list) => (
                        tools_allowed_by_disallow_list(
                            disallow_list,
                            registered.iter().map(String::as_str),
                        ),
                        disallow_list.clone(),
                    ),
                    None => (spec.tools.clone(), Vec::new()),
                }),
                ActivityV2Spec::Deterministic(_) => None,
            }
        });
        let (baseline, disallow_list) = match policy {
            Some((baseline, disallow_list)) => (Some(baseline), disallow_list),
            None => (None, Vec::new()),
        };

        Self {
            registered,
            mcp_exposed,
            baseline,
            disallow_list,
        }
    }

    /// The lane description the task-pilot prompt reads.
    ///
    /// `None` when the activity catalog could not be read, so the prompt does
    /// not invent an empty tool set. `artifact_attachment.claimed` is the
    /// handoff, which attaches screenshots and other task artifacts whether
    /// or not [`ARTIFACT_ATTACHMENT_TOOL`] is callable.
    pub(super) fn prompt_input(&self) -> Value {
        match &self.baseline {
            Some(tools) => json!({
                "activity": IMPLEMENTATION_ACTIVITY,
                "tools": tools,
                "artifact_attachment": {
                    "owner": ARTIFACT_ATTACHMENT_TOOL,
                    "owner_granted": tools.iter().any(|tool| tool == ARTIFACT_ATTACHMENT_TOOL),
                    "claimed": "task_artifacts handoff",
                },
            }),
            None => Value::Null,
        }
    }

    /// Screenshot and task-artifact criteria are feasible when this lane is
    /// known. Owner mode calls [`ARTIFACT_ATTACHMENT_TOOL`] when that tool is
    /// granted. Claimed mode attaches declared files through the handoff even
    /// when the tool is absent, and that handoff is part of the activity, not
    /// a registered tool. An unreadable catalog is not treated as feasible.
    pub(super) fn artifacts_attachable(&self) -> bool {
        self.baseline.is_some()
    }

    /// Tools this task's implement activity can call: the deny-mode baseline
    /// plus `required_tools` entries the disallow list does not remove.
    fn granted_names(&self, required_tools: &[String]) -> Option<BTreeSet<String>> {
        let baseline = self.baseline.as_ref()?;
        let mut granted = baseline.iter().cloned().collect::<BTreeSet<_>>();
        for tool in required_tools {
            if !tool_allowed(tool, &self.disallow_list) {
                granted.insert(tool.clone());
            }
        }
        Some(granted)
    }

    /// Findings for every tool this task's acceptance criteria positively
    /// require.
    ///
    /// Only acceptance criteria are read. They are the task's validation
    /// contract; a tool named in the problem statement describes what broke,
    /// not what the implementing agent has to call.
    pub(super) fn validation_warnings(&self, task: &Task) -> Vec<String> {
        let granted = self.baseline.as_ref().map(|baseline| {
            baseline
                .iter()
                .chain(
                    task.required_tools
                        .iter()
                        .filter(|tool| !tool_allowed(tool, &self.disallow_list)),
                )
                .cloned()
                .collect::<Vec<_>>()
        });

        let mut findings = Findings::default();
        for (_, sentence) in positive_validation_sentences(task) {
            for tool in self.tools_named_in(&sentence) {
                self.report(tool, &sentence, granted.as_deref(), &mut findings);
                if findings.is_full() {
                    return findings.ordered;
                }
            }
        }
        findings.ordered
    }

    /// Typed operator requirements use the same positive-mention parser as
    /// warning generation, so quoted denials never become admission holds.
    pub(super) fn operator_requirements(&self, task: &Task) -> Vec<OperatorValidationRequirement> {
        operator_validation_requirements(task, &self.registered)
    }

    /// Matching walks the registered surface rather than scanning for
    /// dotted-looking words, so a name this Orbit does not expose to agents is
    /// simply not a tool mention. That keeps prose from inventing a tool, at
    /// the cost of staying silent on a name only dispatch can resolve — where
    /// `required_tools` admission already rejects it.
    fn tools_named_in<'a>(&'a self, sentence: &str) -> Vec<&'a str> {
        self.registered
            .iter()
            .map(String::as_str)
            .filter(|tool| names_tool(sentence, tool))
            .collect()
    }

    /// Emit one finding per way the lane refuses this tool, so the author sees
    /// which repair applies: restate the transport, declare the tool, or hand
    /// the validation to an operator.
    fn report(
        &self,
        tool: &str,
        sentence: &str,
        granted: Option<&[String]>,
        findings: &mut Findings,
    ) {
        if names_mcp_transport(sentence)
            && self
                .mcp_exposed
                .as_ref()
                .is_some_and(|exposed| !exposed.contains(tool))
        {
            findings.push(format!(
                "acceptance criterion validates `{tool}` over MCP, but `{tool}` is registered CLI-only and is absent from Orbit's canonical MCP tool list; drive it through `orbit tool run {tool}` or restate the criterion — do not widen the MCP surface to match the wording"
            ));
        }

        if granted.is_some_and(|granted| !tool_allowed(tool, granted)) {
            findings.push(format!(
                "acceptance criterion requires `{tool}`, which the `{IMPLEMENTATION_ACTIVITY}` allowlist does not grant and this task's `required_tools` does not declare; declare it in `required_tools` at creation, because that field is immutable once the task exists"
            ));
        }

        if let Some(operation) = governed_tool(tool)
            && !operation.allowed.contains(&McpCapability::Agent)
        {
            findings.push(format!(
                "acceptance criterion requires `{tool}`, a governed operation reserved for the {} capability; `required_tools` grants allowlist membership only, so route this validation to an explicit operator handoff instead of expecting the implementing agent to perform it",
                capability_label(operation.allowed)
            ));
        }

        if let Some((_, credential)) = EXTERNAL_CREDENTIALS
            .iter()
            .find(|(family, _)| tool.starts_with(family))
        {
            findings.push(format!(
                "acceptance criterion requires `{tool}`, whose result depends on {credential} in the executing lane; `required_tools` cannot supply credentials, so state that precondition or hand the check to an operator"
            ));
        }
    }
}

/// Deduplicated, order-preserving, bounded findings for one task.
#[derive(Default)]
struct Findings {
    seen: BTreeSet<String>,
    ordered: Vec<String>,
}

impl Findings {
    fn push(&mut self, finding: String) {
        if !self.is_full() && self.seen.insert(finding.clone()) {
            self.ordered.push(finding);
        }
    }

    fn is_full(&self) -> bool {
        self.ordered.len() >= MAX_FINDINGS_PER_TASK
    }
}

/// Drop `utility_warnings` that say screenshot or task-artifact attachment is
/// infeasible because of the implement allowlist or an empty `required_tools`.
///
/// The drop runs only when the lane can attach (the owner tool, the claimed
/// `task_artifacts` handoff, or both). A warning that also names a tool the
/// lane does not grant is kept, as is every warning that is not about
/// artifact attachment. Other warning fields are untouched.
pub(super) fn drop_false_artifact_warnings(
    assessment: &mut Value,
    lane: &ImplementationLane,
    required_tools: &[String],
) {
    if !lane.artifacts_attachable() {
        return;
    }
    let Some(granted) = lane.granted_names(required_tools) else {
        return;
    };
    let Some(warnings) = assessment
        .get_mut("utility_warnings")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    warnings.retain(|warning| match warning.as_str() {
        Some(text) => !false_artifact_attachment_warning(text, lane, &granted),
        None => true,
    });
}

fn false_artifact_attachment_warning(
    text: &str,
    lane: &ImplementationLane,
    granted: &BTreeSet<String>,
) -> bool {
    if !claims_artifact_attachment_unavailable(text) {
        return false;
    }
    !lane.registered.iter().any(|tool| {
        tool != ARTIFACT_ATTACHMENT_TOOL && names_tool(text, tool) && !granted.contains(tool)
    })
}

fn claims_artifact_attachment_unavailable(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    if !mentions_artifact_attachment(&lowered) {
        return false;
    }
    const UNAVAILABLE: &[&str] = &[
        "allowlist",
        "required_tools",
        "infeasible",
        "does not grant",
        "doesn't grant",
        "not grant",
        "cannot attach",
        "can't attach",
        "unable to attach",
        "unattachable",
        "not attach",
    ];
    UNAVAILABLE.iter().any(|phrase| lowered.contains(phrase))
}

fn mentions_artifact_attachment(lowered: &str) -> bool {
    lowered.contains(ARTIFACT_ATTACHMENT_TOOL)
        || lowered.contains("task artifact")
        || (lowered.contains("screenshot") && lowered.contains("artifact"))
}

fn capability_label(allowed: &[McpCapability]) -> String {
    allowed
        .iter()
        .map(McpCapability::to_string)
        .collect::<Vec<_>>()
        .join(" or ")
}

fn names_mcp_transport(sentence: &str) -> bool {
    let lowered = sentence.to_ascii_lowercase();
    MCP_TRANSPORT_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}
