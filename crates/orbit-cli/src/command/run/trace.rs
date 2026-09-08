use std::collections::{HashMap, HashSet};

use clap::Args;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::run_audit::RunAuditEvent;
use serde_json::{Value, json};

use crate::command::{CommandOut, Execute, Payload};

use super::events::summarize_audit_event;
use super::steps::resolve_run;

#[derive(Args)]
#[command(
    after_help = "JSON shape: {\"run_id\":\"...\",\"job_id\":\"...\",\"roots\":[<tree-node>],\"orphans\":[<tree-node>]}\nExamples:\n  orbit run trace\n  orbit run trace jrun-20260426-0631 --json"
)]
pub struct RunTraceArgs {
    /// Run ID to inspect. Defaults to the most recently scheduled run globally.
    pub run_id: Option<String>,

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

impl Execute for RunTraceArgs {
    fn execute(self, runtime: &OrbitRuntime) -> CommandOut {
        run_trace_payload(runtime, self.run_id.as_deref())
    }
}

fn run_trace_payload(runtime: &OrbitRuntime, run_id: Option<&str>) -> CommandOut {
    let run = resolve_run(runtime, run_id)?;
    let events = runtime.collect_run_audit_events(&run.run_id)?;
    let tree = build_trace_tree(&events);
    let doc = json!({
        "run_id": run.run_id,
        "job_id": run.job_id,
        "roots": tree.roots.iter().map(trace_node_to_json).collect::<Vec<_>>(),
        "orphans": tree.orphans.iter().map(trace_node_to_json).collect::<Vec<_>>(),
    });

    if tree.roots.is_empty() && tree.orphans.is_empty() {
        return Ok(Payload::detail(doc, "No audit events recorded.").into());
    }

    let mut text = String::new();
    for node in &tree.roots {
        write_trace_node(&mut text, node, 0);
    }
    if !tree.orphans.is_empty() {
        text.push_str("Orphans:\n");
        for node in &tree.orphans {
            write_trace_node(&mut text, node, 1);
        }
    }
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(Payload::detail(doc, text).into())
}

#[derive(Clone, Debug)]
pub(crate) struct TraceTree {
    pub(crate) roots: Vec<TraceNode>,
    pub(crate) orphans: Vec<TraceNode>,
}

#[derive(Clone, Debug)]
pub(crate) struct TraceNode {
    pub(crate) event: RunAuditEvent,
    pub(crate) children: Vec<TraceNode>,
}

pub(crate) fn build_trace_tree(events: &[RunAuditEvent]) -> TraceTree {
    let index_by_id = events
        .iter()
        .enumerate()
        .map(|(index, event)| (event.event_id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut child_indexes = vec![Vec::<usize>::new(); events.len()];
    let mut roots = Vec::new();
    let mut orphans = Vec::new();

    for (index, event) in events.iter().enumerate() {
        match event
            .parent_event_id
            .as_ref()
            .and_then(|parent_id| index_by_id.get(parent_id))
        {
            Some(parent_index) => child_indexes[*parent_index].push(index),
            None if event.parent_event_id.is_some() => orphans.push(index),
            None => roots.push(index),
        }
    }

    TraceTree {
        roots: roots
            .into_iter()
            .map(|index| build_trace_node(index, events, &child_indexes, &mut HashSet::new()))
            .collect(),
        orphans: orphans
            .into_iter()
            .map(|index| build_trace_node(index, events, &child_indexes, &mut HashSet::new()))
            .collect(),
    }
}

fn build_trace_node(
    index: usize,
    events: &[RunAuditEvent],
    child_indexes: &[Vec<usize>],
    visited: &mut HashSet<usize>,
) -> TraceNode {
    if !visited.insert(index) {
        return TraceNode {
            event: events[index].clone(),
            children: Vec::new(),
        };
    }
    let children = child_indexes[index]
        .iter()
        .map(|child_index| build_trace_node(*child_index, events, child_indexes, visited))
        .collect::<Vec<_>>();
    visited.remove(&index);
    TraceNode {
        event: events[index].clone(),
        children,
    }
}

fn trace_node_to_json(node: &TraceNode) -> Value {
    json!({
        "event": node.event.json_with_step_id(),
        "children": node.children.iter().map(trace_node_to_json).collect::<Vec<_>>(),
    })
}

fn write_trace_node(text: &mut String, node: &TraceNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let prefix = if depth == 0 { "" } else { "- " };
    text.push_str(&format!(
        "{indent}{prefix}{} {}\n",
        node.event.event_type.as_deref().unwrap_or("-"),
        summarize_audit_event(&node.event)
    ));
    for child in &node.children {
        write_trace_node(text, child, depth + 1);
    }
}
