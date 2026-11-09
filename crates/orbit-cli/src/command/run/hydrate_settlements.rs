use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;

use clap::Args;
use orbit_core::application::distributed::{
    FollowerRunObserver, LeafHydrationOutcome, LeafHydrationRequest,
};
use orbit_core::{OrbitError, OrbitRuntime};
use orbit_mcp::federated::FederatedMcpHost;
use orbit_mcp::{McpHost, McpSessionAuthority};
use orbit_registry::{MachineIdentityState, inspect_machine_identity};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::command::host_route::caller_authority;
use crate::command::{Block, CommandOut, Execute, Payload};
use crate::parse::parse_since;

#[derive(Args)]
#[command(
    after_help = "Operator-only; run it on the owner. Reads each selected settled claim's leaf \
run record from its follower and keeps it as the claim's immutable `hydrated` evidence; \
`orbit run settlements --no-reconcile` then reports it as run_evidence.\n\
Outcomes, claim matching, paging and the JSON shape: installed Orbit skill \
`references/setup/distributed-drain.md`.\n\
Examples:\n  orbit run settlements-hydrate --follower-workspace orbit --limit 10\n  \
orbit run settlements-hydrate --follower-workspace orbit --limit 10 --after <claim-id>\n  \
orbit run settlements-hydrate --follower-workspace orbit --task <task-id> --recheck --json"
)]
pub struct RunHydrateSettlementsArgs {
    /// The workspace on each follower where its leaves ran, by name or `ws_*`
    /// id as that host lists it
    #[arg(long, value_name = "WORKSPACE")]
    pub follower_workspace: String,

    /// Only this claim
    #[arg(long)]
    pub claim: Option<String>,

    /// Only claims of this task
    #[arg(long)]
    pub task: Option<String>,

    /// Only claims that ran on this machine, by id or name
    #[arg(long)]
    pub machine: Option<String>,

    /// Only settlements recorded at or after this RFC 3339 time or relative
    /// duration (7d, 24h)
    #[arg(long)]
    pub since: Option<String>,

    /// Also observe claims that already hold evidence, to confirm it or
    /// report a conflict
    #[arg(long)]
    pub recheck: bool,

    /// Most claims to observe in one run (default 25, at most 100)
    #[arg(long)]
    pub limit: Option<usize>,

    /// Continue after this claim (the previous run's `next_after_claim_id`)
    #[arg(long, value_name = "CLAIM_ID")]
    pub after: Option<String>,
}

impl Execute for RunHydrateSettlementsArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        if caller_authority() != McpSessionAuthority::Operator {
            return Err(OrbitError::CapabilityRefused(
                "hydrating settlement evidence reads follower run records and requires the \
                 operator; an agent or managed scan reads `orbit run settlements --no-reconcile` \
                 instead"
                    .into(),
            ));
        }
        let global_root = runtime.global_root();
        let caller = match inspect_machine_identity(&global_root)? {
            MachineIdentityState::Present(identity) => identity.id,
            MachineIdentityState::Absent => {
                return Err(OrbitError::InvalidInput(
                    "this machine has no [machine] identity yet; run `orbit init` first".into(),
                ));
            }
        };
        let request = LeafHydrationRequest {
            claim_id: self.claim,
            task_id: self.task,
            machine: self.machine,
            since: self.since.as_deref().map(parse_since).transpose()?,
            recheck: self.recheck,
            limit: self.limit,
            after_claim_id: self.after,
        };
        let observer = OperatorRunObserver {
            global_root,
            caller,
            workspace: self.follower_workspace,
            client: RefCell::new(None),
            selectors: RefCell::new(HashMap::new()),
        };
        let report = runtime.hydrate_leaf_run_evidence(&request, &observer)?;

        use crate::output::table::{Column, Table};
        let mut table = Table::new(vec![
            Column::new("TASK").fixed(),
            Column::new("MACHINE").fixed(),
            Column::new("LEAF_RUN").fixed(),
            Column::new("OUTCOME").fixed(),
            Column::new("DETAIL"),
        ])
        .empty_message("no settled claims selected");
        for row in &report.rows {
            use comfy_table::Cell;
            table.add_row(vec![
                Cell::new(&row.task_id),
                Cell::new(&row.machine_id),
                Cell::new(row.leaf_run_id.as_deref().unwrap_or("-")),
                Cell::new(outcome_label(row.outcome)),
                Cell::new(row.detail.as_deref().unwrap_or("-")),
            ]);
        }
        let doc = json!({
            "rows": report.rows,
            "remaining": report.remaining,
            "next_after_claim_id": report.next_after_claim_id,
        });
        Ok(Payload::blocks(doc, vec![Block::table(table)]).into())
    }
}

fn outcome_label(outcome: LeafHydrationOutcome) -> String {
    orbit_common::protocol::serde_string_label(&outcome).unwrap_or_default()
}

/// The operator's own read of a follower's run records: the existing
/// federated client, opened as the operator, asking the follower for
/// `orbit.run` action `observe` in the workspace the operator named.
struct OperatorRunObserver {
    global_root: PathBuf,
    caller: String,
    workspace: String,
    client: RefCell<Option<FederatedMcpHost>>,
    selectors: RefCell<HashMap<String, String>>,
}

impl FollowerRunObserver for OperatorRunObserver {
    fn observe_run(&self, machine_id: &str, run_id: &str) -> Result<Value, OrbitError> {
        if self.client.borrow().is_none() {
            let client = orbit_cmd::hosts::routed_client(
                &self.global_root,
                &self.caller,
                McpSessionAuthority::Operator,
            )?;
            *self.client.borrow_mut() = Some(client);
        }
        let client = self.client.borrow();
        let client = client
            .as_ref()
            .ok_or_else(|| OrbitError::Store("operator route unavailable".into()))?;
        let cached = self.selectors.borrow().get(machine_id).cloned();
        let selector = match cached {
            Some(selector) => selector,
            None => {
                let selector = client.host_workspace_selector(machine_id, &self.workspace)?;
                self.selectors
                    .borrow_mut()
                    .insert(machine_id.to_string(), selector.clone());
                selector
            }
        };
        client.call_tool(
            "orbit.run",
            json!({ "action": "observe", "id": run_id, "workspace": selector }),
            ToolSessionContext::default(),
        )
    }
}
