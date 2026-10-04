//! Bounded Automation projections and operator actions. Definition discovery,
//! schedule interpretation, atomic edits and dispatch belong to their domain owners.
use std::path::Path;

use chrono::Utc;
use orbit_common::{OrbitError, protocol::tool_input::required_string};
use orbit_types::{
    workflow::JobRunTrigger,
    workspace::{Workspace, WorkspaceStatus},
};
use serde_json::{Value, json};

use super::desktop_read_tools::bounded_text;
use crate::{
    OrbitRuntime,
    application::{
        job::JobCatalogFilter,
        routines::{
            self, DiscoveredWorkspaces, RoutineMachineIdentity, RoutineStatusReport,
            RoutineToggleOutcome, RoutineWorkspaceProvider,
        },
    },
};

// A selected destination is the entire discovery scope. This adapter never
// discovers another store by cwd or consults a client-supplied filesystem path.
struct SelectedWorkspace(OrbitRuntime);
impl RoutineWorkspaceProvider for SelectedWorkspace {
    fn discover_workspaces(&self, _: &Path) -> Result<DiscoveredWorkspaces, OrbitError> {
        let runtime = &self.0;
        let workspace = Workspace {
            id: runtime.workspace_id()?,
            name: runtime.workspace_label(),
            owner_machine_id: runtime
                .workspace_runtime_binding()
                .and_then(|b| b.owner_machine_id.clone()),
            git_remote: None,
            ship_mode: None,
            base_branch: runtime.workspace_base_branch().into(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        Ok(DiscoveredWorkspaces {
            entries: vec![(workspace, runtime.clone())],
            ..DiscoveredWorkspaces::default()
        })
    }
}
fn routine_report(runtime: &OrbitRuntime) -> Result<RoutineStatusReport, OrbitError> {
    routines::routine_statuses_with_providers(
        &runtime.global_root(),
        RoutineMachineIdentity {
            machine_id: runtime
                .automation_machine_identity()
                .unwrap_or("local")
                .into(),
            machine_name: "Selected host".into(),
        },
        &SelectedWorkspace(runtime.clone()),
        Utc::now(),
    )
}
fn text(value: &str) -> String {
    let (value, truncated) = bounded_text(value, 4096);
    if truncated {
        format!("{value} [display truncated]")
    } else {
        value
    }
}

pub(super) fn read(
    runtime: &OrbitRuntime,
    scope: &str,
    offset: usize,
    limit: usize,
) -> Result<Value, OrbitError> {
    let mut notes = Vec::new();
    let mut items=match scope {
        "routines"=>{
            let report=routine_report(runtime)?;
            notes.extend(report.load_errors.iter().map(|e|text(&e.message)));
            notes.extend(report.listed_retired(true).map(|r|text(&format!("{}: {}",r.name,r.reason))));
            report.statuses.iter().map(|s|{
                let d=&s.routine.definition;
                json!({"name":d.name,"description":text(&d.description),"enabled":d.enabled,"effective":s.effective(),
                    "target":d.target.as_ref_string(),"schedule":d.trigger,"state":s.schedule_display_state().as_str(),
                    "next_due":s.next_due,"paused_at":s.paused_at,"last_fire":s.last_fire.as_ref().map(|f|json!({"state":f.state,"at":f.updated_at})),
                    "toggle_available":true})
            }).collect::<Vec<_>>()
        }
        "auto_tasks"=>auto_task_rows(runtime)?,
        "jobs"=>runtime.list_job_catalog_with_last_run(true,JobCatalogFilter::All)?.iter().map(|(j,last)|{
            let runnable=j.supports_no_input_submission();
            json!({"name":j.job_id,"kind":j.kind(),"state":j.state(),"max_active_runs":j.max_active_runs(),
                "steps":j.spec.steps.len(),"run_available":runnable,
                "run_reason":if runnable {"Submit this job with its default input"}else{"Delivery jobs require task input or auto-drain; disabled jobs cannot run here"},
                "last_run":last.as_ref().map(|r|json!({"run_id":r.run_id,"state":r.state,"at":r.created_at,"duration_ms":r.duration_ms}))})
        }).collect::<Vec<_>>(),
        _=>return Err(invalid("unknown automation scope")),
    };
    items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let total = items.len();
    let page = items
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let next = offset.saturating_add(page.len());
    Ok(
        json!({"items":page,"total":total,"controls_authorized":true,
        "pagination":{"offset":offset,"limit":limit,"next_offset":if next<total{Some(next)}else{None}},
        "notes":notes.into_iter().take(20).collect::<Vec<_>>(),
        "observation":"Workspace definitions only. The host clock is independent; enabled does not guarantee a running scheduler."}),
    )
}
fn auto_task_rows(runtime: &OrbitRuntime) -> Result<Vec<Value>, OrbitError> {
    use crate::application::auto_tasks::{
        cursor_state_path, load_cursor_state, schedule::next_scheduled_slot,
    };
    use orbit_types::workflow::AutoTaskSchedule;
    let cursors = load_cursor_state(&cursor_state_path(&runtime.paths().state_dir))?;
    runtime.auto_task_listing(true)?.iter().map(|s|{
        let d=&s.definition;
        let enabled_by_review_policy=runtime.auto_task_enabled_by_review_policy(d);
        let effective_enabled=runtime.auto_task_enabled(d);
        let cursor=cursors.definitions.get(&d.name);
        let baseline=cursor.and_then(|c|chrono::DateTime::parse_from_rfc3339(&c.baseline_at).ok()).map(|d|d.with_timezone(&Utc));
        let next=next_scheduled_slot(&d.schedule,baseline,Utc::now())?;
        let state=if s.inactive_plugin.is_some(){"plugin_inactive"}else if !effective_enabled{"disabled"}else if matches!(d.schedule,AutoTaskSchedule::Deliveries{..}){"waiting"}else if cursor.is_none(){"never_observed"}else if next.is_some(){"scheduled"}else{"unavailable"};
        Ok(json!({"name":d.name,"description":text(&d.description),"enabled":d.enabled,"schedule":d.schedule,
            "state":state,"next_due":if state=="scheduled"{next}else{None},
            "target":text(&d.template.title),"dedupe":d.dedupe,"skip_reason":s.skipped_reason,
            "enabled_by_review_policy":enabled_by_review_policy,"effective_enabled":effective_enabled,
            "toggle_available":s.inactive_plugin.is_none(),"mint_available":s.inactive_plugin.is_none(),
            "updated_at":d.updated_at}))
    }).collect()
}

fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}
fn boolean(input: &Value, key: &str) -> Result<bool, OrbitError> {
    input[key]
        .as_bool()
        .ok_or_else(|| invalid(&format!("{key} must be a boolean")))
}

pub(super) fn control(
    runtime: &OrbitRuntime,
    input: Value,
    trigger: JobRunTrigger,
) -> Result<Value, OrbitError> {
    runtime.ensure_coordination_task_write_permitted()?;
    let action = required_string(&input, &["action"], "action")?;
    let kind = required_string(&input, &["kind"], "kind")?;
    let name = required_string(&input, &["name"], "name")?;
    let allowed = match (kind.as_str(), action.as_str()) {
        ("routine", "toggle") => vec!["expected_enabled", "enabled", "target"],
        ("auto_task", "toggle") => vec!["expected_enabled", "enabled"],
        ("auto_task", "mint") => vec!["acknowledge_unconditional"],
        ("job", "run") => vec![],
        _ => return Err(invalid("unsupported automation action")),
    };
    for key in [
        "expected_enabled",
        "enabled",
        "target",
        "acknowledge_unconditional",
    ] {
        if input.get(key).is_some() && !allowed.contains(&key) {
            return Err(invalid(&format!("{key} is not valid for this action")));
        }
    }
    let mut result = match (kind.as_str(), action.as_str()) {
        ("routine", "toggle") => {
            let expected = boolean(&input, "expected_enabled")?;
            let enabled = boolean(&input, "enabled")?;
            let target = required_string(&input, &["target"], "target")?;
            let report = routine_report(runtime)?;
            let status = report
                .statuses
                .iter()
                .find(|s| s.routine.definition.name == name)
                .ok_or_else(|| invalid("routine unavailable in this workspace"))?;
            if status.routine.definition.target.as_ref_string() != target {
                return Err(invalid("routine target changed; refresh before retrying"));
            }
            match routines::set_routine_enabled(&status.routine, expected, enabled)? {
                RoutineToggleOutcome::Changed | RoutineToggleOutcome::Unchanged => {
                    json!({"enabled":enabled})
                }
                _ => return Err(invalid("routine changed; refresh before retrying")),
            }
        }
        ("auto_task", "toggle") => {
            let d = runtime.auto_task_toggle_checked(
                &name,
                boolean(&input, "expected_enabled")?,
                boolean(&input, "enabled")?,
            )?;
            json!({"enabled":d.enabled,
                "enabled_by_review_policy":runtime.auto_task_enabled_by_review_policy(&d),
                "effective_enabled":runtime.auto_task_enabled(&d)})
        }
        ("auto_task", "mint") => {
            if !boolean(&input, "acknowledge_unconditional")? {
                return Err(invalid(
                    "mint requires acknowledgement that schedule, enabled and dedupe are ignored",
                ));
            }
            let task = runtime.auto_task_mint(&name)?;
            json!({"task_id":task.id})
        }
        ("job", "run") => {
            let r = runtime.submit_no_input_catalog_job_run(&name, Some("desktop"), trigger)?;
            json!({"run_id":r.run_id,"state":if r.queued{"queued"}else{"submitted"}})
        }
        _ => unreachable!(),
    };
    result["schema_version"] = json!(1);
    result["workspace"] = input["workspace"].clone();
    result["action"] = json!(action);
    result["kind"] = json!(kind);
    result["name"] = json!(name);
    Ok(result)
}
