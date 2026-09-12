use axum::body::to_bytes;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use std::fs;
use std::process::Command;

use crate::{
    DASHBOARD_CSP, serve_app_js, serve_audit_js, serve_automation_js, serve_common_js,
    serve_diagnostics_js, serve_field_editor_js, serve_index, serve_inter_font,
    serve_jetbrains_mono_font, serve_log_tail_js, serve_markdown_js, serve_marked_js,
    serve_operations_js, serve_purify_js, serve_reliability_js, serve_router_js,
    serve_run_detail_js, serve_runs_js, serve_scoreboard_js, serve_tasks_js,
};

// The recent-history, aggregate-request, and route-selection assertions
// addressed by this task have three dispositions:
// * Keep static source checks when the source itself is the product contract
//   (embedded asset packaging, CSP/MIME, or required copy/markup).
// * Replace behavior claims with the Node harness below, which imports the
//   shipped ES modules and observes DOM state or requests.
// * Delete implementation-shape checks (helper names, predicate placement, and
//   exact call counts) once the observable behavior is covered. Those shapes
//   are not dashboard contracts and should be free to change during refactors.
fn run_dashboard_javascript_test(script: &str) {
    let temp_dir =
        tempfile::tempdir().expect("create temporary dashboard JavaScript test directory");
    let assets_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/dashboard");
    for entry in fs::read_dir(&assets_dir).expect("read dashboard asset directory") {
        let entry = entry.expect("read dashboard asset entry");
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "js") {
            let destination = temp_dir.path().join(entry.file_name());
            fs::copy(&path, destination).expect("copy shipped dashboard JavaScript module");
        }
    }
    fs::write(temp_dir.path().join("package.json"), r#"{"type":"module"}"#)
        .expect("write temporary JavaScript module manifest");
    let harness_path = temp_dir.path().join("dashboard-behavior.mjs");
    fs::write(&harness_path, script).expect("write dashboard JavaScript behavior harness");

    let output = Command::new("node")
        .arg(&harness_path)
        .current_dir(temp_dir.path())
        .output()
        .expect("run Node dashboard behavior harness");
    assert!(
        output.status.success(),
        "dashboard behavior harness failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[tokio::test]
async fn dashboard_html_and_js_routes_emit_csp() {
    let routes = [
        ("index", serve_index().await),
        ("inter", serve_inter_font().await),
        ("jetbrains_mono", serve_jetbrains_mono_font().await),
        ("marked", serve_marked_js().await),
        ("purify", serve_purify_js().await),
        ("app", serve_app_js().await),
        ("common", serve_common_js().await),
        ("markdown", serve_markdown_js().await),
        ("tasks", serve_tasks_js().await),
        ("field_editor", serve_field_editor_js().await),
        ("audit", serve_audit_js().await),
        ("scoreboard", serve_scoreboard_js().await),
        ("reliability", serve_reliability_js().await),
        ("log_tail", serve_log_tail_js().await),
        ("diagnostics", serve_diagnostics_js().await),
        ("router", serve_router_js().await),
        ("runs", serve_runs_js().await),
        ("run_detail", serve_run_detail_js().await),
        ("operations", serve_operations_js().await),
        ("automation", serve_automation_js().await),
    ];

    for (name, response) in routes {
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&HeaderValue::from_static(DASHBOARD_CSP)),
            "{name} route must emit the dashboard CSP"
        );
    }
}

#[tokio::test]
async fn dashboard_index_self_hosts_markdown_runtime() {
    let body = response_body(serve_index().await).await;

    assert!(body.contains(r#"<script src="/static/marked.umd.js"></script>"#));
    assert!(body.contains(r#"<script src="/static/purify.min.js"></script>"#));
    assert!(!body.contains("cdn.jsdelivr.net"));
}

#[tokio::test]
async fn dashboard_self_hosts_fonts_without_google_requests() {
    let index = response_body(serve_index().await).await;
    let css = response_body(crate::serve_dashboard_css().await).await;

    assert!(!index.contains("fonts.googleapis.com"));
    assert!(!index.contains("fonts.gstatic.com"));
    assert!(css.contains("/static/fonts/inter-latin.woff2"));
    assert!(css.contains("/static/fonts/jetbrains-mono-latin.woff2"));
    assert_eq!(
        DASHBOARD_CSP,
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; font-src 'self'; img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
    );

    for response in [serve_inter_font().await, serve_jetbrains_mono_font().await] {
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("font/woff2"))
        );
        assert!(
            !to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read font response body")
                .is_empty()
        );
    }
}

#[test]
fn dashboard_omits_retired_duel_surfaces() {
    let index = include_str!("../../assets/dashboard/index.html");
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    for asset in [index, scoreboard, css] {
        assert!(!asset.to_ascii_lowercase().contains("duel"));
    }
}

#[test]
fn dashboard_renders_normalized_managed_token_usage_without_provider_ranking() {
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(scoreboard.contains("Normalized token usage"));
    assert!(scoreboard.contains("unknown model/input basis (excluded, never guessed)"));
    assert!(scoreboard.contains("vs preceding equal window"));
    assert!(scoreboard.contains("lifetime window · no comparison baseline"));
    assert!(scoreboard.contains("Model attribution (not a cross-provider ranking)"));
    assert!(scoreboard.contains(
        "Direct interactive Codex or Claude orchestration-session overhead is excluded."
    ));
    assert!(css.contains(".scoreboard-token-usage"));
    assert!(css.contains("@media (max-width: 620px)"));
}

#[test]
fn dashboard_markdown_call_sites_use_sanitizing_wrapper() {
    let wrapper = include_str!("../../assets/dashboard/markdown.js");
    let app = include_str!("../../assets/dashboard/app.js");
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(wrapper.contains("DOMPurify"));
    assert!(wrapper.contains(".sanitize("));
    assert!(wrapper.contains("marked[methodName]"));
    assert!(!app.contains("marked.parse"));
    assert!(!tasks.contains("marked.parse"));
    assert!(app.contains("renderMarkdown("));
    assert!(tasks.contains("renderMarkdown("));
    assert!(tasks.contains("renderMarkdownInline("));
}

#[test]
fn dashboard_surfaces_workspace_location() {
    // ORB-10124: the selector shows only the selected workspace's label — the
    // secondary filesystem-path line (ORB-00037) was removed as distracting
    // implementation detail. Each aggregate task still shows its workspace
    // location in the Details box (a separate, unrelated feature). Asserted
    // against the embedded asset sources since the dashboard has no JS test
    // runner (see dashboard_markdown_call_sites above).
    let app = include_str!("../../assets/dashboard/app.js");
    let tasks = include_str!("../../assets/dashboard/tasks.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    // Selector secondary line must be gone, along with its update helper and style.
    assert!(
        !app.contains("workspace-path"),
        "the selector must no longer render a secondary filesystem-path line"
    );
    assert!(
        !app.contains("updateWorkspacePath"),
        "updateWorkspacePath must be removed along with the path line it rendered"
    );
    assert!(
        !css.contains(".workspace-path"),
        "the workspace-path CSS rule must be removed with its markup"
    );

    // Task Details box: a "location" field driven by the tagged workspace_root.
    assert!(tasks.contains("workspace_root"));
    assert!(tasks.contains(r#"addField(rightCol, "location""#));
    assert!(tasks.contains("ws-location"));
}

#[test]
fn dashboard_task_actions_route_to_selected_workspace() {
    // ORB-10124: approve/reject/archive built their request with a raw
    // `fetch()`, bypassing the `withWorkspace()` helper that every other
    // dashboard request goes through (fetchJson/requestJson in common.js).
    // Against a remote registered workspace the mutation silently applied to
    // the default workspace (or 400'd) instead of the selected one, so the
    // dashboard never reflected the change. Asserted against the embedded
    // asset sources since the dashboard has no JS test runner.
    let tasks = include_str!("../../assets/dashboard/tasks.js");
    let common = include_str!("../../assets/dashboard/common.js");

    assert!(
        common.contains("export function withWorkspace("),
        "withWorkspace must be exported from common.js so other modules can reuse it"
    );
    assert!(
        tasks
            .lines()
            .any(|line| line.contains("from './common.js'") && line.contains("withWorkspace")),
        "tasks.js must import withWorkspace from common.js"
    );
    assert!(
        tasks.contains(
            "fetch(withWorkspace(opts.path || `/api/tasks/${encodeURIComponent(task.id)}/${kind}`)"
        ),
        "runAction (approve/reject/archive) must route its request through withWorkspace"
    );
}

#[test]
fn dashboard_run_resume_matches_runtime_guard_and_surfaces_lineage_and_errors() {
    let runs = include_str!("../../assets/dashboard/runs.js");

    assert!(
        runs.contains(
            r#"const RESUMABLE_RUN_STATES = new Set(["failed", "interrupted", "timeout"])"#
        ),
        "Resume must only be offered for states accepted by resume_job_run"
    );
    assert!(
        runs.contains("/api/job-runs/${encodeURIComponent(runId)}/resume"),
        "Resume must POST to the job-run action route"
    );
    assert!(
        runs.contains("re-runs the failed step and all subsequent steps")
            && runs.contains("underlying cause is resolved"),
        "the confirmation must explain checkpoint resume semantics honestly"
    );
    assert!(
        runs.contains("text: `resumed as ${resumedAsId}`")
            && runs.contains("text: `from ${sourceId}`"),
        "the runs table must expose both directions of resumed-run lineage"
    );
    assert!(
        runs.contains(r#"class: "action-error", text: e.message || "resume failed""#),
        "Resume failures must display the server-provided error text"
    );
}

#[test]
fn dashboard_run_events_show_scan_errors_but_keep_404_empty() {
    run_dashboard_javascript_test(
        r#"
import assert from "node:assert/strict";
class Node {
  constructor() { this.children = []; this.dataset = {}; this.className = ""; this._text = ""; this.parentNode = null; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { if (child.parentNode) child.parentNode.removeChild(child); const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; }
  get textContent() { return this._text + this.children.map((child) => child.textContent).join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get firstChild() { return this.children[0] || null; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
}
const nodes = new Map();
const get = (id) => nodes.get(id) || (nodes.set(id, new Node()), nodes.get(id));
globalThis.document = { getElementById: get, createElement: () => new Node(), createDocumentFragment: () => new Node() };
globalThis.window = { location: new URL("http://dashboard.test/") };
let responseStatus = 413;
globalThis.fetch = async () => ({
  ok: responseStatus < 400,
  status: responseStatus,
  text: async () => JSON.stringify({ error: "run-events audit rows exceed bounded scan budget" }),
});
const { fetchJson } = await import("./common.js");
const { setActiveRunEvents, setActiveRunEventsError, renderRunEvents } = await import("./run-detail.js");
let scanError;
try {
  await fetchJson("/api/runs/jrun-1/events?limit=100");
} catch (error) {
  scanError = error;
}
assert.equal(scanError.status, 413);
assert.match(scanError.message, /bounded scan budget/);
setActiveRunEvents([]);
setActiveRunEventsError(scanError.message);
renderRunEvents();
assert.match(get("run-events-body").textContent, /bounded scan budget/);
assert.match(get("run-events-body").textContent, /narrowing the kind filter/);
responseStatus = 404;
await assert.rejects(fetchJson("/api/runs/jrun-1/events?limit=100"), (error) => error.status === 404);
setActiveRunEvents([]);
renderRunEvents();
assert.match(get("run-events-body").textContent, /No v2 envelope events for this run/);
"#,
    );
}

#[test]
fn dashboard_renders_complexity_as_its_own_dimension() {
    let diagnostics = include_str!("../../assets/dashboard/diagnostics.js");
    assert!(
        diagnostics.contains("Task completion by complexity"),
        "completion-by-complexity panel must exist"
    );
    assert!(
        diagnostics.contains("unset (unlabeled)"),
        "unset complexity must be a named bucket"
    );
    assert!(
        diagnostics.contains("Average implement_one duration by actor (30d) · ${label} · n="),
        "duration-by-actor must be faceted by complexity"
    );
    let app = include_str!("../../assets/dashboard/app.js");
    assert!(
        app.contains("/api/tasks/completion-by-complexity"),
        "completion aggregate must be fetched from the generated index"
    );
}

#[test]
fn dashboard_shipped_javascript_observes_history_routes_and_aggregate_requests() {
    run_dashboard_javascript_test(
        r#"
const nodes = [];
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = { setProperty: () => {} }; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.hidden = false; nodes.push(this); }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  replaceChildren(...next) { for (const child of this.children) child.parentNode = null; this.children = []; for (const child of next) this.appendChild(child); }
  prepend(child) { this.children.unshift(child); child.parentNode = this; }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter((child) => child !== this); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get firstChild() { return this.children[0] || null; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  get classList() { const self = this; return { add: (...c) => { self.className = `${self.className} ${c.join(" ")}`.trim(); }, remove: () => {}, toggle: (c, on) => { if (on) this.addClass(c); } }; }
  addClass(c) { if (!this.className.split(/\\s+/).includes(c)) this.className = `${this.className} ${c}`.trim(); }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  contains(node) { return this === node || this.children.includes(node); }
  focus() {}
  closest() { return null; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
const tabs = ["tasks", "audit", "diagnostics", "operations", "knowledge"].map((tab) => Object.assign(new Node(), { dataset: { tab } }));
const panes = [...tabs, Object.assign(new Node(), { dataset: { tab: "run-detail" } })];
globalThis.document = {
  body: new Node("body"), hidden: false,
  getElementById: get, createElement: () => new Node(), createElementNS: () => new Node(), createTextNode: (text) => Object.assign(new Node(), { textContent: text }), createDocumentFragment: () => new Node(),
  querySelectorAll: (selector) => selector === ".tab" ? tabs : selector === ".tab-pane" ? panes : [],
  querySelector: () => new Node(), addEventListener: () => {},
};
const location = new URL("http://dashboard.test/");
globalThis.window = { location, innerHeight: 900, addEventListener: () => {}, matchMedia: () => ({ addEventListener: () => {}, matches: false }), localStorage: { getItem: () => null, setItem: () => {} } };
globalThis.history = { replaceState: (_, __, url) => { location.href = String(url); } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.requestAnimationFrame = (fn) => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { constructor() {} close() {} };
const requests = [];
globalThis.fetch = async (path) => {
  const url = String(path); requests.push(url);
  const payload = url.startsWith("/api/workspaces") ? [{ id: "one", name: "one", status: "active", is_default: true }, { id: "two", name: "two", status: "active" }]
    : url.startsWith("/api/tasks?") || url === "/api/tasks" ? { items: [], total: 0, limit: 50, truncated: false } : [];
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const { renderTasks } = await import("./tasks.js");
const { initRouter, setActiveTab } = await import("./router.js");
const historyTask = { id: "ORB-11196", title: "history", status: "review", history: [
  { event: "status-1", at: "1", by: "a" }, { event: "status-2", at: "2", by: "b" }, { event: "status-3", at: "3", by: "c" }, { event: "status-4", at: "4", by: "d" }, { event: "status-5", at: "5", by: "e" }, { event: "status-6", at: "6", by: "f" }, { event: "commented", at: "7", by: "noise" }, { event: "commented", at: "8", by: "noise" },
] };
const taskContext = { getTasks: () => [historyTask], getSearchQuery: () => "", getActiveStatuses: () => new Set(["review"]), statusOrder: ["review"], statusUpdateTargets: [], fmtAbsTime: (value) => value, refreshDashboard: () => Promise.resolve() };
renderTasks([historyTask], taskContext);
const row = nodes.find((node) => node.className.includes("row") && node.listeners.click);
row.listeners.click();
const historyLines = nodes.filter((node) => node.className === "history-line").map((node) => node.textContent);
if (historyLines.length !== 5 || historyLines.some((line) => line.includes("commented")) || !historyLines[0].includes("status-6") || !historyLines[4].includes("status-2")) throw new Error(`recent history rendered incorrectly: ${historyLines}`);
let selected = null;
initRouter({ setTab: (tab) => { selected = tab; }, getDiagSubtab: () => "runs", setDiagSubtab: () => {}, getOperationsSubtab: () => "routines", setOperationsSubtab: () => {}, getKnowledgeSubtab: () => "frictions", setKnowledgeSubtab: () => {}, getRunId: () => null, setRunId: () => {}, getRunSubtab: () => "steps", setRunSubtab: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {}, setRunLogs: () => {}, refreshDashboard: () => {}, fitLogPanelToViewport: () => {}, });
setActiveTab("operations/auto-tasks", { refresh: false, updateHash: false });
if (selected !== "operations" || !tabs.find((tab) => tab.dataset.tab === "operations").className.includes("active")) throw new Error("route did not select the Operations view");
await import("./app.js");
await tick(); await tick(); requests.length = 0;
const selector = get("rail-workspace").children.find((child) => child.id === "workspace-select");
selector.value = ""; selector.listeners.change(); await tick(); await tick();
if (!requests.some((path) => path.startsWith("/api/tasks/all?status=")) || requests.some((path) => ["/api/crews", "/api/tasks/locks", "/api/audit/summary"].some((forbidden) => path.startsWith(forbidden)))) throw new Error(`aggregate mode made incorrect requests: ${requests}`);
"#,
    );
}

#[test]
fn dashboard_task_detail_shows_orchestrator_as_attribution_not_execution_crew() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(
        tasks.contains(r#"["orchestrator", "orchestrator"]"#),
        "task detail metadata must expose orchestration attribution"
    );
    assert!(
        tasks.contains("for (const [key, label] of TASK_META_FIELDS)"),
        "task detail must render the orchestrator metadata entry"
    );
    assert!(
        tasks.contains(r#"class: "task-crew-select mono""#),
        "execution crew must remain a distinct task-row control"
    );
}

/// ORB-10444/ORB-10875: the top-level nav includes the bounded Operations view.
/// A deprecated tab was retired outright — nav entry, route and pane — and
/// Scoreboard, being a diagnostics-shaped view, moved under Diagnostics. A route
/// left behind in `TABS` would resolve to a pane that no longer exists, so the
/// router's tab list is asserted alongside the markup.
#[tokio::test]
async fn dashboard_top_level_nav_matches_the_operator_tabs() {
    let body = response_body(serve_index().await).await;

    let nav: Vec<&str> = body
        .match_indices(r#"<button class="tab" data-tab=""#)
        .map(|(index, needle)| {
            let rest = &body[index + needle.len()..];
            match rest.find('"') {
                Some(end) => &rest[..end],
                None => panic!("unterminated data-tab attribute in the nav"),
            }
        })
        .collect();
    assert_eq!(
        nav,
        vec!["tasks", "audit", "diagnostics", "operations", "knowledge"]
    );

    // Every routable tab must still have a pane to render into.
    for tab in [
        "tasks",
        "audit",
        "diagnostics",
        "operations",
        "knowledge",
        "run-detail",
    ] {
        assert!(
            body.contains(&format!(r#"<section class="tab-pane" data-tab="{tab}">"#)),
            "routable tab `{tab}` must have a pane"
        );
    }
    assert!(
        !body.contains(r#"data-tab="scoreboard""#),
        "Scoreboard must no longer be a top-level tab or pane"
    );
}

#[test]
fn dashboard_operations_are_typed_guarded_and_responsive() {
    let index = include_str!("../../assets/dashboard/index.html");
    let operations = include_str!("../../assets/dashboard/operations.js");
    let router = include_str!("../../assets/dashboard/router.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    for id in [
        "routines-body",
        "clock-body",
        "routine-operation-feedback",
        "auto-tasks-body",
        "auto-task-operation-feedback",
        "operations-subtabs",
    ] {
        assert!(index.contains(&format!(r#"id="{id}""#)), "{id}");
    }
    assert!(operations.contains(r#"postJson("/api/routines/toggle""#));
    assert!(operations.contains(r#"postJson("/api/routines/clock""#));
    assert!(operations.contains(r#"postJson("/api/auto-tasks/toggle""#));
    assert!(operations.contains(r#"postJson("/api/auto-tasks/mint""#));
    assert!(operations.contains("pendingOperations.has(key)"));
    assert!(operations.contains("window.confirm("));
    assert!(operations.contains("All-workspace mode is read-only"));
    assert!(operations.contains("routine.target"));
    assert!(operations.contains("last_evaluated_slot"));
    assert!(operations.contains("next_tick_at"));
    assert!(operations.contains("Last scheduler evaluation"));
    assert!(operations.contains("hypothetical next"));
    assert!(operations.contains("Waiting for deliveries"));
    assert!(operations.contains("Never observed"));
    assert!(operations.contains("acknowledge_unconditional: true"));
    assert!(operations.contains("UNCONDITIONAL_MINT_WARNING"));
    assert!(operations.contains(
        "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy."
    ));
    assert!(
        operations.contains("An open instance already exists; this will create another open task.")
    );
    assert!(operations.contains("Minted") || operations.contains("result.message"));
    assert!(operations.contains("Auto-task change failed"));
    assert!(operations.contains("Manual mint failed"));
    assert!(operations.contains("\"/api/auto-tasks\""));
    assert!(
        !operations.contains("postJson(\"/api/auto-tasks")
            || operations.contains("addEventListener(\"click\"")
    );
    assert!(
        !operations.contains("hashchange") && !operations.contains("location.reload"),
        "refresh/back must not replay a toggle or mint POST"
    );
    assert!(
        router.contains(r#"const OPERATIONS_SUBTABS = ["routines", "auto-tasks", "auto-drain"];"#)
    );
    assert!(router.contains(r#"hash = `#operations/${sub}`;"#));
    assert!(css.contains("@media (max-width: 720px)"));
    assert!(css.contains("@media (max-width: 600px)"));
    assert!(css.contains(".operation-grid { grid-template-columns: 1fr; }"));
    assert!(css.contains("body.operations-active"));
    assert!(css.contains(".operation-mint-warning"));
    assert!(css.contains(".operation-row-head"));
    assert!(css.contains(".operation-details summary"));
    assert!(operations.contains("operation-row-head"));
    assert!(operations.contains(r#"{ class: "operation-details" }"#));
    assert!(router.contains(r#"classList.toggle("operations-active", top === "operations")"#));
}

/// ORB-11559: below 760px the 216px rail must give up the content column so
/// Tasks can use the 520px two-row grid at phone widths, and every top-level
/// tab plus diagnostics subtab stays in the (now horizontal) nav.
#[test]
fn dashboard_narrow_shell_collapses_rail_and_task_rows() {
    let css = include_str!("../../assets/dashboard/dashboard.css");
    let index = include_str!("../../assets/dashboard/index.html");

    let narrow = css
        .split("@media (max-width: 760px)")
        .skip(1)
        .find(|block| {
            block.contains(".shell {") && block.contains("grid-template-columns: minmax(0, 1fr);")
        })
        .expect("760px must collapse .shell to a single column");
    assert!(
        narrow.contains(".rail-group { display: contents; }"),
        "rail groups must unwrap so tabs and diagnostics subtabs can reflow"
    );
    assert!(
        narrow.contains("flex: 1 1 100%"),
        "diagnostics subtabs must wrap onto a second row"
    );
    assert!(
        narrow.contains(".kpi .k { display: none; }")
            && narrow.contains(".kpi-spark { display: none; }"),
        "KPI labels and the sparkline must collapse at the same width as the rail"
    );
    assert!(
        css.contains(
            "grid-template-areas:\n            \"id title\"\n            \"status crew\";"
        ),
        "the 520px task row must keep its two-row areas for phone widths"
    );

    for tab in ["tasks", "audit", "diagnostics", "operations", "knowledge"] {
        assert!(
            index.contains(&format!(r#"class="tab" data-tab="{tab}""#)),
            "{tab} must remain a top-level tab"
        );
    }
    for subtab in [
        "runs",
        "metrics",
        "errors",
        "incidents",
        "reliability",
        "scoreboard",
    ] {
        assert!(
            index.contains(&format!(r#"data-subtab="{subtab}""#)),
            "{subtab} must remain a reachable diagnostics subtab"
        );
    }
}

/// ORB-11558: disabled/paused rows must not look scheduled; clock cadence is a
/// duration; timestamps name a timezone, including PST/PDT across DST.
#[test]
fn dashboard_operations_label_paused_schedules_timezones_and_clock_units() {
    run_dashboard_javascript_test(
        r#"
process.env.TZ = "America/Los_Angeles";
const nodes = [];
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.hidden = false; this.disabled = false; this.value = ""; nodes.push(this); }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  append(...children) { for (const child of children) this.appendChild(child); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  insertBefore(child, before) { const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); return child; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
for (const id of ["routines-body", "clock-body", "auto-tasks-body", "auto-drain-body", "operation-mode-body", "routines-count", "clock-host", "auto-tasks-count", "auto-drain-count", "operation-mode-count", "operations-session", "routine-operation-feedback", "clock-operation-feedback", "auto-task-operation-feedback", "auto-drain-operation-feedback", "operation-mode-operation-feedback"]) get(id);
globalThis.document = { getElementById: get, createElement: () => new Node(), createTextNode: (text) => Object.assign(new Node(), { textContent: text }), body: new Node("body") };
globalThis.window = { confirm: () => true, location: new URL("http://dashboard.test/"), addEventListener: () => {}, localStorage: { getItem: () => null, setItem: () => {} } };
const pad = (n) => String(n).padStart(2, "0");
const formatAbsoluteTime = (value) => {
  const d = new Date(value);
  if (Number.isNaN(d.getTime())) return String(value);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
};
const routines = {
  host_id: "host-1",
  session_explanation: "test",
  capabilities: {},
  clock: {
    health: "healthy", provider: "systemd", enabled: true, loaded: true, running: true, schedulable: true,
    configured_cadence_seconds: 300, effective_cadence_seconds: 300,
    last_tick_at: "2026-09-07T21:00:00Z", next_tick_at: "2026-09-07T21:05:00Z",
  },
  routines: [
    { name: "ship-sweep-orbit", source: "one", target: "job:ship", enabled: false, effective: false, cron: "30 14 * * *", hosts: ["host-1"], pinned_to_host: true, next_due: "2026-09-07T21:30:00Z", next_evaluation: { state: "disabled", at: "2026-09-07T21:30:00Z", hypothetical: true }, last_fire: null },
    { name: "paused-nightly", source: "one", target: "job:nightly", enabled: true, effective: false, paused_at: "2026-09-07T20:00:00Z", cron: "0 2 * * *", hosts: ["host-1"], pinned_to_host: true, next_due: "2026-09-08T09:00:00Z", next_evaluation: { state: "paused", at: "2026-09-08T09:00:00Z", hypothetical: true }, last_fire: null },
    { name: "delivery-cover", source: "one", target: "job:cover", enabled: true, effective: true, trigger: { deliveries_landed: { threshold: 3, branch: "agent-main" } }, hosts: ["host-1"], pinned_to_host: true, next_evaluation: { state: "waiting", at: null, hypothetical: false }, last_fire: null },
  ],
};
const autoTasks = {
  unconditional_mint_warning: "Manual mint ignores this definition's schedule, enabled flag, and scheduler dedupe policy.",
  capabilities: { auto_task_toggle: { authorized: true }, auto_task_mint: { authorized: true } },
  definitions: [
    { name: "ci-failure-remediation", enabled: false, schedule_summary: "every 15 minutes", template_summary: "[auto-task] remediate", next_evaluation: { state: "disabled", at: "2026-09-07T21:15:00Z", hypothetical: true }, last_evaluation: null, last_minted_task_id: null },
    { name: "hourly", enabled: true, schedule_summary: "every 60 minutes", last_evaluation: { kind: "fired", last_task_id: "ORB-00001", last_fired_at: "2026-09-07T20:00:00Z" }, last_minted_task_id: "ORB-00099", last_minted_task_status: "backlog", next_evaluation: { state: "scheduled", at: "2026-09-07T22:00:00Z", hypothetical: false } },
    { name: "fresh", enabled: true, schedule_summary: "every 60 minutes", last_evaluation: null, last_minted_task_id: null, next_evaluation: { state: "never_observed", at: null, hypothetical: false } },
    { name: "broken-cover", enabled: true, schedule_summary: "3 deliveries on agent-main", next_evaluation: { state: "unavailable", at: null, hypothetical: false } },
  ],
};
globalThis.fetch = async (path) => {
  const url = String(path);
  const payload = url.startsWith("/api/routines") ? routines
    : url.startsWith("/api/auto-tasks") ? autoTasks
    : {};
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};
const { setWorkspace } = await import("./common.js");
const { initOperations, fetchAndRenderOperations } = await import("./operations.js");
setWorkspace("one");
initOperations({ getWorkspaces: () => [{ id: "one", name: "one", status: "active" }], formatAbsoluteTime });
await fetchAndRenderOperations();
const routineText = get("routines-body").textContent;
const autoText = get("auto-tasks-body").textContent;
const clockText = get("clock-body").textContent;
for (const expected of ["Disabled · hypothetical next", "Paused · hypothetical next", "Waiting for deliveries"]) {
  if (!routineText.includes(expected)) throw new Error(`routines missing ${JSON.stringify(expected)} in: ${routineText}`);
}
if (routineText.includes("Next evaluation2026-09-07") && !routineText.includes("hypothetical")) {
  throw new Error(`unqualified next evaluation in: ${routineText}`);
}
for (const expected of ["Disabled · hypothetical next", "Never observed", "Unavailable", "Last scheduler evaluation", "manual mint"]) {
  if (!autoText.includes(expected)) throw new Error(`auto-tasks missing ${JSON.stringify(expected)} in: ${autoText}`);
}
if (!clockText.includes("every 5 minutes (300s)")) throw new Error(`cadence should be a duration, got: ${clockText}`);
const tzName = (iso) => new Intl.DateTimeFormat("en-US", { timeZoneName: "short" }).formatToParts(new Date(iso)).find((part) => part.type === "timeZoneName")?.value;
if (tzName("2026-01-15T20:00:00Z") !== "PST") throw new Error(`expected PST in January, got ${tzName("2026-01-15T20:00:00Z")}`);
if (tzName("2026-07-15T19:00:00Z") !== "PDT") throw new Error(`expected PDT in July, got ${tzName("2026-07-15T19:00:00Z")}`);
if (!routineText.includes("14:30 PDT") || !routineText.includes("hypothetical")) {
  throw new Error(`disabled 14:30 must be labeled PDT and hypothetical: ${routineText}`);
}
if (!clockText.includes("14:00 PDT") && !clockText.includes("14:05 PDT")) {
  throw new Error(`clock last/next tick must be absolute local times with a timezone: ${clockText}`);
}
"#,
    );
}

/// ORB-11250: the bounded auto-delivery window action. Default completion
/// (review) needs no operator authorization, the same as the ship endpoint;
/// only the `--complete`-equivalent opt-in is separately governed. The panel
/// reuses the mint/clock in-flight idiom — one fixed `pendingOperations` key,
/// guard released in `finally` — rather than a per-row guard, since this is a
/// single workspace-scoped action, not one per task.
#[test]
fn dashboard_auto_drain_action_is_bounded_governed_and_guarded() {
    let index = include_str!("../../assets/dashboard/index.html");
    let operations = include_str!("../../assets/dashboard/operations.js");
    let router = include_str!("../../assets/dashboard/router.js");

    for id in [
        "operations-auto-drain-main",
        "auto-drain-panel",
        "auto-drain-count",
        "auto-drain-operation-feedback",
        "auto-drain-body",
    ] {
        assert!(index.contains(&format!(r#"id="{id}""#)), "{id}");
    }
    assert!(
        index.contains(r#"<button class="subtab" data-subtab="auto-drain" type="button">"#),
        "auto-drain must be offered as an Operations subtab"
    );
    assert!(
        router.contains(r#"const autoDrain = $("operations-auto-drain-main");"#)
            && router.contains(r#"autoDrain.hidden = name !== "auto-drain";"#),
        "the router must toggle the auto-drain main like its siblings"
    );

    assert!(
        operations.contains(r#"postJson("/api/workflows/auto""#),
        "starting the window must submit through the dashboard auto-drain endpoint"
    );
    assert!(
        operations.contains(r#"`/api/workflows/auto/readiness"#),
        "the panel must project the read-only readiness snapshot, not recompute eligibility"
    );
    assert!(
        operations.contains("for_duration: autoDrainDuration"),
        "the submitted duration must come from the bounded picker, not free text"
    );
    assert!(
        operations.contains("complete: autoDrainComplete"),
        "the completion opt-in must be explicit, not inferred"
    );

    // Duplicate-click guard: same fixed-key idiom as the clock/mint buttons.
    assert!(operations.contains(r#"const key = "auto-drain:start";"#));
    assert!(
        operations.contains("if (pendingOperations.has(key)) return;")
            && operations.contains("pendingOperations.add(key);")
            && operations.contains("pendingOperations.delete(key);"),
        "the start action must guard against a duplicate submission while one is pending"
    );

    // Explicit opt-in requires confirmation and states the run's scope.
    assert!(operations.contains("window.confirm(confirmText)"));
    assert!(
        operations.contains("Currently eligible: ${counts.eligible} · waiting: ${counts.waiting}")
    );
    assert!(
        operations.contains("Proposed tasks are never drained automatically"),
        "the panel must explain proposed tasks require separate authorization"
    );

    // Failure recovery: an error must surface, not silently no-op, and must
    // not leave the guard held.
    assert!(operations.contains("Auto-delivery window failed to start"));
}

/// The global `main` rule establishes the visible grid while this narrow
/// Operations selector must win for an inactive HTML-hidden subview. Keep the
/// tiny cascade model here rather than checking only for markup or a `hidden`
/// attribute: the regression was precisely that the inactive main was still
/// rendered after a display rule won the cascade.
fn computed_operations_main_display(css: &str, hidden: bool, viewport_width: u16) -> &'static str {
    let global_main_display = css.contains("main {\n        display: grid;");
    let compact_main_display =
        viewport_width <= 1000 && css.contains("main { grid-template-columns: 1fr !important; }");
    let hidden_override = css.contains(
        ".tab-pane[data-tab=\"operations\"] > main[hidden] { display: none !important; }",
    );

    if hidden && hidden_override {
        "none"
    } else if global_main_display || compact_main_display {
        "grid"
    } else {
        "block"
    }
}

#[test]
fn dashboard_operations_subtabs_compute_exactly_one_rendered_main() {
    let css = include_str!("../../assets/dashboard/dashboard.css");

    for viewport_width in [1280, 720, 480] {
        for (route, hidden_states) in [("routines", [false, true]), ("auto-tasks", [true, false])] {
            let visible = hidden_states
                .into_iter()
                .filter(|hidden| {
                    computed_operations_main_display(css, *hidden, viewport_width) != "none"
                })
                .count();
            assert_eq!(
                visible, 1,
                "#{route} must render exactly one Operations subview at {viewport_width}px"
            );
        }
    }
}

/// ORB-10444: Scoreboard content stays reachable after the move — as a
/// Diagnostics subtab whose markup (and therefore every id `scoreboard.js`
/// renders into, so the scoreboard API contract is untouched) lives inside the
/// diagnostics pane.
#[tokio::test]
async fn dashboard_scoreboard_is_reachable_under_diagnostics() {
    let body = response_body(serve_index().await).await;
    let router = include_str!("../../assets/dashboard/router.js");
    let app = include_str!("../../assets/dashboard/app.js");

    let diagnostics_at = body
        .find(r#"<section class="tab-pane" data-tab="diagnostics">"#)
        .expect("diagnostics pane");
    let scoreboard_at = body
        .find(r#"id="diagnostics-scoreboard-main""#)
        .expect("scoreboard host inside diagnostics");
    assert!(
        diagnostics_at < scoreboard_at,
        "the scoreboard markup must live inside the diagnostics pane"
    );
    assert!(
        body.contains(r#"<button class="subtab" data-subtab="scoreboard" type="button">"#),
        "Scoreboard must be offered as a diagnostics subtab"
    );
    // The panels scoreboard.js renders into came across unchanged.
    for id in [
        "scoreboard-body",
        "scoreboard-count",
        "scoreboard-window-selector",
        "scoreboard-narrative",
        "scoreboard-agent-strip",
        "scoreboard-insights",
        "scoreboard-orchestration",
        "scoreboard-orchestration-count",
        "scoreboard-highlights",
    ] {
        assert!(body.contains(&format!(r#"id="{id}""#)), "{id} must survive");
    }
    // ORB-10588 appended `reliability` to the same list.
    assert!(
        router.contains(
            r#"const DIAG_SUBTABS = ["runs", "metrics", "errors", "incidents", "reliability", "scoreboard"];"#
        ),
        "the scoreboard must route as a diagnostics subtab"
    );
    assert!(
        app.contains(r#"if (activeDiagSubtab === "scoreboard")"#)
            && app.contains(
                r#"fetchJson(`/api/scoreboard?window=${encodeURIComponent(selectedWindow)}`)"#
            ),
        "the scoreboard fetch must hang off the diagnostics subtab branch and honor the shared window"
    );
}

/// ORB-10588: the reliability view routes as a diagnostics subtab and owns the
/// ids `reliability.js` renders into.
#[tokio::test]
async fn dashboard_reliability_is_reachable_under_diagnostics() {
    let body = response_body(serve_index().await).await;
    let router = include_str!("../../assets/dashboard/router.js");
    let app = include_str!("../../assets/dashboard/app.js");

    let diagnostics_at = body
        .find(r#"<section class="tab-pane" data-tab="diagnostics">"#)
        .expect("diagnostics pane");
    let reliability_at = body
        .find(r#"id="diagnostics-reliability-main""#)
        .expect("reliability host inside diagnostics");
    assert!(
        diagnostics_at < reliability_at,
        "the reliability markup must live inside the diagnostics pane"
    );
    assert!(
        body.contains(r#"<button class="subtab" data-subtab="reliability" type="button">"#),
        "Reliability must be offered as a diagnostics subtab"
    );
    for id in [
        "reliability-count",
        "reliability-window-selector",
        "reliability-meta",
        "reliability-summary",
        "reliability-denominator-note",
        "reliability-truncation-note",
        "reliability-over-time",
        "reliability-breakdown",
        "reliability-activities",
    ] {
        assert!(body.contains(&format!(r#"id="{id}""#)), "{id} must exist");
    }
    assert!(
        app.contains(r#"if (activeDiagSubtab === "reliability")"#)
            && app.contains("fetchAndRenderReliability()"),
        "the reliability fetch must hang off the diagnostics subtab branch"
    );
    assert!(
        router.contains(r#"reliability: "diagnostics-reliability-main""#),
        "the reliability subtab must claim its own full-width main"
    );
}

/// ORB-10588: a rate is only actionable with its `n` and its window, and a
/// denominator too thin to trust must be withheld rather than rounded. Both
/// rules live in `reliability.js`; this pins them so a later edit cannot
/// quietly turn a withheld cell back into a confident percentage.
#[test]
fn dashboard_reliability_never_renders_a_rate_without_its_denominator() {
    let reliability = include_str!("../../assets/dashboard/reliability.js");
    let index = include_str!("../../assets/dashboard/index.html");

    assert!(
        reliability.contains("rate.low_sample"),
        "the low-sample flag from the API must be honored"
    );
    assert!(
        reliability.contains("n too small"),
        "a withheld rate must say why it is withheld"
    );
    assert!(
        reliability.contains("rel-rate-low"),
        "a withheld rate must be visually distinct from a real one"
    );
    assert!(
        reliability.contains("(n=${n})"),
        "a rendered percentage must carry its denominator"
    );
    assert!(
        reliability.contains("denominator_label"),
        "the denominator's meaning must be rendered, not left in the backend"
    );
    // `all` would be a rate with no stated range; the endpoint refuses it and
    // the selector must not offer it.
    assert!(
        !reliability.contains(r#""all""#),
        "an unbounded window must not be offered"
    );
    assert!(
        !index.contains(
            r#"id="reliability-window-selector" title="window scope">
                <span class="scoreboard-window-seg" data-window="all">"#
        ),
        "the reliability window selector must not offer `all`"
    );
}

/// ORB-10588: the recovery rate must be computed from durable run state only.
/// Friction F-token-disagreement (recorded in the task) makes any token- or
/// cost-derived input untrustworthy, so the reliability path must not read one.
#[test]
fn dashboard_reliability_reads_no_token_or_cost_field() {
    let reliability = include_str!("../../assets/dashboard/reliability.js");
    // Field identifiers, not the words: the module's own header explains *why*
    // it avoids these inputs, so a bare "token" match would flag the rationale.
    for banned in [
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "cache_read_tokens",
        "cache_create_tokens",
        "provider_cost_usd",
        "derived_cost_usd",
        "total_tool_calls",
    ] {
        assert!(
            !reliability.contains(banned),
            "reliability.js must not read `{banned}` — the token/cost inputs disagree across stores"
        );
    }
}

#[test]
fn dashboard_scoreboard_keeps_managed_cost_ownership_out_of_executor_rankings() {
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let index = include_str!("../../assets/dashboard/index.html");

    assert!(index.contains("Managed Execution Cost"));
    assert!(scoreboard.contains("renderOrchestrationSummary(summary?.orchestration)"));
    assert!(scoreboard.contains("named orchestrator"));
    assert!(scoreboard.contains("shared task ownership"));
    assert!(scoreboard.contains("unattributed task ownership"));
    assert!(scoreboard.contains("missing linked task"));
    assert!(scoreboard.contains("provider-reported"));
    assert!(scoreboard.contains("Provider-first estimate policy"));
    assert!(scoreboard.contains("derived estimate"));
    assert!(scoreboard.contains("if (known === 0)"));
    assert!(scoreboard.contains("formatUsd(total)"));
    assert!(scoreboard.contains("comparable same-invocation population"));
    assert!(scoreboard.contains("do not reconcile partial sums"));
    assert!(
        scoreboard.contains(
            "Direct interactive Codex or Claude orchestration-session overhead is excluded"
        )
    );
    assert!(scoreboard.contains("invocation < ${until} (exclusive cutoff"));
}

#[test]
fn dashboard_managed_execution_cost_panel_has_responsive_presentation_hooks() {
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        scoreboard.contains("scoreboard-orchestration-context")
            && scoreboard.contains("scoreboard-orchestration-buckets")
            && scoreboard.contains("scoreboard-orchestration-bucket-head"),
        "scope metadata and ownership buckets need separate presentation groups"
    );
    assert!(
        scoreboard.contains("cost-value")
            && scoreboard.contains("cost-coverage")
            && scoreboard.contains("cost-comparison"),
        "cost amount, coverage, and comparison text need independent styling hooks"
    );
    assert!(
        scoreboard.contains("scoreboard-orchestration-cost primary")
            && scoreboard.contains("\"reported\",\n      true,"),
        "provider-reported cost must receive the primary visual treatment"
    );
    for kind in ["orchestrator", "shared", "unattributed", "missing"] {
        assert!(
            css.contains(&format!(".scoreboard-orchestration-bucket.kind-{kind}")),
            "the {kind} ownership bucket needs a distinct theme-variable accent"
        );
    }
    assert!(
        css.contains("#scoreboard-orchestration-panel {\n        align-self: start;")
            && css.contains("grid-template-columns: repeat(2, minmax(0, 1fr));"),
        "the desktop panel must stay content-height and use a compact bucket grid"
    );
    assert!(
        css.contains("@media (max-width: 900px)")
            && css.contains("@media (max-width: 620px)")
            && css.contains("overflow-wrap: anywhere;"),
        "the managed-cost layout must collapse and wrap safely at narrow widths"
    );
}

/// ORB-10444: the desktop friction pane stays in view mid-read. ORB-11136:
/// once Knowledge collapses to one column, detail expands under its owning row
/// instead of being stranded after the full list.
#[test]
fn dashboard_knowledge_detail_is_sticky_on_desktop_and_inline_when_narrow() {
    let app = include_str!("../../assets/dashboard/app.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    let sticky_at = css
        .find("#friction-detail-panel {\n        position: sticky;")
        .expect("the friction detail panel must be sticky");
    assert!(
        css[sticky_at..].starts_with(
            "#friction-detail-panel {\n        position: sticky;\n        top: 170px;\n        align-self: start;\n        max-height: calc(100vh - 194px);",
        ),
        "the pane must pin below the chrome and stay inside the viewport"
    );
    assert!(
        css.contains("#friction-detail-panel > .body {\n        overflow-y: auto;",),
        "detail content taller than the pane must scroll inside it, not be clipped"
    );
    assert!(
        !css.contains("min-height: calc(100vh - 360px)"),
        "the old fixed min-height fought the bounded sticky pane and must be gone"
    );
    assert!(
        css.contains(".friction-stats .tile {\n        padding: 8px 16px;"),
        "friction summary tiles need outer breathing room at every width"
    );
    let accordion_at = css
        .find("          display: none;\n        }\n        .friction-row-toggle")
        .expect("the narrow breakpoint must hide the separate detail pane");
    assert!(sticky_at < accordion_at);
    assert!(
        app.contains(r#"const FRICTION_ACCORDION_QUERY = "(max-width: 1000px)";"#)
            && app.contains(r#"row.setAttribute("aria-expanded", String(expanded));"#)
            && app.contains(r#"if (event.key !== "Enter" && event.key !== " ") return;"#)
            && app.contains("frag.appendChild(inlineDetail);")
            && app.contains("frictionAccordionMedia.addEventListener(\"change\"")
            && css.contains(".friction-accordion-detail .knowledge-detail-body")
            && css.contains("@media (max-width: 1400px) {\n        .knowledge-detail-body"),
        "narrow friction rows must expose a keyboard-operable inline accordion that tracks viewport changes"
    );
}

#[test]
fn dashboard_friction_list_defaults_to_active_and_filters_by_status() {
    let index = include_str!("../../assets/dashboard/index.html");
    let app = include_str!("../../assets/dashboard/app.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        index.contains(r#"<label class="friction-filter-control" for="friction-status-filter">"#)
            && index
                .contains(r#"<select id="friction-status-filter" aria-controls="frictions-body">"#),
        "the status filter must have a visible label and name the list it controls"
    );
    for option in ["active", "open", "triaged", "resolved", "all"] {
        assert!(
            index.contains(&format!(r#"<option value="{option}""#)),
            "the friction status filter must expose {option}"
        );
    }
    assert!(
        app.contains(r#"const DEFAULT_FRICTION_STATUS_FILTER = "active";"#)
            && app.contains(
                r#"frictionStatusFilter === "active" ? ["open", "triaged"] : [frictionStatusFilter]"#,
            ),
        "the initial list must fetch open and triaged independently so resolved history cannot consume its limit"
    );
    assert!(
        app.contains(r#"if (status !== "all") sp.set("status", status);"#)
            && app.contains(r#"if (frictionSearchQuery) sp.set("q", frictionSearchQuery);"#),
        "status and text search must compose in every list request"
    );
    assert!(
        app.contains("activeFrictionId = null;") && app.contains(".slice(0, FRICTION_LIMIT);"),
        "filter changes must reset stale selection and the merged active view must honor the shared limit"
    );
    assert!(
        css.contains("#friction-status-filter:focus-visible")
            && css.contains(".friction-filter-control { flex: 1 1 100%; }"),
        "the filter needs visible keyboard focus and a narrow-screen layout"
    );
}

/// ORB-10444: the Tasks tab's two write actions. Ship is one click — the
/// dispatch carries the task id alone, so the pipeline resolves the crew from
/// the task and the mode from the workspace — and comments post to the
/// task's review-thread endpoint rather than patching the task record.
#[test]
fn dashboard_task_write_actions_are_configuration_free() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(
        tasks.contains(r#"const SHIP_STATUSES = new Set(["backlog"]);"#),
        "Ship must be offered only on backlog tasks"
    );
    assert!(
        tasks.contains(r#"postJson("/api/workflows/ship", { task_ids: [task.id] })"#),
        "Ship must dispatch the task id with no crew or mode override"
    );
    assert!(
        tasks.contains("taskActionNotice = `${task.id}: ship run ${runId} ${state}`"),
        "the resulting run must be surfaced to the operator"
    );
    assert!(
        tasks.contains(r#"text: `ship failed: ${error.message || String(error)}`"#),
        "a failed dispatch must surface the server error, not silently no-op"
    );
    // A second click must not launch a duplicate run: the guard is taken before
    // the request and released only when the dispatch failed.
    assert!(
        tasks.contains("if (shipInFlightTaskIds.has(task.id)) return;")
            && tasks.contains("shipInFlightTaskIds.add(task.id);"),
        "Ship must guard against a duplicate dispatch from the UI side"
    );
    assert_eq!(
        tasks
            .matches("shipInFlightTaskIds.delete(task.id);")
            .count(),
        1,
        "the in-flight guard may be released on the failure path only"
    );

    assert!(
        tasks.contains(
            r#"postJson(`/api/tasks/${encodeURIComponent(task.id)}/comments`, { message })"#
        ),
        "comments must post to the task's review-thread endpoint"
    );
    assert!(
        !tasks.contains("author:"),
        "the dashboard must not name the comment author; the server records the human identity"
    );
}

/// ORB-10874: the Tasks count previously read an ambiguous `N/50` with no way
/// to tell a total from a page size from a hard cap. It must now state which
/// number means what, using the `/api/tasks` paging envelope
/// (`{ items, total, limit, truncated, offset, next_cursor }`) when available.
#[test]
fn dashboard_task_count_states_page_range_and_total_explicitly() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(
        tasks.contains("export function formatTaskCount("),
        "the count formatter must be a standalone, testable function"
    );
    assert!(
        tasks.contains("offset + 1") && tasks.contains("offset + fetchedCount"),
        "the formatter must expose the selected page's exact range"
    );
    assert!(
        !tasks.contains("filtered.length}/${tasks.length}"),
        "the old ambiguous `N/M` shorthand must be gone"
    );
    assert!(
        tasks.contains("$(\"tasks-count\").textContent = formatTaskCount("),
        "the rendered count must go through the explicit formatter"
    );
}

#[test]
fn dashboard_task_pagination_is_accessible_responsive_and_race_safe() {
    let index = include_str!("../../assets/dashboard/index.html");
    let css = include_str!("../../assets/dashboard/dashboard.css");
    let app = include_str!("../../assets/dashboard/app.js");
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(
        index.contains(r#"<nav class="task-pagination" aria-label="Task pages">"#)
            && index.contains(r#"id="tasks-previous" type="button""#)
            && index.contains(r#"id="tasks-next" type="button""#)
            && index.contains(r#"id="tasks-page-status" role="status" aria-live="polite""#),
        "visible page controls and live loading/error status must use native accessible markup"
    );
    assert!(
        css.contains(".task-pagination") && css.contains("@media (max-width: 520px)"),
        "pagination must retain a narrow-screen layout"
    );
    assert!(
        app.contains("const sequence = ++taskFetchSequence")
            && app.contains("sequence === taskFetchSequence")
            && app.contains("taskPreviousCursors.push(taskPageCursor)"),
        "navigation must retain a previous stack and reject stale page responses"
    );
    assert!(
        tasks.contains("export function renderTaskPagination(")
            && tasks.contains("context.resetTaskPagination()"),
        "filter navigation must reset page state before rendering"
    );
}

/// ORB-10874: the status chips and search box are represented in the tasks
/// hash so a reload or the browser's back/forward button restores the same
/// filtered view, mirroring the audit tab's existing buildAuditHash /
/// applyAuditHashQuery pair. A visible summary line states the active filter
/// in words, not just via chip color.
#[test]
fn dashboard_task_filters_are_represented_in_the_url_and_summarized() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");
    let router = include_str!("../../assets/dashboard/router.js");
    let index = include_str!("../../assets/dashboard/index.html");

    assert!(
        tasks.contains("export function buildTasksHash(")
            && tasks.contains("export function applyTasksHashQuery(")
            && tasks.contains("export function syncTaskControls("),
        "tasks.js must expose a hash build/apply/sync trio like audit.js does"
    );
    assert!(
        router.contains("ctx.applyTasksHashQuery(query)")
            && router.contains("ctx.buildTasksHash()"),
        "the router must apply and rebuild the tasks hash on every tasks-tab route"
    );
    assert!(
        tasks.contains("function renderFilterSummary("),
        "the active filter must be restated as text, not only via chip color"
    );
    assert!(
        index.contains(r#"id="task-filter-summary""#) && index.contains(r#"aria-live="polite""#),
        "the filter summary element must exist and announce updates to assistive tech"
    );
}

/// ORB-10942: an explicit all-status selection must survive the hash round
/// trip instead of becoming plain `#tasks`, whose omitted status query means
/// the default set without `someday`. The four cases below are asserted as a
/// deterministic source contract because the dashboard has no JS test runner.
#[test]
fn dashboard_task_filter_hash_round_trips_default_all_someday_and_none() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");
    let app = include_str!("../../assets/dashboard/app.js");

    assert!(
        tasks.contains(r#"sp.set("status", "all")"#) && tasks.contains(r#"statusParam === "all""#),
        "all statuses need a distinct hash representation and matching parser branch"
    );
    assert!(
        tasks.contains(r#"sp.set("status", selected.length > 0 ? selected.join(",") : "none")"#)
            && tasks.contains(r#"statusParam === "none""#),
        "partial and empty selections need stable hash representations"
    );
    assert!(
        tasks.contains("setActiveStatuses(context, new Set(defaultActiveStatuses(context)))"),
        "an omitted status query must retain the documented default set"
    );

    for (label, hash_query, parser_marker) in [
        ("default", "#tasks", "statusParam == null"),
        ("all", "#tasks?status=all", "statusParam === \"all\""),
        (
            "someday-only",
            "#tasks?status=someday",
            "statusParam === \"none\"",
        ),
        (
            "none-selected",
            "#tasks?status=none",
            "statusParam === \"none\"",
        ),
    ] {
        assert!(!hash_query.is_empty(), "{label} hash must be deterministic");
        assert!(
            tasks.contains(parser_marker),
            "{label} parser branch must remain present"
        );
    }
    assert!(
        app.contains("activeStatuses.size > 0 && activeStatuses.size < STATUS_ORDER.length"),
        "single-workspace requests must send only partial active status sets"
    );
}

#[test]
fn dashboard_renders_every_other_status_for_a_done_task() {
    let app = include_str!("../../assets/dashboard/app.js");
    for status in [
        "in-progress",
        "review",
        "blocked",
        "proposed",
        "backlog",
        "someday",
        "done",
        "rejected",
        "archived",
    ] {
        assert!(
            app.contains(&format!("\"{status}\"")),
            "dashboard status catalog must include {status}"
        );
    }

    run_dashboard_javascript_test(
        r#"
class Node {
  constructor() { this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const old = child.parentNode; if (old) old.children = old.children.filter((candidate) => candidate !== child); const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  get classList() { return { add: (...names) => { this.className = `${this.className} ${names.join(" ")}`.trim(); } }; }
}
const nodes = new Map();
const get = (id) => nodes.get(id) || (nodes.set(id, new Node()), nodes.get(id));
globalThis.document = {
  getElementById: get,
  createElement: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
const location = new URL("http://dashboard.test/#tasks");
globalThis.window = { location, addEventListener: () => {}, confirm: () => false };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.setTimeout = () => 0;

const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const task = { id: "ORB-1", title: "Done task", status: "done", history: [], artifacts: [] };
const { renderTasks } = await import("./tasks.js");
renderTasks([task], {
  getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["done"]), statusOrder: statuses,
  statusUpdateTargets: statuses, fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
});

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
const select = find(get("tasks-body"), (node) => node.className === "task-status-select mono");
if (!select) throw new Error("status select did not render");
const values = select.children.map((option) => option.value).filter(Boolean);
const expected = statuses.filter((status) => status !== "done");
if (JSON.stringify(values) !== JSON.stringify(expected)) throw new Error(`status options ${JSON.stringify(values)} != ${JSON.stringify(expected)}`);
"#,
    );
}

/// ORB-11655: the Tasks detail node is diffed on the whole task object, so any
/// field change (an agent bumping `updated_at`) rebuilt it — discarding a
/// comment the operator was still typing inside it.
#[test]
fn dashboard_task_refresh_keeps_a_half_written_comment() {
    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(tag = "") { this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const old = child.parentNode; if (old) old.children = old.children.filter((candidate) => candidate !== child); const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  replaceChildren(...next) { for (const child of this.children) child.parentNode = null; this.children = []; for (const child of next) this.appendChild(child); }
  replaceWith(next) { const parent = this.parentNode; if (!parent) return; parent.children = parent.children.map((candidate) => candidate === this ? next : candidate); next.parentNode = parent; this.parentNode = null; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  focus() {}
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  get classList() { return { add: (...names) => { this.className = `${this.className} ${names.join(" ")}`.trim(); } }; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node()), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
};
globalThis.window = { location: new URL("http://dashboard.test/#tasks"), addEventListener: () => {}, confirm: () => false };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.setTimeout = () => 0;

const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const task = { id: "ORB-1", title: "Reviewable", status: "review", updated_at: "2026-09-08T01:00:00Z", history: [], artifacts: [] };
const context = {
  getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["review"]), statusOrder: statuses,
  statusUpdateTargets: statuses, fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
};
const { renderTasks } = await import("./tasks.js");

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
const body = get("tasks-body");
const detail = () => find(body, (node) => node.dataset.key === "detail-ORB-1");
const tick = () => { task.updated_at = `${task.updated_at}+`; renderTasks([task], context); };

renderTasks([task], context);
find(body, (node) => node.dataset.key === "task-ORB-1").listeners.click();

// Control: with no draft open, a task field change does rebuild the detail.
const before = detail();
if (!before) throw new Error("expanding the task did not render its detail");
tick();
if (detail() === before) throw new Error("a changed task must still rebuild its detail");

const held = detail();
find(held, (node) => node.className === "action comment").listeners.click({ stopPropagation: () => {} });
const textarea = find(held, (node) => node.tag === "textarea");
if (!textarea) throw new Error("the comment form did not render a textarea");
textarea.value = "half written";

tick();
if (detail() !== held) throw new Error("the refresh replaced a detail holding an open comment form");
const live = find(body, (node) => node.tag === "textarea");
if (live !== textarea) throw new Error("the refresh replaced the textarea the operator was typing in");
if (live.value !== "half written") throw new Error(`the draft text was lost: ${JSON.stringify(live.value)}`);

// Closing the form hands the detail back to the data: the next tick rebuilds it.
find(held, (node) => node.className === "action cancel").listeners.click({ stopPropagation: () => {} });
tick();
if (detail() === held) throw new Error("the detail stayed frozen after the draft was cancelled");
if (find(body, (node) => node.tag === "textarea")) throw new Error("the cancelled comment form is still rendered");
"#,
    );
}

/// ORB-11655: the Audit summary and the Diagnostics side card are their own
/// scroll boxes. Emptying them on the 30 s tick collapsed their height and
/// dropped the operator's scroll position, so they diff by keyed card instead.
#[test]
fn dashboard_summary_scroll_boxes_replace_only_the_cards_whose_data_moved() {
    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(tag = "") { this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const old = child.parentNode; if (old) old.children = old.children.filter((candidate) => candidate !== child); const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  get classList() { return { add: () => {} }; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node()), byId.get(id));
globalThis.document = { getElementById: get, createElement: (tag) => new Node(tag), createTextNode: (text) => Object.assign(new Node(), { textContent: text }) };
globalThis.window = { location: new URL("http://dashboard.test/#audit"), addEventListener: () => {} };

const ctx = { fmtDuration: (value) => String(value) };
const { renderAuditSummary } = await import("./audit.js");
const { renderDiagnosticsSideCard } = await import("./diagnostics.js");

const cardsIn = (container) => new Map(container.children.filter((node) => node.dataset.key).map((node) => [node.dataset.key, node]));
function expectStable(label, container, before, changedKey) {
  const after = cardsIn(container);
  if (after.size !== before.size) throw new Error(`${label}: card set changed (${before.size} -> ${after.size})`);
  for (const [key, node] of before) {
    const reused = after.get(key) === node;
    if (key === changedKey && reused) throw new Error(`${label}: the ${key} card was not rebuilt after its data moved`);
    if (key !== changedKey && !reused) throw new Error(`${label}: the ${key} card was rebuilt with unchanged data`);
  }
}

const summary = {
  window: "24h",
  duration_by_tool: [{ tool: "orbit.task.show", count: 3, avg: 10, p95: 20 }],
  role_split: [{ label: "human", count: 1, mcp: 1, cli: 0, other: 0, no_subcommand: 0 }],
  mcp_vs_cli_split: [{ label: "mcp", count: 1 }],
};
const auditBody = get("audit-summary-body");
renderAuditSummary(summary, ctx);
const auditCards = cardsIn(auditBody);
if (auditCards.size !== 3) throw new Error(`expected three keyed audit cards, got ${auditCards.size}`);
renderAuditSummary(summary, ctx);
expectStable("audit summary", auditBody, auditCards, null);
summary.role_split[0].count = 2;
renderAuditSummary(summary, ctx);
expectStable("audit summary", auditBody, auditCards, "role-split");

const diagnostics = {
  completion_by_complexity: [{ complexity: "low", total: 2, statuses: [{ status: "done", count: 1 }] }],
  implement_one_by_complexity: [],
  implement_one: [{ actor: "claude", n: 2, avg: 5, p50: 4, p95: 9 }],
};
const diagBody = get("diag-implement-one-body");
renderDiagnosticsSideCard(diagnostics, ctx);
const diagCards = cardsIn(diagBody);
if (diagCards.size !== 2) throw new Error(`expected two keyed diagnostics cards, got ${diagCards.size}`);
renderDiagnosticsSideCard(diagnostics, ctx);
expectStable("diagnostics side card", diagBody, diagCards, null);
diagnostics.implement_one[0].n = 3;
renderDiagnosticsSideCard(diagnostics, ctx);
expectStable("diagnostics side card", diagBody, diagCards, "implement-one");
"#,
    );
}

/// ORB-10874: switching the workspace selector only updated in-memory state,
/// so a reload silently fell back to the server's default workspace instead
/// of the one the operator had selected.
#[test]
fn dashboard_workspace_selection_persists_to_the_url() {
    let app = include_str!("../../assets/dashboard/app.js");

    assert!(
        app.contains("function persistWorkspaceToUrl(") && app.contains("persistScopeToUrl()"),
        "the workspace selector must persist its choice to the URL on every change"
    );
}

/// ORB-10972 supersedes ORB-10874's log-panel affordances. The tail moved into
/// the Tasks tab's right dock, which has two modes (Status / Log) and fills the
/// column's full height — so there is no panel height to drag and no collapsed
/// state to toggle. Their job is now split between the dock's mode toggle and
/// an always-on bottom status bar that carries the newest line on every tab.
/// What survives from ORB-10874 is the principle: the presentation choice is
/// local, so it persists to localStorage under the same key, and the task list
/// keeps an explicit minimum height so it can never be squeezed toward zero.
#[test]
fn dashboard_log_dock_has_two_modes_and_an_always_on_status_bar() {
    let log_tail = include_str!("../../assets/dashboard/log-tail.js");
    let index = include_str!("../../assets/dashboard/index.html");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        log_tail.contains("orbit.dashboard.logPanel"),
        "the dock's mode preference must persist to localStorage"
    );
    assert!(
        log_tail.contains(r#"const DOCK_MODES = ["status", "log"];"#)
            && log_tail.contains("function wireDockModeToggle("),
        "the dock must offer exactly the Status and Log modes, with a wired toggle"
    );
    assert!(
        !log_tail.contains("wireLogPanelResizeHandle")
            && !log_tail.contains("LOG_PANEL_MIN_HEIGHT"),
        "the superseded height-resize handle must be gone, not left dead"
    );
    assert!(
        index.contains(r#"id="dock-mode-toggle""#) && index.contains(r#"id="side-dock""#),
        "the dock and its mode toggle must exist in the markup"
    );
    assert!(
        index.contains(r#"data-pane="status""#) && index.contains(r#"data-pane="log""#),
        "the dock must declare both panes"
    );
    assert!(
        css.contains(r#"#side-dock[data-mode="log"] .dock-pane[data-pane="log"]"#),
        "the visible pane must be driven by the host's data-mode, so the column \
         width is identical in both modes and the task table never reflows"
    );

    // The always-on ambient line, present on every tab — including the ones
    // where the dock is not mounted.
    assert!(
        index.contains(r#"id="log-statusbar""#) && index.contains(r#"id="log-statusbar-message""#),
        "the bottom status bar must exist in the markup"
    );
    assert!(
        log_tail.contains("function updateLogStatusBar(")
            && log_tail.contains("updateLogStatusBar(ev);"),
        "each incoming log event must be mirrored into the status bar"
    );
    assert!(
        css.contains(".log-statusbar"),
        "the status bar must be styled"
    );

    assert!(
        css.contains("#tasks-panel > .body") && css.contains("min-height: 240px;"),
        "the task list must keep a guaranteed minimum usable height"
    );
    assert!(
        css.contains(".main-col > .tab-pane[data-tab=\"tasks\"] .col-tasks")
            && css.contains(".col-tasks {\n        min-height: 0;"),
        "the tasks column must be allowed to shrink so #tasks-body can scroll"
    );
    assert!(
        css.contains("#side-dock.disconnected .live-dot")
            && css.contains(".log-statusbar.disconnected .live-dot"),
        "a failed log stream must restyle the dock and status-bar live dots"
    );
}

/// ORB-11660: the tail must resume from the snapshot byte offset, mark the
/// dock/status bar disconnected when EventSource goes CLOSED (503 / fatal),
/// show "log stream unavailable, retrying", and recover on the next open.
#[test]
fn dashboard_log_tail_resumes_from_snapshot_offset_and_retries_on_close() {
    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(id = "") {
    this.id = id;
    this.children = [];
    this.dataset = {};
    this.style = {};
    this.listeners = {};
    this.className = "";
    this._text = "";
    this.parentNode = null;
    this.attributes = {};
  }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) {
    const index = this.children.indexOf(before);
    if (index < 0) return this.appendChild(child);
    this.children.splice(index, 0, child);
    child.parentNode = this;
    return child;
  }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter((child) => child !== this); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this.attributes[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this._text = String(value); this.children = []; }
  get innerHTML() { return this.textContent; }
  get firstChild() { return this.children[0] || null; }
  querySelector(sel) {
    if (sel.startsWith(".")) {
      const cls = sel.slice(1);
      return this.children.find((child) => (child.className || "").split(/\s+/).includes(cls)) || null;
    }
    return null;
  }
  querySelectorAll() { return []; }
  get classList() {
    const self = this;
    const tokens = () => self.className.split(/\s+/).filter(Boolean);
    const write = (next) => { self.className = next.join(" "); };
    return {
      add: (...c) => write([...new Set([...tokens(), ...c])]),
      remove: (...c) => write(tokens().filter((token) => !c.includes(token))),
      toggle: (c, on) => {
        const has = tokens().includes(c);
        const should = on === undefined ? !has : Boolean(on);
        if (should) write([...new Set([...tokens(), c])]);
        else write(tokens().filter((token) => token !== c));
        return should;
      },
      contains: (c) => tokens().includes(c),
    };
  }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
const bar = get("log-statusbar");
const label = new Node();
label.className = "sb-label";
label.textContent = "orbit.log";
bar.appendChild(label);
get("logInner");
get("side-dock");
globalThis.document = {
  body: new Node("body"),
  getElementById: get,
  createElement: () => new Node(),
  querySelectorAll: () => [],
  querySelector: () => null,
  addEventListener: () => {},
};
const location = new URL("http://dashboard.test/");
globalThis.window = { location, innerHeight: 900, addEventListener: () => {}, localStorage: { getItem: () => null, setItem: () => {} } };
// Long timers are held rather than run so the retry can be driven by hand.
// clearTimeout has to actually clear: fetchJson arms a 30s abort timer and
// cancels it in its `finally`, and a no-op stub would leave that behind and
// make it look like a pending stream retry.
const pendingTimers = new Map();
let nextTimerId = 1;
const nativeSetTimeout = setTimeout;
globalThis.setTimeout = (fn, ms = 0) => {
  if (ms < 250) return nativeSetTimeout(fn, ms);
  const id = nextTimerId++;
  pendingTimers.set(id, fn);
  return id;
};
globalThis.clearTimeout = (id) => { pendingTimers.delete(id); };
const retryFns = () => [...pendingTimers.values()];
const sources = [];
class MockEventSource {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  constructor(url) {
    this.url = String(url);
    this.readyState = MockEventSource.CONNECTING;
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    sources.push(this);
  }
  close() { this.readyState = MockEventSource.CLOSED; }
}
globalThis.EventSource = MockEventSource;
globalThis.fetch = async (path) => {
  const payload = { events: [{ ts: "t", source: "job", code: "OK", level: "info", message_html: "hi" }], offset: 42 };
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};
const tick = () => new Promise((resolve) => nativeSetTimeout(resolve, 0));
const { initLogTail } = await import("./log-tail.js");
initLogTail();
await tick();
await tick();
if (sources.length !== 1) throw new Error(`expected one EventSource, got ${sources.length}`);
if (!sources[0].url.includes("from=42")) throw new Error(`stream url missing snapshot offset: ${sources[0].url}`);
sources[0].readyState = EventSource.CLOSED;
sources[0].onerror();
if (label.textContent !== "log stream unavailable, retrying") {
  throw new Error(`disconnected copy missing, label=${label.textContent}`);
}
if (!bar.classList.contains("disconnected")) throw new Error("status bar did not mark disconnected");
if (!get("side-dock").classList.contains("disconnected")) throw new Error("dock did not mark disconnected");
if (retryFns().length !== 1) throw new Error(`expected one retry timer, got ${retryFns().length}`);
retryFns()[0]();
if (sources.length !== 2) throw new Error(`retry did not open a new EventSource, got ${sources.length}`);
if (!sources[1].url.includes("from=42")) throw new Error(`retry lost resume offset: ${sources[1].url}`);
sources[1].readyState = EventSource.OPEN;
sources[1].onopen();
if (label.textContent !== "orbit.log") throw new Error(`did not recover label, got ${label.textContent}`);
if (bar.classList.contains("disconnected")) throw new Error("status bar stayed disconnected after open");
if (get("side-dock").classList.contains("disconnected")) throw new Error("dock stayed disconnected after open");
"#,
    );
}

/// ORB-10972: the top-level nav is a left rail, and the vertical chrome above
/// the task table collapses into one bar. The rail keeps the class and id
/// contract `router.js` selects on, which is what makes every prior hash route
/// resolve unchanged.
#[test]
fn dashboard_nav_rail_preserves_the_router_selector_contract() {
    let index = include_str!("../../assets/dashboard/index.html");
    let css = include_str!("../../assets/dashboard/dashboard.css");
    let app = include_str!("../../assets/dashboard/app.js");

    assert!(
        index.contains(r#"<nav class="rail""#) && css.contains(".rail {"),
        "the nav must render as a rail"
    );
    // The router appends #tab-indicator to `.tabs` and #subtab-indicator to
    // `#diag-subtabs`, and toggles `.active` on `.tab` / `.subtab`. Those hooks
    // must survive the move or every route breaks at once.
    assert!(
        index.contains(r#"<div class="tabs" id="tabs">"#),
        "the router appends its indicator to .tabs; the container must remain"
    );
    assert_eq!(
        index.matches(r#"id="diag-subtabs""#).count(),
        1,
        "Diagnostics' subtabs must keep exactly one id, now as visible rail children"
    );
    assert!(
        css.contains(".rail .tab-indicator { display: none !important; }"),
        "the sliding underline is suppressed in the rail, not removed from the router"
    );

    // The four health metrics ride inline in the top bar, keeping their ids.
    assert!(
        index.contains(r#"class="topbar""#) && index.contains(r#"class="kpis" id="health-strip""#),
        "the health metrics must ride inline in the top bar"
    );
    for id in [
        "tile-events-value",
        "tile-denials-value",
        "tile-failed-value",
        "tile-active-value",
    ] {
        assert!(
            index.contains(&format!(r#"id="{id}""#)),
            "{id} must survive the move"
        );
    }

    // Rail counts come from data the dashboard already fetches — no new endpoint.
    assert!(
        app.contains("function setRailCount("),
        "rail counts must be set through one helper"
    );
    assert!(
        css.contains(".rail-count.alert"),
        "a failure count must be distinguishable in the rail"
    );
}

/// ORB-10972: a two-tier border scale. `--border` draws panel and control
/// edges; `--hair` draws hairlines inside them. Before this both were #333333,
/// which made the panel grid read as loud as its contents.
#[test]
fn dashboard_separates_panel_edges_from_internal_hairlines() {
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        css.contains("--hair: #17171a;") && css.contains("--border: #2a2a2e;"),
        "both tiers must be defined, and they must differ"
    );
    assert!(
        css.contains("--fg-mute:"),
        "the tertiary text tier must be defined alongside them"
    );

    // The hairlines that separate rows within a panel must use the inner tier.
    for rule in [
        ".row {",
        ".row.header {",
        ".controls {",
        ".filter-summary {",
        ".panel > header {",
    ] {
        let start = css
            .find(rule)
            .unwrap_or_else(|| panic!("{rule} must exist"));
        let block = &css[start..start + 900.min(css.len() - start)];
        let end = block.find('}').map(|i| &block[..i]).unwrap_or(block);
        assert!(
            !end.contains("1px solid var(--border)"),
            "{rule} draws an internal hairline; it must use --hair, not --border"
        );
    }
}

/// ORB-10874: inline status/crew edits must show a pending state, refuse a
/// second submission while one is in flight, report durable success/failure
/// text (not just console.error), and offer a bounded undo while the prior
/// value can still be safely restored.
#[test]
fn dashboard_inline_task_edits_report_pending_success_failure_and_offer_undo() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(
        tasks.contains(r#"{ kind: "pending", text: "saving…" }"#),
        "a status/crew change must show a pending state"
    );
    assert!(
        tasks.contains(r#"kind: "success""#) && tasks.contains(r#"kind: "error""#),
        "a status/crew change must report durable success or failure feedback"
    );
    assert!(
        tasks.contains("class: \"mutation-undo\", text: \"undo\""),
        "a successful change must offer an undo control"
    );
    assert!(
        tasks.contains("MUTATION_UNDO_WINDOW_MS") && tasks.contains("scheduleFeedbackExpiry("),
        "undo must be bounded to a window, not offered indefinitely"
    );
    assert!(
        tasks.contains("(feedback && feedback.kind === \"pending\")"),
        "the control must disable itself while its own change is pending"
    );
}

/// ORB-10874: in the aggregate ("All workspaces") view there is no ambient
/// workspace to scope a status/crew mutation to. A task fetched through
/// /api/tasks/all carries its own workspace_id (ORB-00037); mutation is
/// refused unless that explicit, workspace-qualified target is available.
#[test]
fn dashboard_aggregate_view_guards_inline_task_mutations() {
    let tasks = include_str!("../../assets/dashboard/tasks.js");

    assert!(
        tasks.contains("function canMutateTask(task) {")
            && tasks.contains("!isAggregateView() || Boolean(task && task.workspace_id)"),
        "mutation must be refused in aggregate mode unless the task names its own workspace"
    );
    assert!(
        tasks.contains("function taskMutationPath(task"),
        "an aggregate-mode mutation must target the task's own workspace explicitly, not the ambient one"
    );

    // ORB-12235: every inline control in the detail is refused the same way the
    // crew select is, and says so in the same words.
    run_task_detail_harness(
        r#"
setMultiWorkspace(true);
render();
expand();

const crewSelect = find(body, (node) => node.className === "task-crew-select mono");
if (!crewSelect.disabled) throw new Error("the crew select must be disabled in aggregate view");
const refusal = "select a specific workspace to";
const frame = (title) => title.includes(refusal) && title.endsWith(" in aggregate view");
if (!frame(crewSelect.title)) throw new Error(`crew refusal changed shape: ${crewSelect.title}`);

const complexitySelect = find(body, (node) => node.className === "task-complexity-select mono");
if (!complexitySelect.disabled) throw new Error("the complexity select must be disabled in aggregate view");
if (!frame(complexitySelect.title)) throw new Error(`complexity refusal reads differently: ${complexitySelect.title}`);

for (const title of ["description", "acceptance criteria", "tags", "context files"]) {
  const block = fieldBlock(title);
  if (!block) throw new Error(`${title} is missing from the detail`);
  const edit = find(block, (node) => node.className === "field-edit");
  if (!edit.disabled) throw new Error(`the ${title} editor must be disabled in aggregate view`);
  if (!frame(edit.title)) throw new Error(`${title} refusal reads differently: ${edit.title}`);
  edit.listeners.click({ stopPropagation() {} });
  if (editorInput(fieldBlock(title))) throw new Error(`the ${title} editor opened in aggregate view`);
}

complexitySelect.value = "hard";
complexitySelect.listeners.change({ stopPropagation() {} });
await tick();
if (requests.length !== 0) throw new Error(`aggregate view issued writes: ${JSON.stringify(requests)}`);
"#,
    );
}

/// ORB-12235: the five task fields the dashboard can write. Each save carries
/// that field alone — a whole-task PATCH would clobber a concurrent agent write
/// — and the server's refusal of a context selector stays inline so the
/// operator can answer it with the allow-missing escape instead of retyping.
#[test]
fn dashboard_task_detail_edits_each_field_through_a_single_field_patch() {
    run_task_detail_harness(
        r#"
render();
expand();

// complexity: a fixed select with no `unassessed` option (the server rejects it),
// reporting saving… while the write is in flight and saved once it lands.
const complexitySelect = find(body, (node) => node.className === "task-complexity-select mono");
const offered = complexitySelect.children.filter((option) => option.value).map((option) => option.value);
if (JSON.stringify(offered) !== JSON.stringify(["low", "medium", "hard"])) {
  throw new Error(`complexity offered the wrong options: ${JSON.stringify(offered)}`);
}
if (complexitySelect.children.some((option) => option.value === "unassessed")) {
  throw new Error("unassessed must not be offered; the update endpoint rejects it");
}

let release = null;
hold = new Promise((resolve) => { release = resolve; });
complexitySelect.value = "medium";
complexitySelect.listeners.change({ stopPropagation() {} });
await tick();
if (!detailText().includes("saving…")) throw new Error(`no pending feedback: ${detailText()}`);
if (!find(body, (node) => node.className === "task-complexity-select mono").disabled) {
  throw new Error("the complexity select must refuse a second change while its own write is pending");
}
hold = null;
release();
await tick();
if (!detailText().includes("complexity saved")) throw new Error(`no success feedback: ${detailText()}`);
if (current.complexity !== "medium") throw new Error(`the applied task kept ${current.complexity}`);
expectPatch({ complexity: "medium" });

// Each text field: the editor opens on the persisted value and saves only itself.
await saveField("description", "rewritten body", { description: "rewritten body" });
if (!fieldBlock("description").textContent.includes("rewritten body")) {
  throw new Error(`the saved description was not re-rendered: ${fieldBlock("description").textContent}`);
}
await saveField("acceptance criteria", "first\nsecond\n\n", { acceptance_criteria: ["first", "second"] });
await saveField("tags", "dashboard, orbit-web", { tags: ["dashboard", "orbit-web"] });

// A rejected context-files save keeps the editor, the text, and the server's reason.
const block = openEditor("context files");
editorInput(block).value = "file:a.rs\nfile:missing.rs";
await clickAction(block, "save");
const open = fieldBlock("context files");
const input = editorInput(open);
if (!input) throw new Error("the rejected save closed the context-files editor");
if (input.value !== "file:a.rs\nfile:missing.rs") throw new Error(`the editor lost the text: ${input.value}`);
const error = find(open, (node) => node.className === "field-editor-error");
if (!error || !error.textContent.includes("context selector not found")) {
  throw new Error(`the server's reason was not shown inline: ${open.textContent}`);
}
if (current.context_files.includes("file:missing.rs")) throw new Error("a refused save was applied anyway");

// Ticking allow-missing-context answers it; the escape rides with that one field.
find(open, (node) => node.className === "field-editor-toggle-input").checked = true;
await clickAction(open, "save");
expectPatch({ context_files: ["file:a.rs", "file:missing.rs"], allow_missing_context: true });
if (JSON.stringify(current.context_files) !== JSON.stringify(["file:a.rs", "file:missing.rs"])) {
  throw new Error(`context files were not applied: ${JSON.stringify(current.context_files)}`);
}

// Cancel restores the view without a request.
const before = requests.length;
const cancelled = openEditor("description");
editorInput(cancelled).value = "never saved";
clickAction(cancelled, "cancel");
if (requests.length !== before) throw new Error("cancel issued a request");
const restored = fieldBlock("description");
if (editorInput(restored)) throw new Error("cancel left the editor open");
if (!restored.textContent.includes("rewritten body")) {
  throw new Error(`cancel did not restore the previous view: ${restored.textContent}`);
}
"#,
    );
}

/// ORB-11655/ORB-12235: the 30 s refresh must not rebuild a detail that holds an
/// open editor — the textarea is the only copy of the operator's unsaved text.
/// The detail resumes tracking task data as soon as the editor closes.
#[test]
fn dashboard_task_refresh_keeps_an_open_field_editor() {
    run_task_detail_harness(
        r#"
render();
expand();

// Control: with no editor open, a changed task rebuilds its detail.
const before = detailNode();
refresh();
if (detailNode() === before) throw new Error("a changed task must still rebuild its detail");

const held = detailNode();
const block = openEditor("description");
editorInput(block).value = "half written";
refresh();
if (detailNode() !== held) throw new Error("the refresh replaced a detail holding an open editor");
const live = editorInput(fieldBlock("description"));
if (!live || live.value !== "half written") throw new Error(`the draft text was lost: ${live && live.value}`);

clickAction(fieldBlock("description"), "cancel");
refresh();
if (detailNode() === held) throw new Error("the detail stayed frozen after the editor was closed");
if (editorInput(fieldBlock("description"))) throw new Error("the cancelled editor is still rendered");
"#,
    );
}

/// The DOM stand-in and task fixture shared by the task-detail editor harness
/// tests. The scenario script runs with the shipped `tasks.js` imported, a task
/// expanded on demand, and every PATCH recorded: `requests` holds them in order,
/// `hold` defers the next response, and a context selector naming a missing
/// target is refused the way the server refuses it.
fn run_task_detail_harness(scenario: &str) {
    let prelude = r#"
class Node {
  constructor(tag = "") {
    this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {};
    this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false;
    this.hidden = false; this.value = ""; this.checked = false;
  }
  appendChild(child) { if (child == null) return child; if (child.parentNode) child.parentNode.removeChild(child); this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { if (child.parentNode) child.parentNode.removeChild(child); const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  replaceChildren(...next) { for (const child of this.children) child.parentNode = null; this.children = []; for (const child of next) this.appendChild(child); }
  replaceWith(next) { const parent = this.parentNode; if (!parent) return; parent.children = parent.children.map((candidate) => (candidate === this ? next : candidate)); next.parentNode = parent; this.parentNode = null; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  focus() {}
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); for (const child of this.children) child.parentNode = null; this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  get classList() {
    const self = this;
    const names = () => self.className.split(/\s+/).filter(Boolean);
    return {
      add: (...added) => { for (const name of added) if (!names().includes(name)) self.className = `${self.className} ${name}`.trim(); },
      toggle: (name) => {
        if (names().includes(name)) { self.className = names().filter((candidate) => candidate !== name).join(" "); return false; }
        self.className = `${self.className} ${name}`.trim();
        return true;
      },
    };
  }
  querySelectorAll(selector) {
    const wanted = selector.replace(".", "");
    const found = [];
    const visit = (node) => { for (const child of node.children) { if (child.className.split(/\s+/).includes(wanted)) found.push(child); visit(child); } };
    visit(this);
    return found;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  querySelectorAll: () => [],
};
globalThis.window = { location: new URL("http://dashboard.test/#tasks"), addEventListener() {}, confirm: () => true };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });

// Feedback expiry is a timer the dashboard schedules against wall-clock; the
// harness observes the states themselves, so the timers are recorded, not run.
const nativeSetTimeout = setTimeout;
globalThis.setTimeout = () => 0;
const tick = () => new Promise((resolve) => nativeSetTimeout(resolve, 0));

const task = {
  id: "ORB-1", title: "Editable", status: "review", updated_at: "2026-09-12T01:00:00Z",
  description: "original body", acceptance_criteria: ["first"], tags: ["dashboard"],
  context_files: ["file:a.rs"], history: [], artifacts: [],
};
let current = task;
const requests = [];
let hold = null;
globalThis.fetch = async (path, opts = {}) => {
  const body = opts.body ? JSON.parse(opts.body) : null;
  requests.push({ path: String(path), method: opts.method || "GET", body });
  if (hold) await hold;
  const selectors = body && Array.isArray(body.context_files) ? body.context_files : [];
  if (selectors.some((selector) => selector.includes("missing")) && !(body && body.allow_missing_context)) {
    const refusal = JSON.stringify({ error: "context selector not found: file:missing.rs" });
    return { ok: false, status: 400, text: async () => refusal };
  }
  const updated = { ...current, ...body };
  delete updated.allow_missing_context;
  const payload = JSON.stringify(updated);
  return { ok: true, status: 200, text: async () => payload };
};

const statuses = ["review"];
const context = {
  getTasks: () => [current], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["review"]), statusOrder: statuses, statusUpdateTargets: statuses,
  fmtAbsTime: (value) => value, refreshDashboard: () => Promise.resolve(),
  replaceTask: (next) => { current = next; },
};

const { setMultiWorkspace } = await import("./common.js");
const { renderTasks } = await import("./tasks.js");
const body = get("tasks-body");

function find(node, predicate) {
  if (!node) return null;
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
const render = () => renderTasks(context.getTasks(), context);
const expand = () => find(body, (node) => node.dataset.key === "task-ORB-1").listeners.click();
const detailNode = () => find(body, (node) => node.dataset.key === "detail-ORB-1");
const detailText = () => detailNode().textContent;
// A background poll: the task data moved, nothing the operator did.
const refresh = () => { current = { ...current, updated_at: `${current.updated_at}+` }; render(); };
const fieldBlock = (title) => {
  const blocks = [];
  const visit = (node) => { for (const child of node.children) { if (child.className.split(/\s+/).includes("field-block")) blocks.push(child); visit(child); } };
  visit(detailNode());
  return blocks.find((block) => block.children[0] && block.children[0].textContent === title) || null;
};
const editorInput = (block) => find(block, (node) => node.className === "field-editor-input mono");
const openEditor = (title) => {
  const block = fieldBlock(title);
  find(block, (node) => node.className === "field-edit").listeners.click({ stopPropagation() {} });
  return fieldBlock(title);
};
const clickAction = (block, name) =>
  find(block, (node) => node.className === `action ${name}`).listeners.click({ stopPropagation() {} });
const expectPatch = (expected) => {
  const last = requests[requests.length - 1];
  if (last.method !== "PATCH" || last.path !== "/api/tasks/ORB-1") {
    throw new Error(`unexpected request: ${JSON.stringify(last)}`);
  }
  if (JSON.stringify(last.body) !== JSON.stringify(expected)) {
    throw new Error(`patch body was ${JSON.stringify(last.body)}, expected ${JSON.stringify(expected)}`);
  }
};
const saveField = async (title, text, expected) => {
  const block = openEditor(title);
  const input = editorInput(block);
  if (!input) throw new Error(`${title} has no editor`);
  input.value = text;
  await clickAction(block, "save");
  expectPatch(expected);
};
"#;

    run_dashboard_javascript_test(&format!("{prelude}\n{scenario}"));
}

/// ORB-10444: dashboard assets are a shipped, project-agnostic surface. A
/// personal name, an Orbit/knowledge id, or a checkout path baked into them
/// would ship to every install, so the served assets carry none.
#[test]
fn dashboard_assets_carry_no_project_specific_identifiers() {
    let assets = [
        (
            "index.html",
            include_str!("../../assets/dashboard/index.html"),
        ),
        (
            "dashboard.css",
            include_str!("../../assets/dashboard/dashboard.css"),
        ),
        ("app.js", include_str!("../../assets/dashboard/app.js")),
        (
            "common.js",
            include_str!("../../assets/dashboard/common.js"),
        ),
        (
            "markdown.js",
            include_str!("../../assets/dashboard/markdown.js"),
        ),
        ("tasks.js", include_str!("../../assets/dashboard/tasks.js")),
        (
            "field-editor.js",
            include_str!("../../assets/dashboard/field-editor.js"),
        ),
        ("audit.js", include_str!("../../assets/dashboard/audit.js")),
        (
            "scoreboard.js",
            include_str!("../../assets/dashboard/scoreboard.js"),
        ),
        (
            "log-tail.js",
            include_str!("../../assets/dashboard/log-tail.js"),
        ),
        (
            "diagnostics.js",
            include_str!("../../assets/dashboard/diagnostics.js"),
        ),
        (
            "router.js",
            include_str!("../../assets/dashboard/router.js"),
        ),
        ("runs.js", include_str!("../../assets/dashboard/runs.js")),
        (
            "run-detail.js",
            include_str!("../../assets/dashboard/run-detail.js"),
        ),
        (
            "reliability.js",
            include_str!("../../assets/dashboard/reliability.js"),
        ),
        (
            "operations.js",
            include_str!("../../assets/dashboard/operations.js"),
        ),
    ];
    // Personal names and layout paths of the machine Orbit is developed on, plus
    // the workspace names it registers. `orbit`/`ORB-` themselves are the
    // product's own vocabulary and are not project-specific.
    let banned = [
        "daniel",
        "/home/",
        "constellation",
        "knowledgebase",
        "polaris",
        "almanac",
        "dk-server",
        "sextant",
        "agentbase",
    ];

    for (name, source) in assets {
        let lowered = source.to_lowercase();
        for needle in banned {
            assert!(
                !lowered.contains(needle),
                "{name} must not name `{needle}` — dashboard assets ship to every install"
            );
        }
        for (line_index, line) in source.lines().enumerate() {
            // Knowledge-artifact ids (L-0021, ADR-0001, F2026-07-015) name
            // records that exist only in the authoring workspace. Task ids are
            // the exception: they are the repo's own change provenance and are
            // cited in comments across the codebase.
            for prefix in ["L-", "ADR-", "F20"] {
                assert!(
                    !line.contains(prefix),
                    "{name}:{} references a knowledge id (`{prefix}…`): {line}",
                    line_index + 1
                );
            }
        }
    }
}

/// ORB-10872: workspace + window are one dashboard scope. Scoreboard and
/// Managed Execution honor the same window; a mismatched 24h payload is
/// refused under a 7d selection; Reliability labels Fleet-wide; Audit
/// drill-downs expose removable chips; the URL restores the scope.
#[test]
fn dashboard_scope_is_shared_labeled_and_url_backed() {
    let common = include_str!("../../assets/dashboard/common.js");
    let app = include_str!("../../assets/dashboard/app.js");
    let router = include_str!("../../assets/dashboard/router.js");
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let reliability = include_str!("../../assets/dashboard/reliability.js");
    let audit = include_str!("../../assets/dashboard/audit.js");
    let index = include_str!("../../assets/dashboard/index.html");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        common.contains("export function getWindow(")
            && common.contains("export function setWindow(")
            && common.contains("export function payloadHonorsWindow(")
            && common.contains("export function persistScopeToUrl(")
            && common.contains("export function reliabilityWindowFor("),
        "common.js must own the shared dashboard window and payload-window guard"
    );
    assert!(
        common.contains("if (typeof reported === \"string\") return reported === selected;")
            && common.contains("reported.label === selected"),
        "payloadHonorsWindow must reject a 24h body under an active 7d selection"
    );
    assert!(
        app.contains("if (!payloadHonorsWindow(summary, selectedWindow))")
            && scoreboard.contains("if (summary && !payloadHonorsWindow(summary, getWindow()))"),
        "the scoreboard fetch and renderer must refuse a mismatched window payload"
    );
    assert!(
        reliability.contains("Fleet-wide")
            && index.contains(r#"id="reliability-scope-badge""#)
            && index.contains("Fleet-wide")
            && reliability.contains(r#"payload.scope === "workspace""#),
        "Reliability must label Fleet-wide when it ignores the selected workspace"
    );
    assert!(
        router.contains("markWorkspaceSelectorScope")
            && router.contains("Reliability is Fleet-wide; workspace does not apply")
            && css.contains(".workspace-select.scope-ignored")
            && css.contains(".scope-badge.independent"),
        "the workspace selector must not imply a scope Reliability does not use"
    );
    assert!(
        audit.contains("function navigateToDrilldown(")
            && audit.contains("function renderScopeChips(")
            && audit.contains(r#"removableChip("actor""#)
            && audit.contains(r#"removableChip("workspace""#)
            && audit.contains(r#"removableChip("window""#)
            && audit.contains(r#"removableChip("status""#)
            && audit.contains(r#"removableChip("metric""#)
            && index.contains(r#"id="audit-scope-chips""#),
        "actor/metric drill-down must show removable actor/workspace/window/status/metric chips"
    );
    assert!(
        router
            .contains(r#"hash = `#diagnostics/${sub}?window=${encodeURIComponent(getWindow())}`"#)
            && common.contains(r#"url.searchParams.set("window", currentWindow)"#)
            && audit.contains("sp.set(\"metric\", auditFilter.metric)"),
        "workspace, diagnostics subview, window, and drill-down filters must live in the URL"
    );
    assert!(
        css.contains("@media (max-width: 720px)")
            && css.contains("@media (max-width: 520px)")
            && css.contains(".scope-chip-v")
            && css.contains("max-width: 10ch"),
        "scope badges and filter chips must stay legible at 480–720px"
    );
}

/// ORB-10871: a repeated failure burst is one incident, not hundreds of
/// independent quality failures. The dashboard must therefore (a) show the
/// grouped count and the raw failed-event count side by side, each with its
/// denominator and the selected window, (b) let an operator expand an incident
/// down to the exact audit rows, actor, surfaces, run/task ids, first/last
/// timestamps, and grouping signature, and (c) never imply that a propagated
/// pipeline failure is its own root cause. All three live in the assets; this
/// pins them so a later edit cannot quietly go back to counting raw rows.
#[test]
fn dashboard_failure_metrics_are_incident_aware_and_state_their_denominators() {
    let index = include_str!("../../assets/dashboard/index.html");
    let app = include_str!("../../assets/dashboard/app.js");
    let router = include_str!("../../assets/dashboard/router.js");
    let diagnostics = include_str!("../../assets/dashboard/diagnostics.js");
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let audit = include_str!("../../assets/dashboard/audit.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    // Routed as a diagnostics subtab, fetched against the shared window.
    assert!(
        index.contains(r#"<button class="subtab" data-subtab="incidents" type="button">"#),
        "Incidents must be offered as a diagnostics subtab"
    );
    assert!(
        router.contains(r#""incidents""#),
        "the incidents subtab must be routable"
    );
    assert!(
        app.contains("/api/audit/incidents?since=${encodeURIComponent(selectedWindow)}"),
        "the incidents fetch must hang off the diagnostics subtab branch and honor the shared window"
    );

    // Both counts, both denominators, and the window are rendered — never one
    // number standing in for the other.
    assert!(
        diagnostics.contains("${asCount(payload.failure_categories && payload.failure_categories.unexpected && payload.failure_categories.unexpected.incidents)} unexpected / ${asCount(payload.incident_count)} all incidents / ${asCount(payload.raw_failed_events)} failed events"),
        "the panel count must separate unexpected incidents from the all-incident and raw-event populations"
    );
    assert!(
        diagnostics.contains("${unexpectedEvents} unexpected raw events · ${unexpectedRuns} affected runs; ${incidents} incidents / ${failed} failed events / ${runs} affected runs across ${total} audited events"),
        "the incident headline must state the unexpected and all-category denominators"
    );
    assert!(
        diagnostics.contains("`window ${window}`"),
        "the incident summary must name the window it was measured over"
    );
    assert!(
        diagnostics.contains("incident-class-chip") && diagnostics.contains("INCIDENT_CLASS_ORDER"),
        "denials, expected negative paths, and unexpected failures must stay distinguishable"
    );

    // Expansion exposes the underlying evidence.
    for needle in [
        "grouping signature",
        "first seen",
        "last seen",
        "\"actor\"",
        "\"runs\"",
        "\"tasks\"",
        "Underlying audit events",
        "\"tool\"",
    ] {
        assert!(
            diagnostics.contains(needle),
            "incident expansion must reveal `{needle}`"
        );
    }
    assert!(
        diagnostics.contains("downstream failures, not independent root causes"),
        "a propagation chain must be labeled as a chain, not as separate root causes"
    );
    assert!(
        diagnostics.contains("navigateToDrilldown(")
            && diagnostics.contains("Open raw audit events"),
        "an incident must link out to the raw audit rows it collapsed"
    );
    assert!(
        audit.contains("auditFilter.tool = opts.tool || null;"),
        "the drill-down must carry the incident's surface into the raw Audit filter"
    );

    // Scoreboard keeps the raw failure column and gains the grouped one.
    assert!(
        scoreboard.contains(r#"left: "failed_tool_calls""#)
            && scoreboard.contains(r#"right: "tool_calls""#),
        "the raw failed/total tool-call pair must survive"
    );
    assert!(
        scoreboard.contains(r#"key: "failure_incidents""#)
            && scoreboard.contains(r#"left: "failure_incidents""#)
            && scoreboard.contains(r#"right: "failure_incident_events""#),
        "the scoreboard must show grouped incidents against the raw events they collapsed"
    );
    assert!(
        scoreboard.contains("function allScoreboardSections()")
            && scoreboard.contains("window ${window}"),
        "every scoreboard section badge must name the selected window"
    );

    // Narrow-viewport presentation hooks (480–720px).
    assert!(
        css.contains(".incident-summary")
            && css.contains(".incident-facts")
            && css.contains(".incident-evidence"),
        "the incident summary and its expansion need their own presentation hooks"
    );
    let responsive_at = css
        .rfind("@media (max-width: 720px)")
        .expect("a 720px breakpoint must exist");
    assert!(
        css[responsive_at..].contains(".incident-facts { grid-template-columns: minmax(0, 1fr);")
            && css[responsive_at..].contains(".incident-evidence")
            && css[responsive_at..].contains(".lifecycle-failure-counts")
            && css[responsive_at..]
                .contains(".tool-health-grid { grid-template-columns: minmax(0, 1fr); }"),
        "the incident expansion and tool/lifecycle cards must reflow rather than clip below 720px"
    );
}

/// ORB-10969: Failures-by-tool excludes the synthetic `unknown` bucket;
/// job-run lifecycle failures are labeled on their own; expansion lists
/// every underlying row's run/task/tool identifiers.
#[test]
fn dashboard_tool_metrics_exclude_unknown_and_label_lifecycle_failures() {
    let audit = include_str!("../../assets/dashboard/audit.js");
    let diagnostics = include_str!("../../assets/dashboard/diagnostics.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        audit.contains("function isNamedTool(")
            && audit.contains("trimmed !== \"unknown\"")
            && audit.contains("lifecycle_diagnostic_events")
            && audit.contains("lifecycle diagnostics")
            && audit.contains("excluded from callable-tool denominators and rates"),
        "tool cards must drop `unknown` and name the lifecycle-diagnostic category"
    );
    assert!(
        audit.contains("${lifecycleIncidents} incidents · ${lifecycleFailures} raw events · ${Number(data.lifecycle_diagnostic_affected_run_count) || 0} affected runs"),
        "the lifecycle diagnostic card must distinguish incidents, raw events, and affected runs"
    );
    assert!(
        diagnostics.contains("incident.events")
            && diagnostics.contains("event.tool || \"-\"")
            && diagnostics.contains("event.run_id || \"-\"")
            && diagnostics.contains("event.task_id || \"-\""),
        "incident expansion must expose run/task/tool identifiers for every row"
    );
    assert!(
        css.contains(".lifecycle-failure-card")
            && css.contains(".lifecycle-failure-counts")
            && css.contains(".incident-lifecycle-note"),
        "lifecycle labels need their own presentation hooks"
    );
}

/// ORB-11118: the reliability card has one honest comparison population, while
/// expected negatives, denials, and failure-only diagnostics remain visible
/// as separately labeled incident populations with exact evidence expansion.
#[test]
fn dashboard_reliability_separates_all_four_failure_populations() {
    let audit = include_str!("../../assets/dashboard/audit.js");
    let diagnostics = include_str!("../../assets/dashboard/diagnostics.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    for needle in [
        "Unexpected Failures by Callable Tool",
        "Unexpected Failure Rate",
        "comparable calls (successful + unexpected failed)",
        "Failure categories · window",
        "classification",
        "raw events",
        "affected runs",
    ] {
        assert!(
            audit.contains(needle),
            "audit summary must render `{needle}`"
        );
    }
    assert!(
        !audit.contains("} else if (namedFailures.length)"),
        "failure-only populations must not fall back to a synthetic tool-rate card"
    );
    assert!(
        diagnostics.contains(
            r#"const INCIDENT_CLASS_ORDER = ["unexpected", "expected", "denied", "diagnostic"];"#
        ) && diagnostics.contains(
            "${labels[key] || key}: ${count} incidents · ${events} raw · ${categoryRuns} runs"
        ) && diagnostics.contains("${unexpectedIncidents} unexpected incidents"),
        "the incident view must visibly separate all four classes and headline only unexpected incidents"
    );
    for evidence in [
        "event.id",
        "event.tool || \"-\"",
        "event.run_id || \"-\"",
        "event.task_id || \"-\"",
        "event.execution_id",
    ] {
        assert!(
            diagnostics.contains(evidence),
            "incident expansion must retain `{evidence}`"
        );
    }
    assert!(
        css.contains(".incident-class-chip.diagnostic")
            && css.contains(".incident-row.diagnostic")
            && css.contains(".incident-class.diagnostic"),
        "diagnostic rows need a distinct desktop presentation"
    );
    let responsive_at = css
        .rfind("@media (max-width: 720px)")
        .expect("720px responsive rules");
    assert!(
        css[responsive_at..].contains(".incident-class-chip")
            && css[responsive_at..].contains("white-space: normal")
            && css[responsive_at..]
                .contains(".tool-health-grid { grid-template-columns: minmax(0, 1fr); }"),
        "four category labels and rate cards must remain scannable at narrow viewport widths"
    );
}

/// ORB-10873: Scoreboard delivery highlights, honest empty-section coverage,
/// accessible window tabs, and labeled abbreviations. Assets stay
/// project-agnostic.
#[test]
fn dashboard_scoreboard_highlights_are_accessible_and_honest() {
    let index = include_str!("../../assets/dashboard/index.html");
    let scoreboard = include_str!("../../assets/dashboard/scoreboard.js");
    let common = include_str!("../../assets/dashboard/common.js");
    let css = include_str!("../../assets/dashboard/dashboard.css");

    assert!(
        index.contains(r#"id="scoreboard-window-selector" role="tablist" aria-label="Scoreboard time window""#)
            && index.contains(r#"id="reliability-window-selector" role="tablist" aria-label="Reliability time window""#),
        "window controls must be semantic tablists"
    );
    assert!(
        index.contains(
            r#"role="tab" class="scoreboard-window-seg on" data-window="24h" aria-selected="true""#
        ) && common.contains("setAttribute(\"aria-selected\"")
            && common.contains("ArrowRight")
            && common.contains("ArrowLeft")
            && common.contains("Home")
            && common.contains("End"),
        "window tabs must expose selected state and keyboard navigation"
    );
    assert!(
        css.contains(".scoreboard-window-seg:focus-visible"),
        "window tabs must have a visible focus ring"
    );

    assert!(
        scoreboard.contains("Notable completions")
            && scoreboard.contains("not a quality score")
            && scoreboard.contains("No completion summary recorded.")
            && scoreboard.contains("function renderNotableCompletions("),
        "highlights must name the reading order and missing summaries"
    );
    assert!(
        !scoreboard.contains("quality score") || scoreboard.contains("not a quality score"),
        "the UI must not claim an objective quality score"
    );
    assert!(
        scoreboard.contains("no observed review comments in this source")
            && scoreboard.contains("coverage?.review?.availability === \"unavailable\"")
            && scoreboard.contains("missing coverage, not zero activity"),
        "empty Review must distinguish no events from incomplete coverage"
    );
    assert!(
        scoreboard.contains("coverage?.failure_incidents?.availability === \"unavailable\"")
            && scoreboard.contains("failure-incident coverage is unavailable for this window"),
        "empty Operations must distinguish no events from incomplete failure-incident coverage"
    );
    assert!(
        scoreboard.contains("orbit.task.* tool-call count")
            && scoreboard.contains("raw failed tool calls over total tool calls")
            && scoreboard.contains("append-only friction reports filed by this agent")
            && scoreboard.contains("Highest count in this row. Not a quality score."),
        "abbreviated metrics and the leader mark need plain-language definitions"
    );
    assert!(
        !scoreboard.contains("frict r"),
        "the unexplained frict r abbreviation must be gone"
    );

    assert!(
        css.contains(".scoreboard-highlights")
            && css.contains(".scoreboard-highlight-excerpt")
            && css.contains("overflow-wrap: anywhere;"),
        "highlights must wrap instead of clipping"
    );
    let scoreboard_720 = css
        .find("table.sb2-matrix col.metric { width: 132px; }")
        .expect("narrow scoreboard metric column");
    assert!(
        css[..scoreboard_720].contains("@media (max-width: 720px)"),
        "matrix labels must wrap at 480–720px"
    );

    for banned in ["constellation", "dk-server", "polaris", "SpaceX"] {
        assert!(
            !scoreboard
                .to_ascii_lowercase()
                .contains(&banned.to_ascii_lowercase()),
            "scoreboard assets must stay project-agnostic; found {banned}"
        );
    }
}

/// ORB-11207: ORB-11201 made `/api/scoreboard` emit `null` (not `0`) for
/// `failure_incidents`/`failure_incident_events` when the underlying audit
/// query fails, plus a `coverage.failure_incidents` note. The dashboard used
/// to coerce that `null` to `0`, rendering it as an indistinguishable `0/0`
/// and letting the activity filter drop the row and the section badge claim
/// observed-zero activity — reproducing exactly the confusion ORB-11201
/// fixed. Exercised with the executable Node harness in the ORB-11196 style
/// since the dashboard has no JS test runner.
#[test]
fn dashboard_scoreboard_renders_unavailable_failure_incidents_not_a_measured_zero() {
    run_dashboard_javascript_test(
        r#"
const nodes = [];
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = { setProperty: () => {} }; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.hidden = false; nodes.push(this); }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  prepend(child) { this.children.unshift(child); child.parentNode = this; }
  remove() { if (this.parentNode) this.parentNode.children = this.parentNode.children.filter((child) => child !== this); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get firstChild() { return this.children[0] || null; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  get classList() { const self = this; return { add: (...c) => { self.className = `${self.className} ${c.join(" ")}`.trim(); }, remove: () => {}, toggle: (c, on) => { if (on) this.addClass(c); } }; }
  addClass(c) { if (!this.className.split(/\\s+/).includes(c)) this.className = `${this.className} ${c}`.trim(); }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  contains(node) { return this === node || this.children.includes(node); }
  focus() {}
  closest() { return null; }
}
globalThis.Node = Node;
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = {
  body: new Node("body"), hidden: false,
  getElementById: get, createElement: () => new Node(), createElementNS: () => new Node(), createTextNode: (text) => Object.assign(new Node(), { textContent: text }), createDocumentFragment: () => new Node(),
  querySelectorAll: () => [], querySelector: () => new Node(), addEventListener: () => {},
};
const location = new URL("http://dashboard.test/");
globalThis.window = { location, innerHeight: 900, addEventListener: () => {}, matchMedia: () => ({ addEventListener: () => {}, matches: false }), localStorage: { getItem: () => null, setItem: () => {} } };
globalThis.history = { replaceState: (_, __, url) => { location.href = String(url); } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.requestAnimationFrame = (fn) => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { constructor() {} close() {} };

const { renderScoreboard } = await import("./scoreboard.js");

// A quiet agent whose failure-incident fields are `null`: the audit query
// failed for this window, so the source is unavailable, not a measured zero.
function quietAgentWithUnavailableFailureIncidents() {
  return {
    tasks_created: 0, tasks_planned: 0, tasks_completed: 0,
    tool_calls_by_surface: { graph: 0, task: 0 },
    tool_calls: 0, failed_tool_calls: 0,
    friction: { reported: 0 },
    failure_incidents: null,
    unexpected_failure_incidents: null,
    failure_incident_events: null,
  };
}
const summary = {
  window: "24h",
  agents: {
    codex: quietAgentWithUnavailableFailureIncidents(),
    claude: quietAgentWithUnavailableFailureIncidents(),
    gemini: quietAgentWithUnavailableFailureIncidents(),
    grok: quietAgentWithUnavailableFailureIncidents(),
  },
  coverage: {
    failure_incidents: {
      availability: "unavailable",
      detail: "Audit failure-incident query failed for the requested window; failure_incidents, unexpected_failure_incidents, and failure_incident_events are omitted (null) rather than shown as zero.",
    },
  },
};

renderScoreboard(summary);

const body = get("scoreboard-body");
const table = body.children[0].children[0];
if (!table || table.className !== "sb2-matrix") throw new Error("expected the scoreboard matrix table to render");
const tbody = table.children[2];

const failureRow = tbody.children.find((tr) => tr.dataset.key === "scoreboard-Operations-failure_incidents");
if (!failureRow) throw new Error("the failure_incidents row must not be hidden by the activity filter when its source is unavailable");
const rowText = failureRow.textContent;
if (!rowText.includes("unavailable")) throw new Error(`expected an explicit unavailable indicator, got: ${rowText}`);
if (rowText.includes("0/0")) throw new Error(`must not render an unavailable source as a measured 0/0, got: ${rowText}`);

const operationsDivider = tbody.children.find((tr) => tr.className === "group" && tr.textContent.includes("Operations"));
if (!operationsDivider) throw new Error("the Operations section divider must be present");
if (operationsDivider.textContent.includes("no observed tool calls or friction this window")) {
  throw new Error("the Operations badge must not assert observed-zero activity when failure-incident coverage is unavailable");
}
"#,
    );
}

#[test]
fn dashboard_aggregate_runs_keep_workspace_identity_filters_and_action_scope() {
    let css = include_str!("../../assets/dashboard/dashboard.css");
    let router = include_str!("../../assets/dashboard/router.js");
    assert!(css.contains(".runs-row.workspace-attributed"));
    assert!(css.contains("@media (max-width: 760px)"));
    assert!(css.contains("min-width: 900px"));
    assert!(router.contains("function navigateToRunImpl(ctx, runId, workspaceId = null)"));
    assert!(router.contains("setWorkspace(workspaceId);"));
    assert!(router.contains("persistScopeToUrl();"));

    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; }
  appendChild(child) { if (child == null) return child; if (child.parentNode) child.parentNode.removeChild(child); this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { if (child.parentNode) child.parentNode.removeChild(child); const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  get classList() { const self = this; return { add: (...classes) => { for (const c of classes) if (!self.className.split(/\s+/).includes(c)) self.className = `${self.className} ${c}`.trim(); }, toggle: (c, on) => { if (on) this.addClass(c); } }; }
  addClass(c) { if (!this.className.split(/\s+/).includes(c)) this.className = `${this.className} ${c}`.trim(); }
  querySelectorAll(selector) { const found = []; const visit = (node) => { for (const child of node.children) { if (selector === ".action-error" && child.className.split(/\s+/).includes("action-error")) found.push(child); visit(child); } }; visit(this); return found; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
const location = new URL("http://dashboard.test/?run_state=active");
globalThis.window = { location, innerWidth: 1200, confirm: () => true };
globalThis.history = { replaceState: (_, __, url) => { location.href = String(url); } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
const requests = [];
globalThis.fetch = async (path) => {
  requests.push(String(path));
  const payload = { run_id: "jrun-next" };
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};

const runs = [
  { workspace_id: "alpha", workspace_name: "Alpha", run_id: "jrun-shared", job_id: "ship", state: "running", created_at: "2026-09-05T03:00:00Z" },
  { workspace_id: "beta", workspace_name: "Beta", run_id: "jrun-shared", job_id: "ship", state: "failed", created_at: "2026-09-05T03:00:00Z" },
];
let navigated = null;
const { initRuns, renderRuns, buildReplayRunButton } = await import("./runs.js");
initRuns({
  getLastRuns: () => runs,
  getRunsMeta: () => ({ truncated: false }),
  getRunSourcesUnavailable: () => [{ workspace_id: "gone", workspace_name: "Gone", error: "query failed" }],
  navigateToRun: (runId, workspaceId) => { navigated = { runId, workspaceId }; },
  fetchAndRenderRuns: () => Promise.resolve(),
  getActiveRunId: () => null,
});
renderRuns(runs);
const body = get("runs-body");
let rows = body.children.filter((node) => node.className.includes("runs-row workspace-attributed") && !node.className.includes("runs-header"));
if (rows.length !== 1 || !rows[0].textContent.includes("Alpha")) throw new Error(`active filter rendered wrong rows: ${body.textContent}`);
if (!body.textContent.includes("Unavailable workspace: Gone")) throw new Error("partial workspace failure was hidden");

let controls = body.children.find((node) => node.className.includes("runs-filter"));
controls.children.find((node) => node.textContent === "all").listeners.click();
rows = body.children.filter((node) => node.className.includes("runs-row workspace-attributed") && !node.className.includes("runs-header"));
if (rows.length !== 2) throw new Error("all filter did not render both duplicate run ids");
if (new Set(rows.map((row) => row.dataset.key)).size !== 2) throw new Error("duplicate run ids collided across workspaces");
rows.find((row) => row.textContent.includes("Beta")).listeners.click();
if (!navigated || navigated.runId !== "jrun-shared" || navigated.workspaceId !== "beta") throw new Error(`wrong detail identity: ${JSON.stringify(navigated)}`);

const betaActions = rows.find((row) => row.textContent.includes("Beta")).children.at(-1);
betaActions.children.find((node) => node.className.includes("run-resume")).listeners.click({ stopPropagation() {} });
const alphaActions = rows.find((row) => row.textContent.includes("Alpha")).children.at(-1);
alphaActions.children.find((node) => node.className.includes("run-cancel")).listeners.click({ stopPropagation() {} });
const replay = buildReplayRunButton(runs[1], new Node());
replay.listeners.click({ stopPropagation() {} });
await new Promise((resolve) => setTimeout(resolve, 0));
if (!requests.includes("/api/job-runs/jrun-shared/resume?workspace=beta")) throw new Error(`resume lost workspace scope: ${requests}`);
if (!requests.includes("/api/runs/jrun-shared/cancel?workspace=alpha")) throw new Error(`cancel lost workspace scope: ${requests}`);
if (!requests.includes("/api/runs/jrun-shared/replay?workspace=beta")) throw new Error(`replay lost workspace scope: ${requests}`);

controls = body.children.find((node) => node.className.includes("runs-filter"));
controls.children.find((node) => node.textContent === "failed").listeners.click();
if (new URL(location.href).searchParams.get("run_state") !== "failed") throw new Error("run filter was not persisted in reload-safe URL state");
window.innerWidth = 480;
renderRuns(runs);
rows = body.children.filter((node) => node.className.includes("runs-row workspace-attributed") && !node.className.includes("runs-header"));
if (rows.length !== 1 || !rows[0].textContent.includes("Beta")) throw new Error("narrow-screen render lost the filtered workspace row");
"#,
    );
}

/// ORB-11561: Recent Runs used to limit first, then filter to failed in the
/// browser, so an older Failed run outside the newest success/active slice
/// rendered as 0/0. Loading and mismatched-filter paints must not look like
/// that empty result either.
#[test]
fn dashboard_failed_runs_filter_before_limit_and_label_distinct_scopes() {
    let app = include_str!("../../assets/dashboard/app.js");
    let runs = include_str!("../../assets/dashboard/runs.js");
    let diagnostics = include_str!("../../assets/dashboard/diagnostics.js");
    let index = include_str!("../../assets/dashboard/index.html");

    assert!(
        app.contains(
            r#"`/api/job-runs?limit=${JOB_RUN_LIMIT}&state=${encodeURIComponent(runFilter)}`"#
        ),
        "single-workspace Recent Runs must send the active state filter to the server"
    );
    assert!(
        !runs.contains("${top.length}/${sorted.length}"),
        "the old ambiguous N/M run count shorthand must be gone"
    );
    assert!(
        runs.contains("export function formatRunCount(")
            && runs.contains("shown")
            && runs.contains("total")
            && runs.contains("server limit"),
        "run counts must use explicit shown/total/server-limit language"
    );
    assert!(
        index.contains("Failed, timeout, and interrupted job runs in the selected window"),
        "the Failed runs header tile must explain its windowed population"
    );
    assert!(
        diagnostics
            .contains("No error events this month (step/event failures, not job-run states)."),
        "Errors empty copy must name the month-scoped event population"
    );

    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; }
  appendChild(child) { if (child == null) return child; if (child.parentNode) child.parentNode.removeChild(child); this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { if (child.parentNode) child.parentNode.removeChild(child); const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  remove() { if (this.parentNode) this.parentNode.removeChild(this); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this.textContent = value; }
  get innerHTML() { return this.textContent; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  get classList() { const self = this; return { add: (...classes) => { for (const c of classes) if (!self.className.split(/\s+/).includes(c)) self.className = `${self.className} ${c}`.trim(); }, toggle: (c, on) => { if (on) this.addClass(c); } }; }
  addClass(c) { if (!this.className.split(/\s+/).includes(c)) this.className = `${this.className} ${c}`.trim(); }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
const location = new URL("http://dashboard.test/?run_state=failed");
globalThis.window = { location, innerWidth: 1200, confirm: () => true };
globalThis.history = { replaceState: (_, __, url) => { location.href = String(url); } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });

const { initRuns, renderRuns, formatRunCount } = await import("./runs.js");
const { renderDiagnostics } = await import("./diagnostics.js");

if (formatRunCount(20, 25, { total: 81, limit: 25, truncated: true }) !== "20 shown (of 25 fetched) · 81 total · server limit 25") {
  throw new Error(`formatRunCount missed shown/total/limit language: ${formatRunCount(20, 25, { total: 81, limit: 25, truncated: true })}`);
}

let loading = true;
let lastRuns = [];
let lastMeta = { state: "all", total: 4, limit: 3, truncated: true };
let navigated = null;
initRuns({
  getLastRuns: () => lastRuns,
  getRunsMeta: () => lastMeta,
  getRunsLoading: () => loading,
  markRunsLoading: () => { loading = true; },
  getRunSourcesUnavailable: () => [],
  navigateToRun: (runId, workspaceId) => { navigated = { runId, workspaceId }; },
  fetchAndRenderRuns: () => Promise.resolve(),
  getActiveRunId: () => null,
});

renderRuns(lastRuns);
const loadingBody = get("runs-body").textContent;
if (loadingBody.includes("No failed job runs")) throw new Error("loading painted a zero-failure empty state");
if (get("diag-count").textContent !== "…") throw new Error(`loading count was treated as zero: ${get("diag-count").textContent}`);
if (!get("runs-body").children.some((node) => node.className.includes("skeleton-state"))) {
  throw new Error("loading must keep the skeleton, not an empty result");
}

loading = false;
lastRuns = [];
lastMeta = { state: "failed", total: 0, limit: 25, truncated: false };
renderRuns(lastRuns);
const emptyText = get("runs-body").textContent;
if (!emptyText.includes("No failed job runs (durable Failed state, no time window).")) {
  throw new Error(`empty copy did not name the failed-run scope: ${emptyText}`);
}
if (!emptyText.includes("Header Failed runs counts Failed, Timeout, and Interrupted")) {
  throw new Error("scope note must explain header vs Recent Runs vs Errors");
}
if (!get("diag-count").textContent.includes("0 shown") || !get("diag-count").textContent.includes("0 total")) {
  throw new Error(`empty count must still say shown/total, got ${get("diag-count").textContent}`);
}

lastRuns = [
  { run_id: "jrun-older-failed", job_id: "ship", state: "failed", created_at: "2026-09-07T10:00:00Z", finished_at: "2026-09-07T10:01:00Z" },
];
lastMeta = { state: "failed", total: 1, limit: 3, truncated: false };
renderRuns(lastRuns);
const failedRows = get("runs-body").children.filter((node) => node.className.includes("runs-row") && !node.className.includes("runs-header"));
if (failedRows.length !== 1 || !failedRows[0].textContent.includes("jrun-older-failed")) {
  throw new Error("server-filtered failed payload must keep a failure older than the recent success slice");
}
failedRows[0].listeners.click();
if (!navigated || navigated.runId !== "jrun-older-failed") {
  throw new Error(`failed-run drilldown did not open the older failure: ${JSON.stringify(navigated)}`);
}

lastRuns = Array.from({ length: 25 }, (_, index) => ({
  run_id: `jrun-failed-${index}`,
  job_id: "ship",
  state: "failed",
  created_at: "2026-09-07T12:00:00Z",
}));
lastMeta = { state: "failed", total: 81, limit: 25, truncated: true };
renderRuns(lastRuns);
if (!get("diag-count").textContent.includes("server limit 25") || !get("diag-count").textContent.includes("81 total")) {
  throw new Error(`truncated count missing shown/total/limit: ${get("diag-count").textContent}`);
}
if (!get("runs-body").textContent.includes("Raise the runs URL parameter to load older matches")) {
  throw new Error("truncated results must explain how to find older failures");
}

renderDiagnostics({
  getActiveDiagSubtab: () => "errors",
  getLastDiagnostics: () => ({ metrics: [], errors: [], incidents: null, implement_one: [], implement_one_by_complexity: [], completion_by_complexity: [] }),
});
if (!get("diag-body").textContent.includes("No error events this month (step/event failures, not job-run states).")) {
  throw new Error(`errors empty copy was wrong: ${get("diag-body").textContent}`);
}
if (get("diag-count").textContent !== "0 error events this month") {
  throw new Error(`errors count must name its month-scoped population, got ${get("diag-count").textContent}`);
}
"#,
    );
}

async fn response_body(response: Response) -> String {
    let bytes = match to_bytes(response.into_body(), usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => panic!("read response body: {error}"),
    };
    match String::from_utf8(bytes.to_vec()) {
        Ok(body) => body,
        Err(error) => panic!("response body is not UTF-8: {error}"),
    }
}

#[test]
fn state_automation_renders_unready_withheld_and_absolute_deadline() {
    run_dashboard_javascript_test(
        r#"
import assert from 'node:assert/strict';
class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.textContent = ''; this.dataset = {}; this.style = {}; }
  appendChild(child) { this.children.push(child); return child; }
  setAttribute(key, value) { this[key] = value; }
  addEventListener() {}
}
globalThis.document = {createElement: tag => new Element(tag), createTextNode: text => ({textContent:text})};
globalThis.window = {location: {search:''}};
const {renderAutomation} = await import('./automation.js');
const panel = renderAutomation({reason:'fresh_unready',state:{consumer:'host/ws/routine/pilot',members:{
  pending:{}, assessed:{task:{ready:false,resulting_fingerprint:'f'}},withheld:{other:'human_block'},failed:{},
  active:{member:{key:'task'},attempt:2,max_attempts:2,deadline:'2026-09-06T12:00:00Z',action_id:'run'}
},unresolved:{}},receipts:[],waivers:[]}, 'routine:pilot:automation');
function text(node) { return [node.textContent,...(node.children||[]).map(text)].join(' '); }
const rendered=text(panel);
assert.match(rendered,/State automation/);
assert.match(rendered,/fresh_unready/);
assert.match(rendered,/human_block/);
assert.match(rendered,/2026-09-06T12:00:00Z/);
assert.match(rendered,/Unknown/);
assert.match(rendered,/does not authorize promotion/);
assert.doesNotMatch(rendered,/Examined through/);
"#,
    );
}

/// ORB-11655: `refreshDashboard` rebuilds every Operations panel on a 30 s
/// timer the operator did not trigger. Disclosure state and an unapplied
/// cadence choice are operator state, not payload state, and must survive it.
#[test]
fn dashboard_operations_refresh_keeps_open_details_and_an_unapplied_cadence() {
    run_dashboard_javascript_test(
        r#"
const created = [];
class Node {
  constructor(id = "", tag = "") { this.id = id; this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; this.open = false; created.push(this); }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  append(...children) { for (const child of children) this.appendChild(child); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  get classList() { return { add: () => {}, toggle: () => {} }; }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  insertBefore(child, before) { const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); return child; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = { getElementById: get, createElement: (tag) => new Node("", tag), createTextNode: (text) => Object.assign(new Node(), { textContent: text }), body: new Node("body") };
globalThis.window = { confirm: () => true, location: new URL("http://dashboard.test/"), addEventListener: () => {} };

const routines = {
  host_id: "hm_local",
  session_explanation: "session access",
  capabilities: { routine_toggle: { authorized: true }, clock_service: { authorized: true }, clock_cadence: { authorized: true } },
  routines: [{
    name: "pilot", source: "one", target: "orbit.workflow.auto", enabled: true, effective: true,
    pinned_to_host: true, cron: "*/5 * * * *", hosts: ["hm_local"], description: "pilot routine",
    next_evaluation: { state: "scheduled", at: "2026-09-08T01:00:00Z" }, last_fire: null,
    automation: {
      reason: "not_due", ownership: { owned_here: true }, receipts: [], waivers: [],
      state: { consumer: "hm_local/one/routine/pilot", baseline: null, observed: null, covered: null, pending: [], pending_commits: [], unresolved: {} },
    },
  }],
  clock: { provider: "systemd", enabled: true, health: "healthy", schedulable: true, loaded: true, running: true, configured_cadence_seconds: 60, effective_cadence_seconds: 60, next_tick_at: "2026-09-08T01:00:00Z", last_tick_at: null },
};
const payloads = {
  "/api/routines": routines,
  "/api/auto-tasks": { definitions: [], capabilities: {} },
  "/api/workflows/auto/readiness": { tasks: [], capacity: {}, controls_authorized: true },
  "/api/operation/explain": { policy: {}, authority: {}, delivery: {}, limiting_reasons: [], controls_authorized: true },
};
globalThis.fetch = async (path) => {
  const url = String(path).split("?")[0];
  const payload = payloads[url] || {};
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};

const { setWorkspace } = await import("./common.js");
const { initOperations, fetchAndRenderOperations } = await import("./operations.js");
setWorkspace("one");
initOperations({ getWorkspaces: () => [{ id: "one", name: "one", status: "active" }], formatAbsoluteTime: (value) => value });

const latest = (predicate) => created.filter(predicate).pop();
const automationPanel = () => latest((node) => node.className === "automation-diagnostic");
const routineDetails = () => latest((node) => node.className === "operation-details" && node.textContent.includes("pilot routine"));
const cadenceSelect = () => latest((node) => node.tag === "select" && node.title === "Clock cadence");
const applyButton = () => latest((node) => node.textContent === "Apply cadence");
const chosenCadence = (select) => select.children.filter((option) => option.selected).map((option) => option.value);

await fetchAndRenderOperations();
if (automationPanel().open) throw new Error("an automation diagnostic must start closed");

// The operator opens both disclosures and picks a cadence without applying it.
for (const panel of [routineDetails(), automationPanel()]) {
  if (typeof panel.listeners.toggle !== "function") throw new Error("a disclosure must record its open state on toggle");
  panel.open = true;
  panel.listeners.toggle();
}
const select = cadenceSelect();
select.value = "300";
select.listeners.change();
if (applyButton().disabled) throw new Error("a changed cadence must enable Apply");

// The 30 s tick: same payload, every panel rebuilt.
await fetchAndRenderOperations();

if (!routineDetails().open) throw new Error("the routine details snapped shut on refresh");
if (!automationPanel().open) throw new Error("the automation diagnostic snapped shut on refresh");
const rechosen = chosenCadence(cadenceSelect());
if (JSON.stringify(rechosen) !== JSON.stringify(["300"])) throw new Error(`refresh reverted the cadence choice to ${JSON.stringify(rechosen)}`);
if (applyButton().disabled) throw new Error("refresh disabled Apply for a still-unapplied cadence");

// Once the host reports the chosen cadence as configured, the select follows
// the payload again rather than pinning the stale choice forever.
routines.clock.configured_cadence_seconds = 300;
await fetchAndRenderOperations();
if (!applyButton().disabled) throw new Error("Apply must be disabled once the chosen cadence is the configured one");
routines.clock.configured_cadence_seconds = 900;
await fetchAndRenderOperations();
const configured = chosenCadence(cadenceSelect());
if (JSON.stringify(configured) !== JSON.stringify(["900"])) throw new Error(`the select must track the configured cadence again, got ${JSON.stringify(configured)}`);
"#,
    );
}

/// An enabled delivery consumer this host cannot admit for must say so where
/// the operator reads it, instead of an empty "no baseline recorded" panel.
#[test]
fn delivery_automation_renders_the_ownership_admission_blocker() {
    run_dashboard_javascript_test(
        r#"
import assert from 'node:assert/strict';
class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.textContent = ''; this.dataset = {}; this.style = {}; }
  appendChild(child) { this.children.push(child); return child; }
  setAttribute(key, value) { this[key] = value; }
  addEventListener() {}
}
globalThis.document = {createElement: tag => new Element(tag), createTextNode: text => ({textContent:text})};
globalThis.window = {location: {search:''}};
const {renderAutomation} = await import('./automation.js');
function text(node) { return [node.textContent,...(node.children||[]).map(text)].join(' '); }

const unresolved = text(renderAutomation({reason:'ownership_unresolved',state:null,receipts:[],waivers:[],
  ownership:{authority:'missing',owned_here:false}}, 'a:automation'));
assert.match(unresolved,/ownership_unresolved/);
assert.match(unresolved,/No owner machine is registered/);
assert.match(unresolved,/set owner_machine on the definition/);

const elsewhere = text(renderAutomation({reason:'owned_elsewhere',state:null,receipts:[],waivers:[],
  ownership:{owner_machine:'hm_other',authority:'workspace',owned_here:false}}, 'b:automation'));
assert.match(elsewhere,/Owned by machine hm_other/);

const conflicting = text(renderAutomation({reason:'ownership_unresolved',state:null,receipts:[],waivers:[],
  ownership:{authority:'conflicting',owned_here:false}}, 'c:automation'));
assert.match(conflicting,/contradictory/);

const owned = text(renderAutomation({reason:'not_due',receipts:[],waivers:[],
  ownership:{owner_machine:'hm_local',authority:'workspace',owned_here:true},
  state:{consumer:'hm_local/ws/auto-task/delivery-qa',baseline:{commit:'a',tree:'b'},
    observed:{commit:'a',tree:'b'},covered:{commit:'a',tree:'b'},pending:[],pending_commits:[],unresolved:{}}}, 'd:automation'));
assert.doesNotMatch(owned,/Owned by machine/);
"#,
    );
}

/// The task-detail image preview, driven through the shipped `tasks.js` module
/// against a DOM double rather than asserted against source text: what matters
/// is that a PNG artifact renders as an `<img>` a reader can actually see, that
/// SVG still downloads, and that a decode failure degrades to the bytes.
#[test]
fn dashboard_task_detail_renders_image_artifacts_at_desktop_and_narrow_widths() {
    let css = include_str!("../../assets/dashboard/dashboard.css");
    // Responsiveness is a stylesheet contract, so it is checked where it lives:
    // the element scales to its column and keeps its aspect ratio, and narrow
    // viewports bound the height so a tall screenshot cannot take over the page.
    assert!(css.contains(".artifact-image"));
    assert!(css.contains("max-width: 100%"));
    assert!(css.contains("max-height: 60vh"));

    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(tag = "") { this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.hidden = false; }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  replaceChildren(...nodes) { this.children = []; this._text = ""; for (const node of nodes) this.appendChild(node); }
  removeChild(child) { this.children = this.children.filter((c) => c !== child); child.parentNode = null; return child; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((c) => c.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get classList() { const self = this; return { add: (...c) => { self.className = `${self.className} ${c.join(" ")}`.trim(); }, remove: () => {}, toggle: () => {} }; }
  querySelectorAll() { return []; }
}
const descend = (node, predicate) => {
  for (const child of node.children || []) {
    if (predicate(child)) return child;
    const found = descend(child, predicate);
    if (found) return found;
  }
  return null;
};
const byTag = (node, tag) => descend(node, (n) => n.tag === tag);
globalThis.document = {
  getElementById: () => new Node(),
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
globalThis.window = { location: new URL("http://dashboard.test/"), innerWidth: 1280 };

const revoked = [];
globalThis.URL.createObjectURL = (blob) => `blob:${blob.__kind}`;
globalThis.URL.revokeObjectURL = (url) => revoked.push(url);

// Byte-exact synthetic PNG: signature plus filler, the same fixture shape the
// Rust tests use. Nothing here comes from a real user image.
const PNG_BYTES = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]);

const requested = [];
function respondWith(kind, contentType) {
  return {
    ok: true,
    status: 200,
    headers: { get: (name) => (name.toLowerCase() === "content-type" ? contentType : null) },
    blob: async () => ({ __kind: kind }),
    text: async () => "plain body",
  };
}
let nextResponse = respondWith("png", "image/png");
globalThis.fetch = async (path) => { requested.push(String(path)); return nextResponse; };

const { buildArtifacts } = await import("./tasks.js");

async function renderPreview(artifact, response) {
  nextResponse = response;
  const wrap = buildArtifacts({ id: "ORB-00042", artifacts: [artifact] });
  const row = wrap.children[0];
  const preview = wrap.children[1];
  await row.listeners.click({ stopPropagation() {} });
  return { wrap, row, preview };
}

// --- A PNG artifact renders as a real image with open/download controls -----
const png = { path: "diagrams/flow.png", media_type: "image/png", size_bytes: PNG_BYTES.length };
let { row, preview } = await renderPreview(png, respondWith("png", "image/png"));

if (!requested.includes("/api/tasks/ORB-00042/artifacts/diagrams/flow.png"))
  throw new Error(`preview did not fetch the artifact route: ${requested}`);
if (!row.textContent.includes("image/png"))
  throw new Error(`the metadata row must stay compact and typed: ${row.textContent}`);

const img = byTag(preview, "img");
if (!img) throw new Error(`a PNG artifact must render an <img>, got: ${preview.textContent}`);
if (img.src !== "blob:png") throw new Error(`image src was not the fetched blob: ${img.src}`);
if (img.alt !== "diagrams/flow.png") throw new Error(`image needs descriptive alt text: ${img.alt}`);
if (!String(img.className).includes("artifact-image"))
  throw new Error(`image must carry the responsive class: ${img.className}`);

const collectLinks = (node) => {
  const found = [];
  const visit = (n) => { for (const c of n.children || []) { if (c.tag === "a") found.push(c); visit(c); } };
  visit(node);
  return found;
};
let links = collectLinks(preview);
const open = links.find((a) => a.textContent === "Open");
const download = links.find((a) => a.textContent === "Download");
if (!open || open.href !== "blob:png" || open.target !== "_blank")
  throw new Error("an image preview must offer an Open control in a new tab");
if (!download || download.download !== "flow.png")
  throw new Error("an image preview must offer a Download control with the file name");

// --- A decode failure degrades to the bytes instead of a broken image -------
img.listeners.error();
if (byTag(preview, "img")) throw new Error("a failed image must be removed, not left broken");
if (!preview.textContent.includes("could not be decoded"))
  throw new Error(`a decode failure must be explained: ${preview.textContent}`);
const fallback = collectLinks(preview).filter((a) => a.download === "flow.png");
if (fallback.length !== 1)
  throw new Error(`a failed image must still offer its bytes exactly once, got ${fallback.length}`);

// --- Narrow viewport renders the same image element ------------------------
window.innerWidth = 420;
({ preview } = await renderPreview(png, respondWith("png", "image/png")));
const narrowImg = byTag(preview, "img");
if (!narrowImg || !String(narrowImg.className).includes("artifact-image"))
  throw new Error("the narrow-width render lost the responsive image preview");

// --- SVG is an image format that must still download, never render ---------
window.innerWidth = 1280;
const svg = { path: "diagrams/active.svg", media_type: "image/svg+xml", size_bytes: 40 };
({ preview } = await renderPreview(svg, respondWith("svg", "application/octet-stream")));
if (byTag(preview, "img"))
  throw new Error("SVG hosts script and must never be rendered inline");
const svgLink = collectLinks(preview).find((a) => a.download === "active.svg");
if (!svgLink) throw new Error(`SVG must fall back to a download link: ${preview.textContent}`);

// --- Text artifacts keep working ------------------------------------------
const md = { path: "notes/summary.md", media_type: "text/markdown", size_bytes: 11 };
({ preview } = await renderPreview(md, respondWith("md", "text/plain")));
if (byTag(preview, "img")) throw new Error("a text artifact must not render as an image");
if (!preview.textContent.includes("plain body"))
  throw new Error(`text preview regressed: ${preview.textContent}`);
"#,
    );
}

/// [ORB-11332] The operation-mode panel projects `orbit operation explain`
/// and offers only the two governed grant controls; enablement is not a
/// dashboard action.
#[test]
fn dashboard_operation_mode_panel_is_explained_governed_and_guarded() {
    let index = include_str!("../../assets/dashboard/index.html");
    let operations = include_str!("../../assets/dashboard/operations.js");

    for id in [
        "operation-mode-panel",
        "operation-mode-count",
        "operation-mode-operation-feedback",
        "operation-mode-body",
    ] {
        assert!(index.contains(&format!(r#"id="{id}""#)), "{id}");
    }
    assert!(
        operations.contains(r#""/api/operation/explain""#),
        "the panel must project the runtime explanation, not recompute policy"
    );
    assert!(
        operations.contains("postJson(`/api/operation/${kind}`"),
        "stop and revoke must post through the governed endpoints"
    );
    assert!(
        operations.contains("expected_revision: authority.revision"),
        "grant controls must carry the rendered revision for compare-and-set"
    );
    assert!(
        operations.contains("Changing a preference activates nothing")
            && operations.contains("no grant authorizes merge"),
        "the panel must state that preferences activate nothing and merge is never granted"
    );
    assert!(
        operations.contains("Active grant policy (captured at enablement")
            && operations.contains("Current preferences (apply to a future grant only).")
            && operations.contains("const grantPolicy = authority.policy"),
        "an active grant must show the captured policy separately from current preferences"
    );
    assert!(
        !operations.contains("/api/operation/enable"),
        "enablement stays a CLI/MCP operator decision"
    );
    assert!(
        operations.contains("if (pendingOperations.has(key)) return;")
            && operations.contains("pendingOperations.delete(key);"),
        "grant controls must guard against duplicate submission"
    );
}

/// The rendered panel: policy fields with sources, the grant, limiting
/// reasons, and a stop click that posts the compare-and-set request.
#[test]
fn dashboard_operation_mode_renders_sources_and_posts_a_guarded_stop() {
    run_dashboard_javascript_test(
        r#"
const nodes = [];
class Node {
  constructor(id = "") { this.id = id; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.hidden = false; this.disabled = false; nodes.push(this); }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  append(...children) { for (const child of children) this.appendChild(child); }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  insertBefore(child, before) { const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); return child; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
globalThis.document = { getElementById: get, createElement: () => new Node(), createTextNode: (text) => Object.assign(new Node(), { textContent: text }), body: new Node("body") };
let confirmed = false;
globalThis.window = { confirm: () => { confirmed = true; return true; }, location: new URL("http://dashboard.test/"), addEventListener: () => {}, localStorage: { getItem: () => null, setItem: () => {} } };
const requests = [];
const explanation = {
  policy: { preset: { value: "supervised", source: "workspace" }, leaf_ceiling: { value: 5, source: "preset:supervised@workspace" }, preparation: { value: "manual", source: "preset:supervised@workspace" }, promotion: { value: "separate_approval", source: "preset:supervised@workspace" }, completion: { value: "review", source: "preset:supervised@workspace" }, recovery: { value: "existing", source: "preset:supervised@workspace" }, review_policy: { value: "after-landing", source: "workspace" }, review_crew: { value: "reviewers", source: "workspace" }, review_reviewer_starts: { value: 2, source: "built-in" }, review_repair_cycles: { value: 2, source: "built-in" }, review_minutes: { value: 30, source: "built-in" }, delivery_cap: { value: "done", source: "workspace" } },
  authority: { grant_id: "ogrant-1", status: "active", admission: "open", rights: ["prepare", "promote"], task_ids: ["ORB-1", "ORB-2"], expires_at: "2026-09-07T12:00:00Z", revision: 3, policy: { preset: { value: "autonomous", source: "workspace" }, leaf_ceiling: { value: 10, source: "preset:autonomous@workspace" }, preparation: { value: "automatic", source: "preset:autonomous@workspace" }, promotion: { value: "automatic", source: "preset:autonomous@workspace" }, completion: { value: "done", source: "preset:autonomous@workspace" }, recovery: { value: "scheduled", source: "preset:autonomous@workspace" }, review_policy: { value: "before-pr", source: "workspace" }, review_crew: { value: "reviewers", source: "workspace" }, review_reviewer_starts: { value: 2, source: "built-in" }, review_repair_cycles: { value: 2, source: "built-in" }, review_minutes: { value: 30, source: "built-in" }, delivery_cap: { value: "review", source: "built-in" } } },
  delivery: { effective_completion: "review", cap: "delivery_cap_review" },
  limiting_reasons: ["delivery_cap_review"],
  controls_authorized: true,
};
globalThis.fetch = async (path, opts = {}) => {
  const url = String(path); requests.push({ url, method: opts.method || "GET", body: opts.body ? JSON.parse(opts.body) : null });
  const payload = url.startsWith("/api/operation/explain") ? explanation
    : url.startsWith("/api/operation/stop") ? { grant_id: "ogrant-1", outcome: "stopped", revision: 4 }
    : {};
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};
const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const { setWorkspace } = await import("./common.js");
const { initOperations, fetchAndRenderOperationMode } = await import("./operations.js");
setWorkspace("one");
initOperations({ getWorkspaces: () => [{ id: "one", name: "one", status: "active" }], formatAbsoluteTime: (value) => value });
await fetchAndRenderOperationMode();
const body = get("operation-mode-body");
const text = body.textContent;
for (const expected of ["Active grant policy (captured at enablement", "Current preferences (apply to a future grant only).", "autonomous [workspace]", "10 [preset:autonomous@workspace]", "before-pr [workspace]", "supervised [workspace]", "5 [preset:supervised@workspace]", "after-landing [workspace]", "reviewers [workspace]", "Reviewer starts / lineage", "2 [built-in]", "30 [built-in]", "review (cap: delivery_cap_review)", "ogrant-1", "Limiting reasons: delivery_cap_review", "prepare, promote", "2 task(s)"]) {
  if (!text.includes(expected)) throw new Error(`panel is missing ${JSON.stringify(expected)} in: ${text}`);
}
if (!get("operation-mode-count").textContent.includes("open")) throw new Error("count must show the grant admission");
const buttons = nodes.filter((node) => node.listeners.click && (node.textContent === "Stop grant" || node.textContent === "Revoke grant"));
if (buttons.length !== 2) throw new Error(`expected two grant controls, found ${buttons.length}`);
const stop = buttons.find((node) => node.textContent === "Stop grant");
if (stop.disabled) throw new Error("stop must be enabled for an open, authorized grant");
await stop.listeners.click();
await tick(); await tick();
const posted = requests.find((request) => request.url.startsWith("/api/operation/stop"));
if (!confirmed) throw new Error("stop must confirm before posting");
if (!posted || posted.method !== "POST") throw new Error(`stop did not post: ${JSON.stringify(requests)}`);
if (posted.body.grant_id !== "ogrant-1" || posted.body.expected_revision !== 3) throw new Error(`stop body lacked the compare-and-set revision: ${JSON.stringify(posted.body)}`);
if (!get("operation-mode-operation-feedback").textContent.includes("stopped")) throw new Error("feedback must report the outcome");
if (requests.filter((request) => request.url.startsWith("/api/operation/explain")).length < 2) throw new Error("the panel must refresh after a control");

// Unauthorized callers see the controls disabled, never a silent no-op.
explanation.controls_authorized = false;
await fetchAndRenderOperationMode();
const disabled = nodes.filter((node) => node.listeners.click && node.textContent === "Revoke grant").pop();
if (!disabled.disabled) throw new Error("controls must be disabled for an unauthorized caller");
"#,
    );
}

/// ORB-11691: Jump to task ids must accept non-ORB prefixes, look up in the
/// selected workspace, and probe concrete workspaces from the aggregate view,
/// not the server default. The live failure was Diagnostics/Errors on ws_orbit with
/// `window=24h&run_state=failed`: GET /api/tasks/ORB-11514 (no workspace) 404'd
/// against polaris while the same id existed as blocked in ws_orbit.
#[test]
fn dashboard_global_task_jump_scopes_to_selected_workspace_and_distinguishes_errors() {
    let app = include_str!("../../assets/dashboard/app.js");
    let index = include_str!("../../assets/dashboard/index.html");
    assert!(app.contains(r#"const ID_RE = /^[A-Z]{2,5}-\d+$/i;"#));
    assert!(index.contains(r#"placeholder="Jump to task id"#));

    run_dashboard_javascript_test(
        r##"
class Node {
  constructor(id = "") {
    this.id = id;
    this.children = [];
    this.dataset = {};
    this.style = { setProperty: () => {}, display: "" };
    this.listeners = {};
    this.className = "";
    this._text = "";
    this.parentNode = null;
    this.hidden = false;
    this.disabled = false;
    this.value = "";
    this.offsetWidth = 40;
    this.offsetLeft = 0;
  }
  appendChild(child) {
    if (child == null) return child;
    this.children.push(child);
    child.parentNode = this;
    return child;
  }
  insertBefore(child, before) {
    const index = this.children.indexOf(before);
    if (index < 0) return this.appendChild(child);
    this.children.splice(index, 0, child);
    child.parentNode = this;
    return child;
  }
  removeChild(child) {
    this.children = this.children.filter((candidate) => candidate !== child);
    child.parentNode = null;
    return child;
  }
  replaceChildren(...next) {
    for (const child of this.children) child.parentNode = null;
    this.children = [];
    for (const child of next) this.appendChild(child);
  }
  prepend(child) { this.children.unshift(child); child.parentNode = this; return child; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) { this._text = String(value); this.children = []; }
  get innerHTML() { return this.textContent; }
  get firstChild() { return this.children[0] || null; }
  get lastElementChild() { return this.children[this.children.length - 1] || null; }
  querySelectorAll() { return []; }
  querySelector() { return null; }
  scrollIntoView() {}
  get classList() {
    const self = this;
    const tokens = () => self.className.split(/\s+/).filter(Boolean);
    const add = (...names) => { self.className = [...new Set([...tokens(), ...names])].join(" "); };
    const remove = (...names) => {
      const drop = new Set(names);
      self.className = tokens().filter((token) => !drop.has(token)).join(" ");
    };
    return {
      add,
      remove,
      contains: (name) => tokens().includes(name),
      toggle: (name, on) => {
        if (on === undefined) on = !tokens().includes(name);
        if (on) add(name); else remove(name);
      },
    };
  }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
const tabs = ["tasks", "audit", "diagnostics", "operations", "knowledge"].map((tab) => Object.assign(new Node(), { dataset: { tab } }));
const panes = [...tabs, Object.assign(new Node(), { dataset: { tab: "run-detail" } })];
const tabsStrip = new Node("tabs");
tabsStrip.className = "tabs";
const wrap = get("global-id-wrap");
wrap.className = "global-id-wrap";
wrap.appendChild(get("global-task-id"));
wrap.appendChild(get("global-task-id-error"));
globalThis.document = {
  body: new Node("body"),
  hidden: false,
  getElementById: get,
  createElement: () => new Node(),
  createElementNS: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
  querySelectorAll: (selector) => {
    if (selector === ".tab") return tabs;
    if (selector === ".tab-pane") return panes;
    if (selector === "#task-filter .chip") return get("task-filter").children;
    if (selector === "#tasks-body .row") return get("tasks-body").children.filter((node) => String(node.className).includes("row"));
    return [];
  },
  querySelector: (selector) => {
    if (selector === ".tabs") return tabsStrip;
    const tabMatch = /^\.tab\[data-tab="([^"]+)"\]$/.exec(selector || "");
    if (tabMatch) return tabs.find((tab) => tab.dataset.tab === tabMatch[1]) || null;
    return new Node();
  },
  addEventListener: () => {},
};
const location = new URL("http://dashboard.test/?workspace=ws_orbit&window=24h&run_state=failed");
location.hash = "#diagnostics/errors?window=24h";
const hashListeners = [];
globalThis.window = {
  location,
  innerHeight: 900,
  addEventListener: (name, fn) => { if (name === "hashchange") hashListeners.push(fn); },
  matchMedia: () => ({ addEventListener: () => {}, matches: false }),
  localStorage: { getItem: () => null, setItem: () => {} },
};
Object.defineProperty(globalThis.window, "location", {
  configurable: true,
  get: () => location,
  set: () => {},
});
Object.defineProperty(location, "hash", {
  configurable: true,
  get() { return this._hash || ""; },
  set(value) {
    const next = String(value || "");
    const normalized = next.startsWith("#") ? next : `#${next}`;
    if (this._hash === normalized) return;
    this._hash = normalized;
    for (const fn of hashListeners) fn();
  },
});
location._hash = "#diagnostics/errors?window=24h";
globalThis.history = { replaceState: (_, __, url) => { const next = new URL(String(url), location.href); location.search = next.search; location.pathname = next.pathname; } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.requestAnimationFrame = (fn) => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { constructor() {} close() {} };

const existing = { id: "DANI-00012", title: "Enforce proc.spawn filesystem policy against indirect child access", status: "blocked", history: [], artifacts: [], comments: [] };
let lookupMode = "existing";
let delayed = null;
const requests = [];
function json(payload, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => payload, text: async () => JSON.stringify(payload) };
}
globalThis.fetch = async (path) => {
  const url = new URL(String(path), "http://dashboard.test");
  requests.push(url.pathname + url.search);
  if (url.pathname === "/api/workspaces") {
    return json([
      { id: "ws_polaris", name: "polaris", status: "active", is_default: true },
      { id: "ws_orbit", name: "orbit", status: "active", is_default: false },
    ]);
  }
  if (/^\/api\/tasks\/(?:ORB-|DANI-)/.test(url.pathname)) {
    const id = decodeURIComponent(url.pathname.slice("/api/tasks/".length));
    const workspace = url.searchParams.get("workspace");
    if (lookupMode === "network" && id === "ORB-00001") throw new Error("offline");
    if (lookupMode === "denied" && id === "ORB-00002") return json({ error: "cross-origin requests not allowed" }, 403);
    if (lookupMode === "server" && id === "ORB-00003") return json({ error: "boom" }, 500);
    if (lookupMode === "stale" && id === "DANI-00012") {
      await new Promise((resolve) => { delayed = resolve; });
      return workspace === "ws_orbit" ? json(existing) : json({ error: `task not found: ${id}` }, 404);
    }
    if (id === "DANI-00012" && workspace === "ws_orbit") return json(existing);
    return json({ error: `task not found: ${id}` }, 404);
  }
  if (url.pathname === "/api/tasks" || url.pathname === "/api/tasks/all") {
    return json({ items: [], total: 0, limit: 50, truncated: false });
  }
  return json([]);
};

const originalSetTimeout = setTimeout;
globalThis.setTimeout = (fn, ms, ...args) => originalSetTimeout(fn, ms === 250 ? 0 : ms, ...args);
const tick = () => new Promise((resolve) => originalSetTimeout(resolve, 0));

await import("./app.js");
await tick(); await tick(); await tick();

const { getWorkspace, setWorkspace } = await import("./common.js");
if (getWorkspace() !== "ws_orbit") throw new Error(`selected workspace should remain ws_orbit, got ${getWorkspace()}`);

const input = get("global-task-id");
const err = get("global-task-id-error");
function jump(id) {
  input.value = id;
  if (input.listeners.keydown) input.listeners.keydown({ key: "Enter", preventDefault() {} });
  else input.listeners.input();
}

requests.length = 0;
jump("dani-00012");
await tick(); await tick(); await tick(); await tick(); await tick();
const taskGets = requests.filter((url) => url.startsWith("/api/tasks/DANI-00012"));
if (!taskGets[0] || !taskGets[0].includes("workspace=ws_orbit")) {
  throw new Error(`existing-task jump must query the selected workspace first; got ${JSON.stringify(taskGets)}`);
}
if (err.textContent.includes("not found")) throw new Error(`existing task reported missing: ${err.textContent}`);
if (wrap.classList.contains("error")) throw new Error("successful jump must not leave the error state");
if (input.value) throw new Error("successful jump should clear the input");
if (!String(location.hash).includes("tasks")) throw new Error(`successful jump should open Tasks, hash=${location.hash}`);
const opened = get("tasks-body").children.some((node) => String(node.textContent).includes("DANI-00012"));
if (!opened) throw new Error("existing blocked task must render after jump");

setWorkspace("");
requests.length = 0;
jump("DANI-00012");
await tick(); await tick(); await tick(); await tick(); await tick();
const aggregateGets = requests.filter((url) => url.startsWith("/api/tasks/DANI-00012"));
if (aggregateGets.length < 2 || !aggregateGets[0].includes("workspace=ws_polaris") || !aggregateGets.some((url) => url.includes("workspace=ws_orbit"))) {
  throw new Error(`aggregate lookup must probe concrete workspaces; got ${JSON.stringify(aggregateGets)}`);
}
if (getWorkspace() !== "ws_orbit") throw new Error(`aggregate lookup should adopt the owner, got ${getWorkspace()}`);
if (err.textContent.includes("Error 400")) throw new Error(`aggregate lookup must not expose a missing workspace: ${err.textContent}`);

lookupMode = "missing";
requests.length = 0;
jump("ORB-99999");
await tick(); await tick(); await tick();
if (err.textContent !== "ORB-99999 not found") throw new Error(`missing task copy: ${err.textContent}`);
if (!wrap.classList.contains("error") || wrap.classList.contains("pending")) throw new Error("confirmed miss must use the error state");

lookupMode = "network";
jump("ORB-00001");
await tick(); await tick(); await tick();
if (err.textContent !== "Network error resolving ORB-00001") throw new Error(`transport copy: ${err.textContent}`);
if (err.textContent.includes("not found")) throw new Error("transport failure must not look like a miss");

lookupMode = "denied";
jump("ORB-00002");
await tick(); await tick(); await tick();
if (err.textContent !== "Lookup denied for ORB-00002") throw new Error(`denied copy: ${err.textContent}`);

lookupMode = "server";
jump("ORB-00003");
await tick(); await tick(); await tick();
if (err.textContent !== "Server error resolving ORB-00003") throw new Error(`server copy: ${err.textContent}`);

lookupMode = "stale";
const hashBeforeStale = String(location.hash);
input.value = "DANI-00012";
jump("DANI-00012");
await tick();
setWorkspace("ws_polaris");
if (typeof delayed === "function") delayed();
await tick(); await tick(); await tick();
if (String(location.hash) !== hashBeforeStale) throw new Error(`stale lookup overwrote navigation: ${location.hash}`);
if (!input.value) throw new Error("stale lookup must not clear the jump input as a success");
"##,
    );
}

#[test]
fn operations_actions_preserve_capabilities_feedback_and_workspace_identity() {
    let dom = r#"
class Node {
  constructor() { this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this._text = ""; }
  appendChild(child) { this.children.push(child); return child; }
  append(...children) { children.forEach(child => this.appendChild(child)); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get textContent() { return this._text + this.children.map(child => child.textContent).join(""); }
  setAttribute(name, value) { this[name] = String(value); }
  getAttribute(name) { return this[name]; }
  addEventListener(name, fn) { this.listeners[name] = fn; }
  click() { if (!this.disabled) return this.listeners.click?.(); }
  dispatchEvent(event) { return this.listeners[event.type]?.(event); }
  insertBefore(child, before) { const index = this.children.indexOf(before); if (index < 0) return this.appendChild(child); this.children.splice(index, 0, child); return child; }
}
const nodes = new Map();
globalThis.document = {
  getElementById: id => { if (!nodes.has(id)) nodes.set(id, new Node()); return nodes.get(id); },
  createElement: () => new Node(),
};
globalThis.window = { location: new URL("http://dashboard.test"), localStorage: { getItem: () => null } };
"#;
    run_dashboard_javascript_test(&format!(
        "{dom}\n{}",
        include_str!("dashboard_operations.mjs")
    ));
}

#[test]
fn dashboard_loading_rejects_stale_responses_and_reports_panel_errors() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_loading_dom.mjs"),
        include_str!("dashboard_loading.mjs")
    ));
}

// ORB-11658: the dashboard's primary interaction is expanding a row, and the
// log dock's level filters gate what the operator can even see. Both were
// pointer-only. Two things carry the fix and are asserted separately: the
// shipped markup (a real <button> is what makes Space activate a pill, and
// document order is what makes Tab reach a row from the search box), and the
// shipped modules (role, tab stop, aria state, and the key handlers), which the
// Node scenario drives directly.
#[test]
fn dashboard_log_dock_controls_are_real_toggle_buttons() {
    let index = include_str!("../../assets/dashboard/index.html");

    for control in [
        r#"<button type="button" class="filter-pill on" data-filter="all" aria-pressed="true">all</button>"#,
        r#"<button type="button" class="filter-pill" data-filter="err" aria-pressed="false">err</button>"#,
        r#"<button type="button" class="filter-pill" data-filter="deny" aria-pressed="false">deny</button>"#,
        r#"<button type="button" class="filter-pill" data-filter="warn" aria-pressed="false">warn</button>"#,
        r#"<button type="button" class="seg right on" id="log-follow-tail" title="Follow the tail" aria-pressed="true">"#,
        r#"<button type="button" class="count" id="log-buffered-count""#,
    ] {
        assert!(
            index.contains(control),
            "the log dock must ship this control as a pressable button: {control}"
        );
    }
    assert!(
        !index.contains(r#"<span class="filter-pill"#),
        "no log filter may remain a <span>"
    );

    // Tab order is document order: the search box has to come before the rows
    // it filters for "Tab from the search box reaches the first task row".
    let search = index
        .find(r#"id="task-search""#)
        .expect("task search input must exist");
    let rows = index
        .find(r#"id="tasks-body""#)
        .expect("task list body must exist");
    assert!(
        search < rows,
        "the task search box must precede the task rows in document order"
    );
}

#[test]
fn dashboard_rows_are_keyboard_operable_without_changing_click_behaviour() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_keyboard_dom.mjs"),
        include_str!("dashboard_keyboard.mjs")
    ));
}

// The focus ring is the other half of keyboard operability: a row that can be
// focused but shows nothing is not usable.
#[test]
fn dashboard_css_shows_focus_on_every_operable_row() {
    let css = include_str!("../../assets/dashboard/dashboard.css");

    for selector in [
        ".row:focus-visible",
        ".artifact-row:focus-visible",
        ".audit-row:focus-visible",
        ".step-row:focus-visible",
        ".runs-row:focus-visible",
        ".field-block.collapsible h4:focus-visible",
        ".log-foot .filter-pill:focus-visible",
    ] {
        assert!(
            css.contains(selector),
            "{selector} must have a visible focus ring"
        );
    }
}
