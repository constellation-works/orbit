use axum::body::to_bytes;
use axum::http::{HeaderMap, HeaderValue, header};
use flate2::read::GzDecoder;
use std::fs;
use std::io::Read;
use std::process::Command;

use crate::{DASHBOARD_CSP, DASHBOARD_FILES, serve_dashboard_file};

fn copy_dashboard_javascript(source: &std::path::Path, destination: &std::path::Path) {
    for entry in fs::read_dir(source).expect("read dashboard asset directory") {
        let path = entry.expect("read dashboard asset entry").path();
        let target = destination.join(path.file_name().expect("dashboard asset name"));
        if path.is_dir() {
            fs::create_dir_all(&target).expect("create dashboard asset directory copy");
            copy_dashboard_javascript(&path, &target);
        } else if path.extension().is_some_and(|extension| extension == "js") {
            fs::copy(&path, target).expect("copy shipped dashboard JavaScript module");
        }
    }
}

fn run_dashboard_javascript_test(script: &str) {
    let temp_dir =
        tempfile::tempdir().expect("create temporary dashboard JavaScript test directory");
    let assets_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/dashboard");
    copy_dashboard_javascript(&assets_dir, temp_dir.path());
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

#[test]
fn dashboard_routes_emit_csp() {
    for &(route, _, _) in DASHBOARD_FILES {
        let response = serve_dashboard_file(route, &HeaderMap::new());
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&HeaderValue::from_static(DASHBOARD_CSP)),
            "{route} route must emit the dashboard CSP"
        );
    }
}

#[tokio::test]
async fn dashboard_assets_emit_validators_and_revalidate() {
    let initial = serve_dashboard_file("/", &HeaderMap::new());
    let etag = initial
        .headers()
        .get(header::ETAG)
        .cloned()
        .expect("dashboard assets must emit an ETag");

    assert_eq!(initial.status(), axum::http::StatusCode::OK);
    assert_eq!(
        initial.headers().get(header::CACHE_CONTROL),
        Some(&HeaderValue::from_static("no-cache"))
    );

    let mut request_headers = HeaderMap::new();
    request_headers.insert(header::IF_NONE_MATCH, etag.clone());
    let revalidated = serve_dashboard_file("/", &request_headers);

    assert_eq!(revalidated.status(), axum::http::StatusCode::NOT_MODIFIED);
    assert_eq!(revalidated.headers().get(header::ETAG), Some(&etag));
    assert_eq!(
        revalidated.headers().get(header::CONTENT_SECURITY_POLICY),
        Some(&HeaderValue::from_static(DASHBOARD_CSP))
    );
    assert!(
        to_bytes(revalidated.into_body(), usize::MAX)
            .await
            .expect("read 304 response body")
            .is_empty()
    );
}

#[tokio::test]
async fn dashboard_assets_serve_precompressed_gzip_bodies() {
    let plain = to_bytes(
        serve_dashboard_file("/", &HeaderMap::new()).into_body(),
        usize::MAX,
    )
    .await
    .expect("read uncompressed dashboard body");
    let mut request_headers = HeaderMap::new();
    request_headers.insert(
        header::ACCEPT_ENCODING,
        HeaderValue::from_static("br, gzip;q=1.0"),
    );

    let compressed = serve_dashboard_file("/", &request_headers);
    assert_eq!(
        compressed.headers().get(header::CONTENT_ENCODING),
        Some(&HeaderValue::from_static("gzip"))
    );
    assert_eq!(
        compressed.headers().get(header::VARY),
        Some(&HeaderValue::from_static("Accept-Encoding"))
    );

    let compressed_body = to_bytes(compressed.into_body(), usize::MAX)
        .await
        .expect("read compressed dashboard body");
    let mut decoder = GzDecoder::new(compressed_body.as_ref());
    let mut decoded = Vec::new();
    decoder
        .read_to_end(&mut decoded)
        .expect("decompress dashboard body");
    assert_eq!(decoded, plain.as_ref());
}

#[tokio::test]
async fn dashboard_index_self_hosts_markdown_runtime() {
    for route in [
        "/static/vendor/marked.umd.js",
        "/static/vendor/purify.min.js",
    ] {
        let response = serve_dashboard_file(route, &HeaderMap::new());
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "{route} must be served"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static(
                "application/javascript; charset=utf-8"
            )),
            "{route} must be served as JavaScript"
        );
        assert!(
            !to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read vendor asset")
                .is_empty(),
            "{route} must have a body"
        );
    }
}

#[tokio::test]
async fn dashboard_self_hosts_fonts_without_google_requests() {
    for route in [
        "/static/fonts/geist-latin.woff2",
        "/static/fonts/geist-mono-latin.woff2",
    ] {
        let response = serve_dashboard_file(route, &HeaderMap::new());
        assert_eq!(
            response.status(),
            axum::http::StatusCode::OK,
            "{route} must be served"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("font/woff2")),
            "{route} must have font MIME type"
        );
        assert!(
            !to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read font body")
                .is_empty(),
            "{route} must have a body"
        );
    }
}

#[test]
fn dashboard_markdown_call_sites_use_sanitizing_wrapper() {
    let wrapper = include_str!("../../assets/dashboard/js/markdown.js");
    let app = include_str!("../../assets/dashboard/app.js");
    let tasks = include_str!("../../assets/dashboard/js/tasks.js");
    let plugins = include_str!("../../assets/dashboard/js/plugins.js");
    assert!(
        wrapper.contains("purifier.sanitize("),
        "markdown wrapper must sanitize rendered HTML before DOM insertion"
    );
    for (name, source) in [("app", app), ("tasks", tasks), ("plugins", plugins)] {
        assert!(
            !source.contains("marked.parse("),
            "{name} must not bypass the sanitizing markdown wrapper"
        );
        assert!(
            source.contains("renderMarkdown("),
            "{name} must call the sanitizing markdown wrapper"
        );
    }
    assert!(
        !plugins.contains("innerHTML = source"),
        "plugin source must not be assigned as raw innerHTML"
    );
}

#[tokio::test]
async fn dashboard_scoreboard_module_is_served() {
    let response = serve_dashboard_file("/static/js/scoreboard.js", &HeaderMap::new());
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&HeaderValue::from_static(
            "application/javascript; charset=utf-8"
        ))
    );
    assert!(
        !to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read served scoreboard module")
            .is_empty(),
        "the scoreboard module route must serve a nonempty body"
    );
}

#[test]
fn dashboard_resume_disables_while_in_flight_and_links_the_live_run_on_conflict() {
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
  get classList() { return { add: (c) => { if (!this.className.split(/\s+/).includes(c)) this.className = `${this.className} ${c}`.trim(); }, toggle: () => {} }; }
  querySelectorAll(selector) { const found = []; const visit = (node) => { for (const child of node.children) { if (selector === ".action-error" && child.className.split(/\s+/).includes("action-error")) found.push(child); visit(child); } }; visit(this); return found; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node(id)), byId.get(id));
const find = (node, predicate) => { for (const child of node.children) { if (predicate(child)) return child; const hit = find(child, predicate); if (hit) return hit; } return null; };
globalThis.document = {
  getElementById: get,
  createElement: () => new Node(),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
globalThis.window = { location: new URL("http://dashboard.test/"), innerWidth: 1200, confirm: () => true };
globalThis.history = { replaceState: () => {} };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });

const requests = [];
let respond = null;
globalThis.fetch = (path) => {
  requests.push(String(path));
  return new Promise((resolve) => { respond = resolve; });
};

const runs = [{ run_id: "jrun-source", job_id: "task_pr_pipeline", state: "interrupted", created_at: "2026-09-25T03:24:00Z" }];
let navigated = null;
const { initRuns, renderRuns } = await import("./js/runs.js");
initRuns({
  getLastRuns: () => runs,
  getRunsMeta: () => ({ truncated: false }),
  navigateToRun: (runId) => { navigated = runId; },
  fetchAndRenderRuns: () => Promise.resolve(),
  getActiveRunId: () => null,
});
renderRuns(runs);
const body = get("runs-body");
const resumeButton = () => find(body, (node) => node.className.includes("run-resume"));

resumeButton().listeners.click({ stopPropagation() {} });
if (requests.length !== 1) throw new Error(`expected one resume request, got ${requests}`);
if (!resumeButton().disabled) throw new Error("resume must be disabled while its request is in flight");

renderRuns(runs);
const rebuilt = resumeButton();
if (!rebuilt.disabled) throw new Error("a re-render during the request must not re-enable resume");
rebuilt.listeners.click({ stopPropagation() {} });
if (requests.length !== 1) throw new Error(`a second click while in flight sent another request: ${requests}`);

const conflict = { error: "job run 'jrun-source' already has a live resume in its retry lineage (jrun-live)", code: "resume_run_in_flight", run_id: "jrun-live", source_run_id: "jrun-source" };
respond({ ok: false, status: 409, text: async () => JSON.stringify(conflict) });
await new Promise((resolve) => setTimeout(resolve, 0));

const error = find(body, (node) => node.className.includes("action-error"));
if (!error || !error.textContent.includes("jrun-live")) throw new Error(`conflict must name the live run: ${body.textContent}`);
const open = find(error, (node) => node.className.includes("resume-live-run"));
if (!open) throw new Error(`conflict must link to the live run: ${error.textContent}`);
open.listeners.click({ stopPropagation() {} });
if (navigated !== "jrun-live") throw new Error(`live-run link navigated to ${navigated}`);

renderRuns(runs);
if (resumeButton().disabled) throw new Error("resume must re-enable once the request settles");
"#,
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
const { fetchJson } = await import("./js/common.js");
const { setActiveRunEvents, setActiveRunEventsError, renderRunEvents } = await import("./js/run-detail.js");
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
responseStatus = 404;
await assert.rejects(fetchJson("/api/runs/jrun-1/events?limit=100"), (error) => error.status === 404);
setActiveRunEvents([]);
renderRunEvents();
assert.doesNotMatch(get("run-events-body").textContent, /bounded scan budget/);
"#,
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
const { renderTasks } = await import("./js/tasks.js");
const { initRouter, setActiveTab } = await import("./js/router.js");
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
let drainDock = 0;
initRouter({ showDrainDock: () => { drainDock += 1; }, setTab: (tab) => { selected = tab; }, getDiagSubtab: () => "runs", setDiagSubtab: () => {}, getOperationsSubtab: () => "routines", setOperationsSubtab: () => {}, getKnowledgeSubtab: () => "frictions", setKnowledgeSubtab: () => {}, getRunId: () => null, setRunId: () => {}, getRunSubtab: () => "steps", setRunSubtab: () => {}, getExpandedSteps: () => new Set(), setExpandedSteps: () => {}, setRunLogs: () => {}, refreshDashboard: () => {}, fitLogPanelToViewport: () => {}, });
setActiveTab("operations/auto-tasks", { refresh: false, updateHash: false });
if (selected !== "operations") throw new Error("route did not select the Operations view");
setActiveTab("auto-drain", { refresh: false, updateHash: false });
if (selected !== "tasks" || drainDock !== 1) throw new Error("retired #auto-drain did not open Tasks with the Drain dock");
setActiveTab("operations/auto-drain", { refresh: false, updateHash: false });
if (selected !== "tasks" || drainDock !== 2) throw new Error("legacy #operations/auto-drain did not open Tasks with the Drain dock");
await import("./app.js");
await tick(); await tick(); requests.length = 0;
const selector = get("rail-workspace").children.find((child) => child.id === "workspace-select");
selector.value = ""; selector.listeners.change(); await tick(); await tick();
if (!requests.some((path) => path.startsWith("/api/tasks/all?status=")) || requests.some((path) => ["/api/crews", "/api/tasks/locks", "/api/audit/summary"].some((forbidden) => path.startsWith(forbidden)))) throw new Error(`aggregate mode made incorrect requests: ${requests}`);
"#,
    );
}

#[test]
fn dashboard_renders_governed_transitions_and_marks_forced_targets() {
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
const { renderTasks } = await import("./js/tasks.js");

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}

const fixtures = [
  ["proposed", ["in-progress", "blocked", "backlog", "someday", "rejected", "archived"]],
  ["backlog", ["in-progress", "blocked", "proposed", "someday", "rejected", "archived"]],
  ["blocked", ["in-progress", "backlog", "archived"]],
  ["review", ["in-progress", "blocked", "backlog", "done", "rejected", "archived"]],
  ["done", []],
  ["archived", []],
];
for (const [status, targets] of fixtures) {
  const task = {
    id: `ORB-${status}`, title: `${status} task`, status, history: [], artifacts: [],
    status_transitions: targets.map((target) => ({ status: target, required_field: null })),
  };
  renderTasks([task], {
    getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
    getActiveStatuses: () => new Set([status]), statusOrder: statuses,
    fmtAbsTime: (value) => value, refreshDashboard: () => Promise.resolve(),
  });
  const select = find(get("tasks-body"), (node) => node.className === "task-status-select mono");
  if (!select) throw new Error(`status select did not render for ${status}`);
  const governed = select.children.filter((node) => node.value).map((option) => option.value);
  if (JSON.stringify(governed) !== JSON.stringify(targets)) {
    throw new Error(`${status} governed options ${JSON.stringify(governed)} != ${JSON.stringify(targets)}`);
  }

  const group = select.children.find((node) => node.label);
  const expectedForced = statuses.filter((candidate) => candidate !== status && !targets.includes(candidate));
  if (expectedForced.length === 0) {
    if (group) throw new Error(`${status} must not render an empty force group`);
  } else {
    if (!group) throw new Error(`${status} did not render the force group`);
    if (group.label !== "force (off-table)") {
      throw new Error(`${status} force group label was ${group.label}`);
    }
    const forced = group.children.map((option) => option.value);
    if (JSON.stringify(forced) !== JSON.stringify(expectedForced)) {
      throw new Error(`${status} forced options ${JSON.stringify(forced)} != ${JSON.stringify(expectedForced)}`);
    }
    for (const option of group.children) {
      if (!option.textContent.includes("⚠")) {
        throw new Error(`${status} forced option ${option.value} is not marked`);
      }
    }
  }

  const offered = governed.concat(expectedForced).sort();
  const everyOther = statuses.filter((candidate) => candidate !== status).sort();
  if (JSON.stringify(offered) !== JSON.stringify(everyOther)) {
    throw new Error(`${status} did not offer every other lifecycle status: ${JSON.stringify(offered)}`);
  }
  if (select.disabled) {
    throw new Error(`${status} status select must stay operable for a human override`);
  }
}
"#,
    );
}

#[test]
fn dashboard_collects_status_evidence_and_suppresses_invalid_reverse_undo() {
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
let promptValue = "";
globalThis.window = { location, addEventListener: () => {}, confirm: () => false, prompt: () => promptValue };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.setTimeout = () => 0;

let task = {
  id: "ORB-1", title: "Proposed task", status: "proposed", plan: "", execution_summary: "",
  history: [], artifacts: [],
  status_transitions: [{ status: "in-progress", required_field: "plan" }],
};
const requests = [];
globalThis.fetch = async (_path, options) => {
  const request = JSON.parse(options.body);
  requests.push(request);
  const statusTransitions = request.status === "blocked"
    ? [{ status: "backlog", required_field: null }]
    : [];
  task = {
    ...task, status: request.status, status_transitions: statusTransitions,
  };
  return { ok: true, text: async () => JSON.stringify(task) };
};
const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const context = {
  getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set([task.status]), statusOrder: statuses,
  replaceTask: (updated) => { task = updated; }, fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
};
const { renderTasks } = await import("./js/tasks.js");

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
function selectStatus(target) {
  const select = find(get("tasks-body"), (node) => node.className === "task-status-select mono");
  select.value = target;
  select.listeners.change({ stopPropagation: () => {} });
}

renderTasks([task], context);
selectStatus("in-progress");
if (requests.length !== 0) throw new Error("missing plan must not be submitted");
const unavailable = find(get("tasks-body"), (node) => node.className.includes("mutation-feedback error"));
if (!unavailable || !unavailable.textContent.includes("execution plan is required")) {
  throw new Error("missing plan reason was not shown");
}

promptValue = "1. implement and verify";
selectStatus("in-progress");
await new Promise(setImmediate);
if (requests.length !== 1 || requests[0].plan !== promptValue) {
  throw new Error(`plan evidence was not submitted: ${JSON.stringify(requests)}`);
}
if (find(get("tasks-body"), (node) => node.className === "mutation-undo")) {
  throw new Error("undo was offered for an invalid in-progress to proposed reverse transition");
}

task = {
  ...task, status: "review", execution_summary: "",
  status_transitions: [{ status: "done", required_field: "execution_summary" }],
};
promptValue = "Completed and verified";
renderTasks([task], context);
selectStatus("done");
await new Promise(setImmediate);
if (requests.length !== 2 || requests[1].execution_summary !== promptValue) {
  throw new Error(`completion evidence was not submitted: ${JSON.stringify(requests)}`);
}
if (find(get("tasks-body"), (node) => node.className === "mutation-undo")) {
  throw new Error("undo was offered for a terminal done task");
}

task = {
  ...task, status: "backlog",
  status_transitions: [{ status: "blocked", required_field: null }],
};
renderTasks([task], context);
selectStatus("blocked");
await new Promise(setImmediate);
if (requests.length !== 3 || requests[2].status !== "blocked") {
  throw new Error(`valid status change was not submitted: ${JSON.stringify(requests)}`);
}
if (!find(get("tasks-body"), (node) => node.className === "mutation-undo")) {
  throw new Error("undo was not offered for a valid blocked to backlog reverse transition");
}
if (requests.some((request) => "force" in request)) {
  throw new Error("a governed transition must not request a force override");
}
"#,
    );
}

/// ORB-12445: an off-table target is the human override. It costs one confirm
/// naming the move, and only then does the PATCH carry `force: true` — the same
/// escape hatch as `orbit task update --force`, which the server records as a
/// `forced` history event.
#[test]
fn dashboard_forces_off_table_status_only_after_one_confirmation() {
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
const confirmations = [];
let confirmed = false;
globalThis.window = {
  location,
  addEventListener: () => {},
  confirm: (message) => { confirmations.push(message); return confirmed; },
  prompt: () => { throw new Error("a forced transition must not prompt for evidence"); },
};
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.setTimeout = () => 0;

// A delivered task: the lifecycle table offers it nothing.
let task = {
  id: "ORB-1", title: "Delivered task", status: "done", plan: "1) do it",
  execution_summary: "did it", history: [], artifacts: [], status_transitions: [],
};
const requests = [];
globalThis.fetch = async (_path, options) => {
  const request = JSON.parse(options.body);
  requests.push(request);
  task = { ...task, status: request.status, status_transitions: [] };
  return { ok: true, text: async () => JSON.stringify(task) };
};
const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const context = {
  getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set(statuses), statusOrder: statuses,
  replaceTask: (updated) => { task = updated; }, fmtAbsTime: (value) => value,
  refreshDashboard: () => Promise.resolve(),
};
const { renderTasks } = await import("./js/tasks.js");

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
function selectStatus(target) {
  const select = find(get("tasks-body"), (node) => node.className === "task-status-select mono");
  select.value = target;
  select.listeners.change({ stopPropagation: () => {} });
}

renderTasks([task], context);
selectStatus("backlog");
await new Promise(setImmediate);
if (confirmations.length !== 1) {
  throw new Error(`a forced transition needs exactly one confirm: ${JSON.stringify(confirmations)}`);
}
if (!confirmations[0].includes("done") || !confirmations[0].includes("backlog")) {
  throw new Error(`the confirm must name the move: ${confirmations[0]}`);
}
if (requests.length !== 0) throw new Error("a declined confirm must not send the override");
if (task.status !== "done") throw new Error("a declined confirm must leave the status alone");

confirmed = true;
renderTasks([task], context);
selectStatus("backlog");
await new Promise(setImmediate);
if (confirmations.length !== 2) {
  throw new Error(`the accepted override needs its own confirm: ${JSON.stringify(confirmations)}`);
}
if (requests.length !== 1 || requests[0].status !== "backlog" || requests[0].force !== true) {
  throw new Error(`the forced status change was not submitted: ${JSON.stringify(requests)}`);
}
if ("plan" in requests[0] || "execution_summary" in requests[0]) {
  throw new Error("a forced transition must not fabricate lifecycle evidence");
}
if (task.status !== "backlog") throw new Error("the forced status was not applied");
const feedback = find(get("tasks-body"), (node) => node.className.includes("mutation-feedback"));
if (!feedback || !feedback.textContent.includes("forced")) {
  throw new Error(`the forced write was not reported as forced: ${feedback && feedback.textContent}`);
}
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
const { renderTasks } = await import("./js/tasks.js");

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

/// ORB-12645: comments are the dashboard's long-form channel — agents post
/// spec-length Markdown — so the thread renders as its own full-width panel
/// below the two detail columns: Markdown bodies, a cap with a fade over a
/// long one, per-comment actions, an outline for a sectioned body, and a
/// composer that says its draft is Markdown.
#[test]
fn dashboard_renders_task_comments_as_a_collapsible_markdown_thread() {
    run_dashboard_javascript_test(
        r###"
class Node {
  constructor(tag = "") { this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; this.scrolled = 0; }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const old = child.parentNode; if (old) old.children = old.children.filter((candidate) => candidate !== child); const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  replaceChildren(...next) { for (const child of this.children) child.parentNode = null; this.children = []; for (const child of next) this.appendChild(child); }
  replaceWith(next) { const parent = this.parentNode; if (!parent) return; parent.children = parent.children.map((candidate) => candidate === this ? next : candidate); next.parentNode = parent; this.parentNode = null; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  getAttribute(name) { return this[name] != null ? String(this[name]) : null; }
  focus() {}
  scrollIntoView() { this.scrolled += 1; }
  querySelectorAll(selector) { const out = []; const walk = (node) => { for (const child of node.children) { if (child.tag === selector) out.push(child); walk(child); } }; walk(this); return out; }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  get textContent() { return this.children.length > 0 ? this.children.map((child) => child.textContent || "").join("") : this._text; }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) {
    this._text = String(value);
    this.children = [];
    const regex = /<([a-z0-9]+)[^>]*>(.*?)<\/\1>/gis;
    let match;
    while ((match = regex.exec(this._text)) !== null) {
      const child = new Node(match[1].toLowerCase());
      child.textContent = match[2].replace(/<[^>]+>/g, "");
      this.appendChild(child);
    }
  }
  get innerHTML() { return this._text; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  set id(value) { this._id = String(value); byId.set(String(value), this); }
  get id() { return this._id || ""; }
  get classList() {
    const self = this;
    const tokens = () => new Set(String(self.className || "").split(/\s+/).filter(Boolean));
    const write = (set) => { self.className = [...set].join(" "); };
    return {
      add: (...names) => { const set = tokens(); for (const name of names) set.add(name); write(set); },
      remove: (...names) => { const set = tokens(); for (const name of names) set.delete(name); write(set); },
      contains: (name) => tokens().has(name),
      toggle: (name, on) => {
        const set = tokens();
        const next = on === undefined ? !set.has(name) : !!on;
        if (next) set.add(name); else set.delete(name);
        write(set);
        return next;
      },
    };
  }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node()), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
};
const store = new Map();
globalThis.window = {
  location: new URL("http://dashboard.test/#tasks"),
  addEventListener: () => {}, confirm: () => false,
  localStorage: { getItem: (key) => (store.has(key) ? store.get(key) : null), setItem: (key, value) => store.set(key, String(value)) },
};
window.location.href = "http://dashboard.test/#tasks?status=all";
const copied = [];
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: (text) => { copied.push(text); return Promise.resolve(); } } }, configurable: true });
globalThis.setTimeout = () => 0;
const sanitized = [];
globalThis.marked = {
  use: () => {},
  parse: (source) => String(source).split("\n").map((line) => {
    const heading = /^##\s+(.*)$/.exec(line);
    if (heading) return `<h2>${heading[1]}</h2>`;
    return line ? `<p>${line}</p>` : "";
  }).join(""),
  parseInline: (source) => String(source),
};
globalThis.DOMPurify = {
  isSupported: true,
  sanitize: (html) => { sanitized.push(String(html)); return String(html).replace(/<script[\s\S]*?<\/script>/g, "").replace(/ style="[^"]*"/g, ""); },
};
const liveObservers = new Set();
class FakeIntersectionObserver {
  constructor(callback, options = {}) {
    this.callback = callback;
    this.options = options;
    this.targets = new Set();
    this.disconnected = false;
    liveObservers.add(this);
  }
  observe(target) {
    if (this.disconnected) throw new Error("cannot observe on disconnected observer");
    this.targets.add(target);
  }
  unobserve(target) {
    this.targets.delete(target);
  }
  disconnect() {
    this.disconnected = true;
    this.targets.clear();
    liveObservers.delete(this);
  }
  trigger(entries) {
    this.callback(entries);
  }
}
globalThis.IntersectionObserver = FakeIntersectionObserver;

const paragraph = "A long paragraph line that easily runs past the rendered measure and wraps more than once in the card. ";
const longBody = [
  "## Findings", paragraph, paragraph, paragraph,
  "## Risks", paragraph, paragraph,
  "## Next steps", paragraph, paragraph, "- do the thing", "- then the other thing",
].join("\n");
const comments = [
  { at: "2026-09-19T10:00:00Z", by: "dani", message: "Looks right to me." },
  { at: "2026-09-20T09:00:00Z", by: "codex", message: longBody },
  { at: "2026-09-20T09:30:00Z", by: "dani", message: "<script>alert(1)</script> and <span style=\"color:red\">inline</span>" },
];
const task = { id: "ORB-1", title: "Threaded", status: "review", updated_at: "2026-09-20T10:00:00Z", history: [], artifacts: [], comments };
const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const context = {
  getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["review"]), statusOrder: statuses,
  statusUpdateTargets: statuses, fmtAbsTime: (value) => `abs:${value}`,
  refreshDashboard: () => Promise.resolve(),
};
const { renderTasks, scrollToComment } = await import("./js/tasks.js");
function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
function collect(node, predicate, out = []) {
  if (predicate(node)) out.push(node);
  for (const child of node.children || []) collect(child, predicate, out);
  return out;
}
const has = (node, name) => String(node.className).split(" ").includes(name);
const body = get("tasks-body");
renderTasks([task], context);
find(body, (node) => node.dataset.key === "task-ORB-1").listeners.click();
const detail = find(body, (node) => node.dataset.key === "detail-ORB-1");
if (!detail) throw new Error("the task detail did not render");

// 1. Comments block appears in the left/main column after review gate; the two-column grid has no full-width child besides the actions row
const leftCol = detail.children.find((node) => has(node, "detail-main"));
if (!leftCol) throw new Error("the main column did not render");
const sideCol = detail.children.find((node) => has(node, "detail-side"));
if (!sideCol) throw new Error("the side column did not render");
let panel = leftCol.children.find((node) => has(node, "comments-panel"));
if (!panel) throw new Error("the comments panel is not inside detail-main");
if (leftCol.children[leftCol.children.length - 1] !== panel) throw new Error("comments panel must be the last child of detail-main");
if (detail.children.some((node) => node !== leftCol && node !== sideCol && node !== detail.children[detail.children.length - 1])) {
  throw new Error("detail grid has unexpected full-width children");
}
if (find(panel, (node) => has(node, "field-count")).textContent !== "3") throw new Error("the panel must count its comments");
if (!has(panel, "collapsible")) throw new Error("the comments panel must be collapsible");

// Acceptance criteria: no inline style attributes
if (panel.style && (panel.style.display || panel.style.maxHeight)) {
  throw new Error("comments panel structure/collapse must not use inline style attributes");
}

// 1b. Block header collapses/expands with click and keyboard, chevron matches other blocks, state persists across refresh via localStorage
let panelHead = find(panel, (node) => node.tag === "h4");
if (!panelHead) throw new Error("the panel needs an h4 header");
if (panelHead.getAttribute("aria-expanded") !== "true") throw new Error("panel header must start expanded");
if (has(panel, "collapsed")) throw new Error("panel must start expanded when task has comments");

// Click header to collapse
panelHead.listeners.click({ stopPropagation: () => {} });
if (!has(panel, "collapsed")) throw new Error("clicking panel header must collapse the panel");
if (panelHead.getAttribute("aria-expanded") !== "false") throw new Error("collapsed panel must set aria-expanded=false");
let savedPrefs = JSON.parse(store.get("orbit.dashboard.comments") || "{}");
if (savedPrefs.collapsed !== true) throw new Error(`collapsed pref must persist: ${store.get("orbit.dashboard.comments")}`);

// State persists across refresh via localStorage
task.updated_at = "2026-09-20T10:15:00Z";
renderTasks([task], context);
let refreshedDetail = find(get("tasks-body"), (node) => node.dataset.key === "detail-ORB-1");
let refreshedLeft = refreshedDetail.children.find((node) => has(node, "detail-main"));
panel = refreshedLeft.children.find((node) => has(node, "comments-panel"));
if (!has(panel, "collapsed")) throw new Error("comments panel must stay collapsed across refresh");
panelHead = find(panel, (node) => node.tag === "h4");
if (panelHead.getAttribute("aria-expanded") !== "false") throw new Error("refreshed panel must keep aria-expanded=false");

// Keyboard toggle expands the panel
panelHead.listeners.keydown({ key: "Enter", target: panelHead, preventDefault: () => {}, stopPropagation: () => {} });
if (has(panel, "collapsed")) throw new Error("keyboard toggle must expand the panel");
if (panelHead.getAttribute("aria-expanded") !== "true") throw new Error("keyboard toggle must set aria-expanded=true");
savedPrefs = JSON.parse(store.get("orbit.dashboard.comments") || "{}");
if (savedPrefs.collapsed !== false) throw new Error("expanded pref must persist");

// 1c. Permalink #comment-<task>-<n> expands a collapsed panel before scrolling to the card
panelHead.listeners.click({ stopPropagation: () => {} });
if (!has(panel, "collapsed")) throw new Error("panel should be collapsed before permalink test");
const targetCard = find(panel, (node) => node.id === "comment-ORB-1-2");
const targetScrolledBefore = targetCard.scrolled;
scrollToComment("#comment-ORB-1-2");
if (has(panel, "collapsed")) throw new Error("permalink must expand the collapsed panel");
if (panelHead.getAttribute("aria-expanded") !== "true") throw new Error("expanded panel must update aria-expanded");
if (targetCard.scrolled <= targetScrolledBefore) throw new Error("permalink must scroll target card into view");
targetCard.scrolled = 0;

// 1d. Permalink #comment-<task>-<n> in address bar scrolls target card into view once; repeated renders do not re-scroll or overwrite collapse preference
window.location.hash = "#comment-ORB-1-2";
renderTasks([task], context);
if (targetCard.scrolled !== 1) throw new Error(`initial render with comment hash must scroll target card into view once, got ${targetCard.scrolled}`);

// A second render (e.g. 30 s background poll) must not scroll the card again
renderTasks([task], context);
if (targetCard.scrolled !== 1) throw new Error(`second render with comment hash must not scroll target card again, got ${targetCard.scrolled}`);

// Collapsing the comments block while that hash is present persists collapsed: true
panelHead.listeners.click({ stopPropagation: () => {} });
if (!has(panel, "collapsed")) throw new Error("clicking panel header must collapse the panel");
savedPrefs = JSON.parse(store.get("orbit.dashboard.comments") || "{}");
if (savedPrefs.collapsed !== true) throw new Error(`collapsed preference must persist in store: ${store.get("orbit.dashboard.comments")}`);

// A subsequent refresh while that hash is present stays collapsed and does not overwrite stored preference or re-scroll
task.updated_at = "2026-09-20T10:20:00Z";
renderTasks([task], context);
refreshedDetail = find(get("tasks-body"), (node) => node.dataset.key === "detail-ORB-1");
refreshedLeft = refreshedDetail.children.find((node) => has(node, "detail-main"));
panel = refreshedLeft.children.find((node) => has(node, "comments-panel"));
if (!has(panel, "collapsed")) throw new Error("comments panel must stay collapsed across refresh with comment hash");
panelHead = find(panel, (node) => node.tag === "h4");
if (panelHead.getAttribute("aria-expanded") !== "false") throw new Error("refreshed panel must keep aria-expanded=false");
savedPrefs = JSON.parse(store.get("orbit.dashboard.comments") || "{}");
if (savedPrefs.collapsed !== true) throw new Error("stored collapsed preference must not be overwritten by refresh");
const refreshedTargetCard = find(panel, (node) => node.id === "comment-ORB-1-2");
if (refreshedTargetCard.scrolled !== 0) throw new Error(`refreshed card must not be scrolled: ${refreshedTargetCard.scrolled}`);
if (targetCard.scrolled !== 1) throw new Error(`prior card scroll count must remain 1: ${targetCard.scrolled}`);

// Re-expand panel for remaining tests
panelHead.listeners.click({ stopPropagation: () => {} });
if (has(panel, "collapsed")) throw new Error("re-expanding panel must remove collapsed");

const cards = () => collect(panel, (node) => has(node, "comment-card"));
if (cards().length !== 3) throw new Error(`expected three cards, got ${cards().length}`);
if (cards()[0].id !== "comment-ORB-1-1") throw new Error(`oldest first by default: ${cards()[0].id}`);
if (liveObservers.size !== 0) throw new Error("collapsed comment must not register an observer");

// 2. a long comment is capped, lists its sections, and opens and closes.
const long = cards()[1];
const summary = find(long, (node) => has(node, "comment-summary"));
if (!summary.textContent.includes("Findings") || !summary.textContent.includes("Next steps")) throw new Error(`the collapsed card must list its sections: ${summary.textContent}`);
const toggle = find(long, (node) => has(node, "comment-toggle"));
toggle.listeners.click({ stopPropagation: () => {} });
if (liveObservers.size !== 1) throw new Error(`expanding a multi-section comment must register an IntersectionObserver, got ${liveObservers.size}`);
const observer = Array.from(liveObservers)[0];
if (observer.targets.size !== 3) throw new Error(`observer must observe each rendered h2, got ${observer.targets.size}`);
const short = cards()[0];
if (has(short, "long") || find(short, (node) => has(node, "comment-foot")).style.display !== "none") throw new Error("a short comment must not be capped or carry a disclosure footer");

// 3. the outline appears for a body with three or more sections and highlights as headings intersect.
const outline = find(long, (node) => has(node, "comment-outline"));
const links = collect(outline, (node) => has(node, "comment-outline-link"));
if (links.length !== 3 || links[0].textContent !== "Findings") throw new Error(`the outline must name each section: ${links.map((l) => l.textContent)}`);
const headings = collect(long, (node) => node.tag === "h2");
if (headings.length !== 3) throw new Error("long comment must render three h2 headings");
observer.trigger([{ target: headings[1], isIntersecting: true }]);
if (!has(links[1], "active") || has(links[0], "active")) throw new Error("scrolling past heading must highlight outline entry");
observer.trigger([{ target: headings[2], isIntersecting: true }]);
if (!has(links[2], "active") || has(links[1], "active")) throw new Error("scrolling past next heading must move outline highlight");

// 4. identity, time and the per-comment actions.
const head = find(long, (node) => has(node, "comment-head"));
if (!find(head, (node) => has(node, "comment-agent-pill"))) throw new Error("an agent-written comment needs its pill");
if (!head.textContent.includes("abs:2026-09-20T09:00:00Z")) throw new Error(`the card needs the absolute time: ${head.textContent}`);
if (!find(head, (node) => has(node, "comment-age")).textContent.trim()) throw new Error("the card needs a relative time beside the absolute one");
if (find(cards()[0], (node) => has(node, "comment-agent-pill"))) throw new Error("a human comment must not be pilled as an agent");
const raw = find(long, (node) => node.title && node.title.includes("Markdown source"));
raw.listeners.click({ stopPropagation: () => {} });
if (liveObservers.size !== 0) throw new Error("switching comment to raw must disconnect outline observer");
const rawView = find(long, (node) => has(node, "comment-raw"));
if (rawView.style.display === "none" || !rawView.textContent.includes("## Findings")) throw new Error("raw must show the original text");
if (find(long, (node) => has(node, "comment-body")).style.display !== "none") throw new Error("raw must replace the rendered body");
raw.listeners.click({ stopPropagation: () => {} });
if (liveObservers.size !== 1) throw new Error("restoring rendered view must reconnect outline observer");
if (rawView.style.display !== "none") throw new Error("raw must toggle back");
find(long, (node) => node.title && node.title.startsWith("Copy")).listeners.click({ stopPropagation: () => {} });
if (copied[copied.length - 1] !== longBody) throw new Error("copy must yield the comment's Markdown");
const permalink = find(long, (node) => has(node, "permalink"));
permalink.listeners.click({ stopPropagation: () => {} });
if (long.scrolled !== 1) throw new Error("the permalink must scroll to its comment");
if (copied[copied.length - 1] !== "http://dashboard.test/#comment-ORB-1-2") throw new Error(`unexpected permalink: ${copied[copied.length - 1]}`);

// 5. bodies render as Markdown, sanitized.
const renderedBody = find(long, (node) => has(node, "comment-body"));
if (!renderedBody.innerHTML.includes("<h2>Findings</h2>")) throw new Error(`the body must render Markdown: ${renderedBody.innerHTML.slice(0, 80)}`);
if (sanitized.length === 0) throw new Error("rendering must go through the sanitizing wrapper");
const hostile = find(cards()[2], (node) => has(node, "comment-body"));
if (hostile.innerHTML.includes("<script") || hostile.innerHTML.includes("style=")) throw new Error(`a hostile comment rendered live: ${hostile.innerHTML}`);

// 6. thread order is an operator preference that persists; collapse all folds the thread.
const orderToggle = find(panel, (node) => node.textContent === "oldest first");
orderToggle.listeners.click({ stopPropagation: () => {} });
if (cards()[0].id !== "comment-ORB-1-3") throw new Error(`newest first did not reorder: ${cards()[0].id}`);
if (!String(store.get("orbit.dashboard.comments")).includes("true")) throw new Error("the order preference must persist");
if (liveObservers.size !== 1) throw new Error(`reordering thread must not leak observers: got ${liveObservers.size}`);
for (const obs of liveObservers) {
  for (const target of obs.targets) {
    let curr = target;
    while (curr.parentNode) curr = curr.parentNode;
    if (curr !== body) throw new Error("observed targets must belong to attached comment cards after reorder");
  }
}
const reopened = cards().find((card) => card.id === "comment-ORB-1-2");
if (!has(reopened, "expanded")) throw new Error("a rebuilt card must keep its disclosure");
find(panel, (node) => node.textContent === "collapse all").listeners.click({ stopPropagation: () => {} });
if (!has(cards().find((card) => card.id === "comment-ORB-1-2"), "collapsed")) throw new Error("collapse all must fold the thread");
if (liveObservers.size !== 0) throw new Error("collapse all must disconnect all outline observers");

// 7. the refresh keeps the operator's disclosure and does not leak observers or retain detached nodes.
find(cards().find((card) => card.id === "comment-ORB-1-2"), (node) => has(node, "comment-toggle")).listeners.click({ stopPropagation: () => {} });
if (liveObservers.size !== 1) throw new Error("re-expanding comment must register outline observer");
for (let i = 0; i < 5; i++) {
  renderTasks([task], context);
}
if (liveObservers.size !== 1) throw new Error(`repeated unchanged refreshes must not accumulate observers, got ${liveObservers.size}`);
for (const obs of liveObservers) {
  for (const target of obs.targets) {
    let curr = target;
    while (curr.parentNode) curr = curr.parentNode;
    if (curr !== body) throw new Error("detached card retained live observer after unchanged refresh");
  }
}
task.updated_at = "2026-09-20T11:00:00Z";
renderTasks([task], context);
const rebuilt = find(get("tasks-body"), (node) => node.dataset.key === "detail-ORB-1");
if (rebuilt === detail) throw new Error("the detail should have been rebuilt by the changed task");
const rebuiltCard = find(rebuilt, (node) => node.id === "comment-ORB-1-2");
if (!has(rebuiltCard, "expanded")) throw new Error("the refresh dropped the operator's expansion");
if (liveObservers.size !== 1) throw new Error(`rebuilding detail on changed task must not leak observers, got ${liveObservers.size}`);
for (const obs of liveObservers) {
  for (const target of obs.targets) {
    let curr = target;
    while (curr.parentNode) curr = curr.parentNode;
    if (curr !== body) throw new Error("detached card retained live observer after task rebuild");
  }
}
find(body, (node) => node.dataset.key === "task-ORB-1").listeners.click();
if (liveObservers.size !== 0) throw new Error("collapsing task row must disconnect detail outline observers");
find(body, (node) => node.dataset.key === "task-ORB-1").listeners.click();
if (liveObservers.size !== 1) throw new Error("reopening task row must restore outline observer for expanded comment");

// 8. the composer states that Markdown is rendered and previews the draft.
find(rebuilt, (node) => node.className === "action comment").listeners.click({ stopPropagation: () => {} });
const form = find(rebuilt, (node) => node.className === "comment-form");
const textarea = find(form, (node) => node.tag === "textarea");
if (!textarea.placeholder.includes("Markdown")) throw new Error(`the placeholder must say Markdown is rendered: ${textarea.placeholder}`);
textarea.value = "## Draft\nbody";
const previewToggle = find(form, (node) => node.className === "action preview");
previewToggle.listeners.click({ stopPropagation: () => {} });
const preview = find(form, (node) => has(node, "comment-preview"));
if (preview.style.display === "none" || !preview.innerHTML.includes("<h2>Draft</h2>")) throw new Error(`the preview must render the draft: ${preview.innerHTML}`);
previewToggle.listeners.click({ stopPropagation: () => {} });
if (preview.style.display !== "none") throw new Error("the preview must toggle back off");
"###,
    );
}

/// ORB-12656: permalink #comment-<task>-<n> in the address bar must restore
/// (scroll into view) exactly once across multiple renders (such as the 30 s
/// poll or status changes), and collapsing the thread must persist across
/// refresh rather than being force-expanded or overwriting commentPrefs.
#[test]
fn dashboard_comment_permalink_hash_renders_twice_and_scrolls_once() {
    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(tag = "") { this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; this.disabled = false; this.scrolled = 0; }
  appendChild(child) { if (child == null) return child; this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) { const old = child.parentNode; if (old) old.children = old.children.filter((candidate) => candidate !== child); const index = this.children.indexOf(before); this.children.splice(index < 0 ? this.children.length : index, 0, child); child.parentNode = this; return child; }
  removeChild(child) { this.children = this.children.filter((candidate) => candidate !== child); child.parentNode = null; return child; }
  replaceChildren(...next) { for (const child of this.children) child.parentNode = null; this.children = []; for (const child of next) this.appendChild(child); }
  replaceWith(next) { const parent = this.parentNode; if (!parent) return; parent.children = parent.children.map((candidate) => candidate === this ? next : candidate); next.parentNode = parent; this.parentNode = null; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  getAttribute(name) { return this[name] != null ? String(this[name]) : null; }
  focus() {}
  scrollIntoView() { this.scrolled += 1; }
  querySelectorAll(selector) { const out = []; const walk = (node) => { for (const child of node.children) { if (child.tag === selector) out.push(child); walk(child); } }; walk(this); return out; }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  get textContent() { return this.children.length > 0 ? this.children.map((child) => child.textContent || "").join("") : this._text; }
  set textContent(value) { this._text = String(value); this.children = []; }
  set innerHTML(value) {
    this._text = String(value);
    this.children = [];
    const regex = /<([a-z0-9]+)[^>]*>(.*?)<\/\1>/gis;
    let match;
    while ((match = regex.exec(this._text)) !== null) {
      const child = new Node(match[1].toLowerCase());
      child.textContent = match[2].replace(/<[^>]+>/g, "");
      this.appendChild(child);
    }
  }
  get innerHTML() { return this._text; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  set id(value) { this._id = String(value); byId.set(String(value), this); }
  get id() { return this._id || ""; }
  get classList() {
    const self = this;
    const tokens = () => new Set(String(self.className || "").split(/\s+/).filter(Boolean));
    const write = (set) => { self.className = [...set].join(" "); };
    return {
      add: (...names) => { const set = tokens(); for (const name of names) set.add(name); write(set); },
      remove: (...names) => { const set = tokens(); for (const name of names) set.delete(name); write(set); },
      contains: (name) => tokens().has(name),
      toggle: (name, on) => {
        const set = tokens();
        const next = on === undefined ? !set.has(name) : !!on;
        if (next) set.add(name); else set.delete(name);
        write(set);
        return next;
      },
    };
  }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node()), byId.get(id));
globalThis.document = {
  getElementById: get,
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
};
const store = new Map();
globalThis.window = {
  location: new URL("http://dashboard.test/#comment-ORB-1-2"),
  addEventListener: () => {}, confirm: () => false,
  localStorage: { getItem: (key) => (store.has(key) ? store.get(key) : null), setItem: (key, value) => store.set(key, String(value)) },
};
const comments = [
  { at: "2026-09-19T10:00:00Z", by: "dani", message: "First comment." },
  { at: "2026-09-20T09:00:00Z", by: "codex", message: "Target comment." },
];
const task = { id: "ORB-1", title: "Permalink", status: "review", updated_at: "2026-09-20T10:00:00Z", history: [], artifacts: [], comments };
const statuses = ["in-progress", "review", "blocked", "done"];
const context = {
  getTasks: () => [task], getTasksMeta: () => null, getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["review"]), statusOrder: statuses,
  statusUpdateTargets: statuses, fmtAbsTime: (value) => `abs:${value}`,
  refreshDashboard: () => Promise.resolve(),
};
const { renderTasks } = await import("./js/tasks.js");
function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}
const has = (node, name) => String(node.className).split(" ").includes(name);
const body = get("tasks-body");

// Initial render with #comment-ORB-1-2 hash in address bar, row collapsed
renderTasks([task], context);

// Expanding the task row renders the detail and scrolls comment-ORB-1-2 into view exactly once
find(body, (node) => node.dataset.key === "task-ORB-1").listeners.click();
const targetCard = find(body, (node) => node.id === "comment-ORB-1-2");
if (!targetCard) throw new Error("target card not found after expanding row");
if (targetCard.scrolled !== 1) throw new Error(`initial render must scroll target card once, got ${targetCard.scrolled}`);

// Second render (simulating 30 s poll) must not scroll target card again
renderTasks([task], context);
if (targetCard.scrolled !== 1) throw new Error(`second render must not scroll target card again, got ${targetCard.scrolled}`);

// Collapsing comments panel persists collapsed: true
let panel = find(body, (node) => has(node, "comments-panel"));
let panelHead = find(panel, (node) => node.tag === "h4");
panelHead.listeners.click({ stopPropagation: () => {} });
if (!has(panel, "collapsed")) throw new Error("comments panel must be collapsed after click");
let savedPrefs = JSON.parse(store.get("orbit.dashboard.comments") || "{}");
if (savedPrefs.collapsed !== true) throw new Error("collapsed preference must persist in store");

// A subsequent refresh while hash is still present stays collapsed and does not overwrite preference
task.updated_at = "2026-09-20T10:30:00Z";
renderTasks([task], context);
panel = find(get("tasks-body"), (node) => has(node, "comments-panel"));
if (!has(panel, "collapsed")) throw new Error("comments panel must stay collapsed across refresh");
panelHead = find(panel, (node) => node.tag === "h4");
if (panelHead.getAttribute("aria-expanded") !== "false") throw new Error("panel header must keep aria-expanded=false");
savedPrefs = JSON.parse(store.get("orbit.dashboard.comments") || "{}");
if (savedPrefs.collapsed !== true) throw new Error("stored collapsed preference must not be overwritten by refresh");
const refreshedTargetCard = find(panel, (node) => node.id === "comment-ORB-1-2");
if (refreshedTargetCard.scrolled !== 0) throw new Error(`refreshed card must not be scrolled: ${refreshedTargetCard.scrolled}`);
if (targetCard.scrolled !== 1) throw new Error(`prior card scroll count must remain 1: ${targetCard.scrolled}`);
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
const { renderAuditSummary } = await import("./js/audit.js");
const { renderDiagnosticsSideCard } = await import("./js/diagnostics.js");

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
const { initLogTail } = await import("./js/log-tail.js");
initLogTail();
await tick();
await tick();
if (sources.length !== 1) throw new Error(`expected one EventSource, got ${sources.length}`);
if (!sources[0].url.includes("from=42")) throw new Error(`stream url missing snapshot offset: ${sources[0].url}`);
sources[0].readyState = EventSource.CLOSED;
sources[0].onerror();
if (retryFns().length !== 1) throw new Error(`expected one retry timer, got ${retryFns().length}`);
retryFns()[0]();
if (sources.length !== 2) throw new Error(`retry did not open a new EventSource, got ${sources.length}`);
if (!sources[1].url.includes("from=42")) throw new Error(`retry lost resume offset: ${sources[1].url}`);
sources[1].readyState = EventSource.OPEN;
sources[1].onopen();
"#,
    );
}

#[test]
fn dashboard_task_detail_offers_archive_for_non_archived_tasks_and_omits_for_archived() {
    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(tag = "") {
    this.tag = tag;
    this.children = [];
    this.dataset = {};
    this.style = {};
    this.listeners = {};
    this.className = "";
    this._text = "";
    this.parentNode = null;
    this.disabled = false;
  }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  insertBefore(child, before) {
    const old = child.parentNode;
    if (old) old.children = old.children.filter((c) => c !== child);
    const index = this.children.indexOf(before);
    this.children.splice(index < 0 ? this.children.length : index, 0, child);
    child.parentNode = this;
    return child;
  }
  removeChild(child) {
    this.children = this.children.filter((c) => c !== child);
    child.parentNode = null;
    return child;
  }
  replaceChildren(...next) { for (const child of this.children) child.parentNode = null; this.children = []; for (const child of next) this.appendChild(child); }
  replaceWith(next) { const parent = this.parentNode; if (!parent) return; parent.children = parent.children.map((c) => c === this ? next : c); next.parentNode = parent; this.parentNode = null; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  focus() {}
  get textContent() { return this._text + this.children.map((c) => c.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get lastElementChild() { return this.children[this.children.length - 1]; }
  get classList() {
    return {
      add: (...names) => { this.className = `${this.className} ${names.join(" ")}`.trim(); },
      remove: () => {},
      toggle: () => {},
    };
  }
}
const nodes = new Map();
const get = (id) => nodes.get(id) || (nodes.set(id, new Node()), nodes.get(id));
globalThis.document = {
  getElementById: get,
  createElement: (tag) => new Node(tag),
  createTextNode: (text) => Object.assign(new Node(), { textContent: text }),
  createDocumentFragment: () => new Node(),
};
const location = new URL("http://dashboard.test/#tasks");
globalThis.window = { location, addEventListener: () => {}, confirm: () => false };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.setTimeout = () => 0;

const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const { renderTasks } = await import("./js/tasks.js");

function find(node, predicate) {
  if (predicate(node)) return node;
  for (const child of node.children || []) { const match = find(child, predicate); if (match) return match; }
  return null;
}

for (const status of statuses) {
  // Even when status_transitions is empty (as for done, archived) or excludes archived (as for rejected):
  const task = {
    id: `ORB-${status}`,
    title: `${status} task`,
    status,
    history: [],
    artifacts: [],
    status_transitions: status === "done" || status === "archived" ? [] : [{ status: "backlog", required_field: null }],
  };
  renderTasks([task], {
    getTasks: () => [task],
    getTasksMeta: () => null,
    getSearchQuery: () => "",
    getActiveStatuses: () => new Set([status]),
    statusOrder: statuses,
    fmtAbsTime: (value) => value,
    refreshDashboard: () => Promise.resolve(),
  });

  const row = find(get("tasks-body"), (node) => node.dataset.key === `task-ORB-${status}`);
  if (!row || !row.listeners.click) throw new Error(`task row did not render for ${status}`);
  row.listeners.click();

  const detail = find(get("tasks-body"), (node) => node.dataset.key === `detail-ORB-${status}`);
  if (!detail) throw new Error(`task detail did not render for ${status}`);

  const archiveBtn = find(detail, (node) => node.className && node.className.includes("action archive"));
  if (status === "archived") {
    if (archiveBtn) throw new Error("archived task must not offer archive action");
  } else {
    if (!archiveBtn) throw new Error(`task with status '${status}' must offer archive action`);
  }
}
"#,
    );
}

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
if (JSON.stringify(offered) !== JSON.stringify(["low", "medium", "hard", "xhard"])) {
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
await saveField("properties", "dashboard, orbit-web", { tags: ["dashboard", "orbit-web"] });

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
  // ORB-12516: the task detail also reads claim provenance. It is read-only and
  // owned by its own scenario, so it is answered here and kept out of the
  // request log this harness asserts task writes against.
  if (String(path).startsWith("/api/distributed/claims")) {
    const empty = JSON.stringify({ schema_version: 1, owner_workspace: true, claims: [], capabilities: {} });
    return { ok: true, status: 200, text: async () => empty, json: async () => JSON.parse(empty) };
  }
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

const { setMultiWorkspace } = await import("./js/common.js");
const { renderTasks } = await import("./js/tasks.js");
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
  // The header carries its title in a span so it can also hold a count and the
  // field's edit button; tags live in the `properties` card beside complexity.
  const heading = (block) => find(block.children[0], (node) => node.className === "field-title");
  return blocks.find((block) => heading(block) && heading(block).textContent === title) || null;
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

#[test]
fn dashboard_audit_summary_renders_aggregate_and_every_failing_tool() {
    run_dashboard_javascript_test(
        r#"
class Node {
  constructor(tag = "") { this.tag = tag; this.children = []; this.dataset = {}; this.style = {}; this.listeners = {}; this.className = ""; this._text = ""; this.parentNode = null; }
  appendChild(child) { this.children.push(child); child.parentNode = this; return child; }
  addEventListener(name, callback) { this.listeners[name] = callback; }
  setAttribute(name, value) { this[name] = String(value); }
  get textContent() { return this._text + this.children.map((child) => child.textContent || "").join(""); }
  set textContent(value) { this._text = String(value); this.children = []; }
  get classList() { return { add: () => {}, toggle: () => {} }; }
}
const byId = new Map();
const get = (id) => byId.get(id) || (byId.set(id, new Node()), byId.get(id));
globalThis.document = { getElementById: get, createElement: (tag) => new Node(tag), createTextNode: (text) => Object.assign(new Node(), { textContent: text }) };
globalThis.window = { location: new URL("http://dashboard.test/#audit"), addEventListener: () => {} };

const { renderAuditSummary } = await import("./js/audit.js");
renderAuditSummary({
  window: "24h",
  tool_call_failure_rate: { failed: 7, total: 18, rate: 7 / 18 },
  tool_call_failures_by_tool: [
    { tool: "orbit.task.update", failed: 3, total: 7, rate: 3 / 7 },
    { tool: "orbit.search", failed: 2, total: 7, rate: 2 / 7 },
    { tool: "orbit.task.add", failed: 1, total: 1, rate: 1 },
    { tool: "orbit.task.show", failed: 1, total: 3, rate: 1 / 3 },
  ],
  failure_rate_by_tool: [
    { tool: "orbit.search", rate: 2 / 7, failures: 2, successes: 5, total: 7 },
  ],
}, { fmtDuration: (value) => String(value) });

const body = get("audit-summary-body").textContent;
if (!body.includes("38.9%")) {
  throw new Error(`aggregate tool call failure rate missing: ${body}`);
}
for (const tool of ["orbit.task.update", "orbit.search", "orbit.task.add", "orbit.task.show"]) {
  if (!body.includes(tool)) throw new Error(`per-tool list omitted ${tool}: ${body}`);
}
if (get("audit-summary-title").textContent !== "Audit Summary 24h") {
  throw new Error(`summary title was ${get("audit-summary-title").textContent}`);
}
"#,
    );
}

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

const { renderScoreboard } = await import("./js/scoreboard.js");

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
if (!table) throw new Error("expected the scoreboard matrix table to render");
const tbody = table.children[2];

const failureRow = tbody.children.find((tr) => tr.dataset.key === "scoreboard-Operations-failure_incidents");
if (!failureRow) throw new Error("the failure_incidents row must not be hidden by the activity filter when its source is unavailable");
const rowText = failureRow.textContent;
if (rowText.includes("0/0")) throw new Error(`must not render an unavailable source as a measured 0/0, got: ${rowText}`);


"#,
    );
}

#[test]
fn dashboard_aggregate_runs_keep_workspace_identity_filters_and_action_scope() {
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
const { initRuns, renderRuns, buildReplayRunButton } = await import("./js/runs.js");
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
if (!body.textContent.includes("Gone")) throw new Error("partial workspace failure was hidden");

let controls = body.children.find((node) => node.className.includes("runs-filter"));
controls.children.find((node) => node.textContent === "All").listeners.click();
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
controls.children.find((node) => node.textContent === "Failed").listeners.click();
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

const { initRuns, renderRuns } = await import("./js/runs.js");

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
loading = false;
lastRuns = [];
lastMeta = { state: "failed", total: 0, limit: 25, truncated: false };
renderRuns(lastRuns);
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

"#,
    );
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
const {renderAutomation} = await import('./js/automation.js');
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
  machine_name: "hm_local",
  session_explanation: "session access",
  capabilities: { routine_toggle: { authorized: true }, clock_service: { authorized: true }, clock_cadence: { authorized: true } },
  routines: [{
    name: "pilot", source: "one", target: "orbit.workflow.auto", enabled: true, effective: true,
    cron: "*/5 * * * *", description: "pilot routine",
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
};
globalThis.fetch = async (path) => {
  const url = String(path).split("?")[0];
  const payload = payloads[url] || {};
  return { ok: true, status: 200, json: async () => payload, text: async () => JSON.stringify(payload) };
};

const { setWorkspace } = await import("./js/common.js");
const { initOperations, fetchAndRenderOperations } = await import("./js/operations.js");
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
const {renderAutomation} = await import('./js/automation.js');
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

const { buildArtifacts } = await import("./js/tasks.js");

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
if (!preview.textContent)
  throw new Error(`a decode failure must be explained: ${preview.textContent}`);
const fallback = collectLinks(preview).filter((a) => a.download === "flow.png");
if (fallback.length !== 1)
  throw new Error(`a failed image must still offer its bytes exactly once, got ${fallback.length}`);

// --- Narrow viewport renders the same image element ------------------------
window.innerWidth = 420;
({ preview } = await renderPreview(png, respondWith("png", "image/png")));
const narrowImg = byTag(preview, "img");
if (!narrowImg || narrowImg.src !== "blob:png")
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

/// Task jumps must scope lookup and all resulting dashboard state to the task's
/// workspace. Cross-workspace jumps refresh through the normal Tasks path;
/// same-workspace jumps stay local, and superseded lookups cannot adopt or
/// render their workspace after a newer lookup wins.
#[test]
fn dashboard_global_task_jump_scopes_to_selected_workspace_and_distinguishes_errors() {
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
const wrap = get("task-search-wrap");
wrap.className = "task-search-wrap";
wrap.appendChild(get("task-search"));
wrap.appendChild(get("task-lookup-status"));
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
const location = new URL("http://dashboard.test/?workspace=ws_polaris&window=24h&run_state=failed");
location.hash = "#tasks?status=in-progress%2Creview%2Cblocked%2Cproposed%2Cbacklog";
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
location._hash = "#tasks?status=in-progress%2Creview%2Cblocked%2Cproposed%2Cbacklog";
globalThis.history = { replaceState: (_, __, url) => { const next = new URL(String(url), location.href); location.search = next.search; location.pathname = next.pathname; } };
Object.defineProperty(globalThis, "navigator", { value: { clipboard: { writeText: () => Promise.resolve() } }, configurable: true });
globalThis.requestAnimationFrame = (fn) => fn();
globalThis.setInterval = () => 0;
globalThis.EventSource = class { constructor() {} close() {} };

const polarisTask = { id: "POLA-00001", title: "Polaris cached task", status: "blocked", history: [], artifacts: [], comments: [] };
const existing = { id: "DANI-00012", title: "Orbit adopted task", status: "blocked", history: [], artifacts: [], comments: [] };
const nebulaTask = { id: "NEBU-00002", title: "Nebula adopted task", status: "blocked", history: [], artifacts: [], comments: [] };
let lookupMode = "existing";
let delayed = null;
let delayedOrbit = null;
let delayedNebula = null;
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
      { id: "ws_nebula", name: "nebula", status: "active", is_default: false },
    ]);
  }
  if (/^\/api\/tasks\/(?:ORB-|DANI-|POLA-|NEBU-)/.test(url.pathname)) {
    const id = decodeURIComponent(url.pathname.slice("/api/tasks/".length));
    const workspace = url.searchParams.get("workspace");
    if (lookupMode === "network" && id === "ORB-00001") throw new Error("offline");
    if (lookupMode === "denied" && id === "ORB-00002") return json({ error: "cross-origin requests not allowed" }, 403);
    if (lookupMode === "server" && id === "ORB-00003") return json({ error: "boom" }, 500);
    if (lookupMode === "stale" && id === "DANI-00012") {
      await new Promise((resolve) => { delayed = resolve; });
      return workspace === "ws_orbit" ? json(existing) : json({ error: `task not found: ${id}` }, 404);
    }
    if (lookupMode === "rapid" && id === "DANI-00012" && workspace === "ws_orbit") {
      await new Promise((resolve) => { delayedOrbit = resolve; });
      return json(existing);
    }
    if (lookupMode === "rapid" && id === "NEBU-00002" && workspace === "ws_nebula") {
      await new Promise((resolve) => { delayedNebula = resolve; });
      return json(nebulaTask);
    }
    if (id === "POLA-00001" && workspace === "ws_polaris") return json(polarisTask);
    if (id === "DANI-00012" && workspace === "ws_orbit") return json(existing);
    if (id === "NEBU-00002" && workspace === "ws_nebula") return json(nebulaTask);
    return json({ error: `task not found: ${id}` }, 404);
  }
  if (url.pathname === "/api/tasks" || url.pathname === "/api/tasks/all") {
    const workspace = url.searchParams.get("workspace");
    const item = workspace === "ws_polaris" ? polarisTask
      : workspace === "ws_orbit" ? existing
      : workspace === "ws_nebula" ? nebulaTask
      : null;
    return json({ items: item ? [item] : [], total: item ? 1 : 0, limit: 50, truncated: false });
  }
  if (url.pathname === "/api/audit/summary") {
    const workspace = url.searchParams.get("workspace");
    const value = workspace === "ws_polaris" ? 101 : workspace === "ws_orbit" ? 202 : 303;
    return json({ events: value, denials: 0, failed_runs: 1, active_long_runs: 0, sparkline: [] });
  }
  if (url.pathname === "/api/job-runs") {
    const workspace = url.searchParams.get("workspace");
    const runId = workspace === "ws_polaris" ? "jrun-polaris" : workspace === "ws_orbit" ? "jrun-orbit" : "jrun-nebula";
    return json({ items: [{ run_id: runId, job_id: `${workspace}-job`, state: "failed", created_at: "2026-09-12T10:00:00Z" }], total: 1, limit: 25, truncated: false, state: "failed" });
  }
  if (url.pathname === "/api/crews") return json({ crews: [] });
  if (url.pathname === "/api/tasks/locks") return json([]);
  if (url.pathname === "/api/diagnostics/friction") return json([]);
  return json([]);
};

const originalSetTimeout = setTimeout;
globalThis.setTimeout = (fn, ms, ...args) => originalSetTimeout(fn, ms === 250 ? 0 : ms, ...args);
const tick = () => new Promise((resolve) => originalSetTimeout(resolve, 0));

await import("./app.js");
await tick(); await tick(); await tick();

const { getWorkspace, setWorkspace } = await import("./js/common.js");
if (getWorkspace() !== "ws_polaris") throw new Error(`selected workspace should remain ws_polaris, got ${getWorkspace()}`);
if (!get("tasks-body").textContent.includes("Polaris cached task")) throw new Error("initial ws_polaris task cache did not render");
if (get("rail-count-audit").textContent !== "101") throw new Error(`initial rail count was not scoped to ws_polaris: ${get("rail-count-audit").textContent}`);

const input = get("task-search");
const err = get("task-lookup-status");
function jump(id) {
  input.value = id;
  if (input.listeners.keydown) input.listeners.keydown({ key: "Enter", preventDefault() {} });
  else input.listeners.input();
}

requests.length = 0;
jump("pola-00001");
await tick(); await tick(); await tick(); await tick(); await tick();
const taskGets = requests.filter((url) => url.startsWith("/api/tasks/POLA-00001"));
if (!taskGets[0] || !taskGets[0].includes("workspace=ws_polaris")) {
  throw new Error(`existing-task jump must query the selected workspace first; got ${JSON.stringify(taskGets)}`);
}
// ORB-12516: the pinned task's own detail reads its claim provenance, scoped to
// the same workspace. That is the jumped-to task's data, not a panel refresh.
const panelRefreshes = requests.filter(
  (url) => !url.startsWith("/api/tasks/POLA-00001") && !url.startsWith("/api/distributed/claims"),
);
if (panelRefreshes.length) {
  throw new Error(`same-workspace jump must not refresh dashboard panels: ${JSON.stringify(panelRefreshes)}`);
}
if (requests.some((url) => url.startsWith("/api/distributed/claims") && !url.includes("workspace=ws_polaris"))) {
  throw new Error(`the claim read must stay in the jumped-to task's workspace: ${JSON.stringify(requests)}`);
}
if (err.textContent.includes("not found")) throw new Error(`existing task reported missing: ${err.textContent}`);
if (input.value) throw new Error("successful jump should clear the input");
if (!String(location.hash).includes("tasks")) throw new Error(`successful jump should open Tasks, hash=${location.hash}`);
const opened = get("tasks-body").children.some((node) => String(node.textContent).includes("POLA-00001"));
if (!opened) throw new Error("existing blocked task must render after jump");

requests.length = 0;
jump("DANI-00012");
await tick(); await tick(); await tick(); await tick(); await tick();
if (getWorkspace() !== "ws_orbit") throw new Error(`cross-workspace lookup should adopt ws_orbit, got ${getWorkspace()}`);
const refreshed = requests.filter((url) => !url.startsWith("/api/tasks/DANI-00012"));
if (!refreshed.some((url) => url.startsWith("/api/tasks?")) || !refreshed.some((url) => url.startsWith("/api/audit/summary?"))) {
  throw new Error(`cross-workspace lookup did not use the normal Tasks refresh: ${JSON.stringify(requests)}`);
}
if (refreshed.some((url) => !url.includes("workspace=ws_orbit"))) {
  throw new Error(`cross-workspace refresh leaked a non-orbit request: ${JSON.stringify(refreshed)}`);
}
if (!get("tasks-body").textContent.includes("Orbit adopted task") || get("tasks-body").textContent.includes("Polaris cached task")) {
  throw new Error(`task rows were not replaced with ws_orbit state: ${get("tasks-body").textContent}`);
}
if (get("rail-count-audit").textContent !== "202") throw new Error(`rail count was not refreshed for ws_orbit: ${get("rail-count-audit").textContent}`);

requests.length = 0;
location.hash = "#diagnostics/runs?window=24h";
await tick(); await tick(); await tick(); await tick(); await tick();
const runRequests = requests.filter((url) => url.startsWith("/api/job-runs") || url.startsWith("/api/diagnostics/friction"));
if (runRequests.length !== 2 || runRequests.some((url) => !url.includes("workspace=ws_orbit"))) {
  throw new Error(`run refresh was not scoped to adopted ws_orbit: ${JSON.stringify(runRequests)}`);
}
if (!get("runs-body").textContent.includes("jrun-orbit") || get("runs-body").textContent.includes("jrun-polaris")) {
  throw new Error(`run rows were not replaced with ws_orbit state: ${get("runs-body").textContent}`);
}

setWorkspace("ws_polaris");
location.hash = "#tasks";
await tick(); await tick(); await tick(); await tick(); await tick();
lookupMode = "rapid";
jump("DANI-00012");
await tick(); await tick();
jump("NEBU-00002");
await tick(); await tick();
if (typeof delayedNebula !== "function" || typeof delayedOrbit !== "function") throw new Error("rapid lookup responses were not both pending");
delayedNebula();
await tick(); await tick(); await tick(); await tick(); await tick();
delayedOrbit();
await tick(); await tick(); await tick(); await tick(); await tick();
if (getWorkspace() !== "ws_nebula") throw new Error(`older lookup adopted ws_orbit after newer ws_nebula lookup: ${getWorkspace()}`);
if (!get("tasks-body").textContent.includes("Nebula adopted task") || get("tasks-body").textContent.includes("Orbit adopted task")) {
  throw new Error(`older lookup overwrote newer task state: ${get("tasks-body").textContent}`);
}
lookupMode = "existing";

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
if (!err.textContent.includes(input.value)) throw new Error(`lookup error must name the requested task: ${err.textContent}`);

lookupMode = "network";
jump("ORB-00001");
await tick(); await tick(); await tick();
if (!err.textContent.includes(input.value)) throw new Error(`lookup error must name the requested task: ${err.textContent}`);
if (err.textContent.includes("not found")) throw new Error("transport failure must not look like a miss");

lookupMode = "denied";
jump("ORB-00002");
await tick(); await tick(); await tick();
if (!err.textContent.includes(input.value)) throw new Error(`lookup error must name the requested task: ${err.textContent}`);

lookupMode = "server";
jump("ORB-00003");
await tick(); await tick(); await tick();
if (!err.textContent.includes(input.value)) throw new Error(`lookup error must name the requested task: ${err.textContent}`);

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

/// The Config tab's contract is what it paints from a layered payload and
/// what it writes when a row is edited; the scenario drives the shipped
/// module against a fetch stub rather than asserting its internals.
#[test]
fn dashboard_config_renders_provenance_and_writes_one_key_per_save() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_loading_dom.mjs"),
        include_str!("dashboard_config.mjs")
    ));
}

/// The Plugins tab's contract is the generic renderer: four render modes,
/// link tiles, and sanitised markdown, all from fixture data with no
/// plugin-specific script [ORB-12738].
#[test]
fn dashboard_plugins_tab_renders_every_panel_mode_and_sanitises_markdown() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_purify_dom.mjs"),
        include_str!("dashboard_plugins.mjs")
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
fn dashboard_rows_are_keyboard_operable_without_changing_click_behaviour() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_keyboard_dom.mjs"),
        include_str!("dashboard_keyboard.mjs")
    ));
}

// DANI-10391: list rows are summaries; the detail behind a row comes from
// `GET /api/tasks/:id` when it opens. The scenario drives the shipped module
// against a fetch stub and observes the reads it issues and what it paints.
#[test]
fn dashboard_summary_rows_expand_through_the_detail_endpoint() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_keyboard_dom.mjs"),
        include_str!("dashboard_task_detail.mjs")
    ));
}

// ORB-12516: claim provenance and the owner's handoff actions are the one
// dashboard surface where a wrong word is a wrong decision — an expired
// reservation that reads as a revocation, or a "review" that reads as a code
// review, would send an operator to fence a live attempt. The scenario drives
// the shipped module against a fetch stub and asserts what it paints and what
// it sends. The same file runs in a real Chromium via
// `dashboard_distributed_browser.mjs`.
#[test]
fn dashboard_renders_claim_provenance_and_sends_exact_owner_decisions() {
    run_dashboard_javascript_test(&format!(
        "{}\n{}",
        include_str!("dashboard_keyboard_dom.mjs"),
        include_str!("dashboard_distributed.mjs")
    ));
}

// The focus ring is the other half of keyboard operability: a row that can be
// focused but shows nothing is not usable.
#[test]
fn dashboard_persisted_dock_width_clamp_and_wrap_toggle_behavior() {
    let script = r#"
// Setup mock window & localStorage before importing shipped modules
const store = new Map();
globalThis.window = {
  location: { search: '' },
  innerWidth: 1200,
  localStorage: {
    getItem: (key) => store.get(key) ?? null,
    setItem: (key, val) => store.set(key, String(val)),
    removeItem: (key) => store.delete(key),
  },
  addEventListener: () => {},
  removeEventListener: () => {},
};

const {
  clampDockWidth,
  loadDockWidthPref,
  saveDockWidthPref,
  applyDockWidth,
  loadLogWrapPref,
  saveLogWrapPref,
  applyLogWrap,
  getDockMaxWidth,
} = await import('./js/log-tail.js');

// 1. Clamp logic: derived from Tasks grid available width
// Viewport 1200px: rail is 216px, padding (40px) + gap (20px) = 60px.
// Grid track space: min(1800, 1200 - 216) - 60 = 924px. Dock max: 60% = 554px.
const minW = 336;
const maxW1200 = Math.round(924 * 0.6); // 554
if (getDockMaxWidth() !== 554) throw new Error(`expected max 554 at 1200px, got ${getDockMaxWidth()}`);
if (clampDockWidth(200) !== minW) throw new Error(`expected clamp to ${minW}, got ${clampDockWidth(200)}`);
if (clampDockWidth(450) !== 450) throw new Error(`expected 450, got ${clampDockWidth(450)}`);
if (clampDockWidth(1000) !== maxW1200) throw new Error(`expected clamp to ${maxW1200}, got ${clampDockWidth(1000)}`);

// 2. Persisted dock width restore & clamp
saveDockWidthPref(250);
if (loadDockWidthPref() !== minW) throw new Error(`expected restore clamped to ${minW}, got ${loadDockWidthPref()}`);
saveDockWidthPref(450);
if (loadDockWidthPref() !== 450) throw new Error(`expected restore 450, got ${loadDockWidthPref()}`);
saveDockWidthPref(2000);
if (loadDockWidthPref() !== maxW1200) throw new Error(`expected restore clamped to ${maxW1200}, got ${loadDockWidthPref()}`);
saveDockWidthPref(null);
if (loadDockWidthPref() !== null) throw new Error(`expected null after removal, got ${loadDockWidthPref()}`);

// 3. Viewport wider than 1800px layout cap (e.g. 2900px, 3440px ultrawide)
// The 1800px cap and 60px padding/gap saturate track space at 1740px.
// Dock max width is capped at 60% of grid (1044px), leaving 40% (696px) for tasks.
window.innerWidth = 2900;
const maxWUltrawide = Math.round(1740 * 0.6); // 1044
if (getDockMaxWidth() !== 1044) throw new Error(`expected max 1044 at 2900px, got ${getDockMaxWidth()}`);
if (clampDockWidth(2000) !== maxWUltrawide) throw new Error(`expected clamp to ${maxWUltrawide} at 2900px, got ${clampDockWidth(2000)}`);
saveDockWidthPref(2000);
if (loadDockWidthPref() !== maxWUltrawide) throw new Error(`expected restore clamped to ${maxWUltrawide}, got ${loadDockWidthPref()}`);
const remainingTaskWidth = 1740 - getDockMaxWidth();
if (remainingTaskWidth < 336) throw new Error(`expected task column to retain usable width, got ${remainingTaskWidth}`);

// Dragging or pressing End at >= 2900px clamps to maxWUltrawide (1044px), leaves task list visible, no overflow
const endKeyWidth = clampDockWidth(getDockMaxWidth());
if (endKeyWidth !== 1044) throw new Error(`expected End key width 1044, got ${endKeyWidth}`);
const dragExcessWidth = clampDockWidth(3000);
if (dragExcessWidth !== 1044) throw new Error(`expected drag excess clamped to 1044, got ${dragExcessWidth}`);

// Ultrawide 3440px also saturates at the 1800px layout cap (1740px grid tracks)
window.innerWidth = 3440;
if (getDockMaxWidth() !== 1044) throw new Error(`expected max 1044 at 3440px, got ${getDockMaxWidth()}`);

// Viewport shrink from ultrawide to 1200px re-clamps restored dock width
window.innerWidth = 1200;
if (loadDockWidthPref() !== maxW1200) throw new Error(`expected re-clamp to ${maxW1200} after shrink, got ${loadDockWidthPref()}`);
saveDockWidthPref(null);

// 4. Mock DOM for applyDockWidth and splitter interaction
const styles = {};
const layout = {
  style: {
    setProperty: (k, v) => { styles[k] = v; },
    removeProperty: (k) => { delete styles[k]; },
  },
};
const attrs = {};
const splitter = {
  setAttribute: (k, v) => { attrs[k] = String(v); },
  getAttribute: (k) => attrs[k],
};
globalThis.document = {
  querySelector: (sel) => {
    if (sel === 'main.tasks-layout') return layout;
    if (sel === '.log-stream') return logStream;
    return null;
  },
  getElementById: (id) => {
    if (id === 'dock-splitter') return splitter;
    if (id === 'log-wrap-lines') return wrapBtn;
    if (id === 'side-dock') return { getBoundingClientRect: () => ({ width: 400 }) };
    return null;
  },
};

// When layout clientWidth is available from rendered DOM, derive from layout.clientWidth - 60
layout.clientWidth = 1800;
if (getDockMaxWidth() !== 1044) throw new Error(`expected max 1044 from layout clientWidth 1800, got ${getDockMaxWidth()}`);
delete layout.clientWidth;

// applyDockWidth
applyDockWidth(480);
if (styles['--dock-w'] !== '480px') throw new Error(`expected --dock-w 480px, got ${styles['--dock-w']}`);
if (attrs['aria-valuenow'] !== '480') throw new Error(`expected aria-valuenow 480, got ${attrs['aria-valuenow']}`);

// Reset to default (null) removes property
applyDockWidth(null);
if (styles['--dock-w'] !== undefined) throw new Error(`expected --dock-w removed, got ${styles['--dock-w']}`);
if (attrs['aria-valuenow'] !== '400') throw new Error(`expected default aria-valuenow 400, got ${attrs['aria-valuenow']}`);

// 5. Wrap toggle behavior and persistence
const streamClasses = new Set();
const logStream = {
  classList: {
    toggle: (c, val) => { if (val) streamClasses.add(c); else streamClasses.delete(c); },
    contains: (c) => streamClasses.has(c),
  },
};
const btnClasses = new Set();
const btnAttrs = {};
const wrapBtn = {
  classList: {
    toggle: (c, val) => { if (val) btnClasses.add(c); else btnClasses.delete(c); },
    contains: (c) => btnClasses.has(c),
  },
  setAttribute: (k, v) => { btnAttrs[k] = String(v); },
};

// Default off
if (loadLogWrapPref() !== false) throw new Error('expected default logWrap false');
applyLogWrap(false);
if (streamClasses.has('wrap')) throw new Error('expected no wrap class on stream');
if (btnAttrs['aria-pressed'] !== 'false') throw new Error('expected aria-pressed false');

// Toggle on
saveLogWrapPref(true);
if (loadLogWrapPref() !== true) throw new Error('expected logWrap true from localStorage');
applyLogWrap(true);
if (!streamClasses.has('wrap')) throw new Error('expected wrap class on stream');
if (!btnClasses.has('on')) throw new Error('expected on class on wrap button');
if (btnAttrs['aria-pressed'] !== 'true') throw new Error('expected aria-pressed true');
"#;
    run_dashboard_javascript_test(script);
}

/// ORB-12889: task-id links (taskLink) and all Operations links use the dashboard
/// accent link color and are legible on dark and light themes (no browser-default blue).
#[test]
fn dashboard_quick_action_refusals_show_full_message_and_link_runs() {
    run_dashboard_javascript_test(
        r##"
import assert from "node:assert/strict";

class Node {
  constructor() {
    this.children = [];
    this.dataset = {};
    this.style = {};
    this.listeners = {};
    this.className = "";
    this._text = "";
    this.parentNode = null;
    this.disabled = false;
  }
  appendChild(child) {
    this.children.push(child);
    child.parentNode = this;
    return child;
  }
  insertBefore(child, before) {
    const old = child.parentNode;
    if (old) old.children = old.children.filter((c) => c !== child);
    const index = this.children.indexOf(before);
    this.children.splice(index < 0 ? this.children.length : index, 0, child);
    child.parentNode = this;
    return child;
  }
  removeChild(child) {
    this.children = this.children.filter((c) => c !== child);
    child.parentNode = null;
    return child;
  }
  remove() {
    if (this.parentNode) {
      this.parentNode.removeChild(this);
    }
  }
  prepend(...newNodes) {
    for (let i = newNodes.length - 1; i >= 0; i--) {
      const child = newNodes[i];
      if (child.parentNode) child.parentNode.removeChild(child);
      this.children.unshift(child);
      child.parentNode = this;
    }
  }
  querySelector(selector) {
    const sel = selector.startsWith(".") ? selector.slice(1) : selector;
    return find(this, (n) => n !== this && n.className && n.className.split(" ").includes(sel));
  }
  querySelectorAll(selector) {
    const sel = selector.startsWith(".") ? selector.slice(1) : selector;
    const results = [];
    const collect = (n) => {
      for (const child of n.children || []) {
        if (child.className && child.className.split(" ").includes(sel)) {
          results.push(child);
        }
        collect(child);
      }
    };
    collect(this);
    return results;
  }
  replaceChildren(...newChildren) {
    this.children = [];
    for (const c of newChildren) {
      this.appendChild(c);
    }
  }
  addEventListener(name, callback) {
    this.listeners[name] = callback;
  }
  setAttribute(name, value) {
    this[name] = String(value);
  }
  getAttribute(name) {
    return this[name] || null;
  }
  get textContent() {
    return this._text + this.children.map((c) => c.textContent || "").join("");
  }
  set textContent(value) {
    this._text = String(value);
    this.children = [];
  }
  get lastElementChild() {
    return this.children[this.children.length - 1];
  }
  get classList() {
    return {
      add: (...names) => {
        const set = new Set(this.className.split(" ").filter(Boolean));
        for (const n of names) set.add(n);
        this.className = Array.from(set).join(" ");
      },
      remove: (...names) => {
        const set = new Set(this.className.split(" ").filter(Boolean));
        for (const n of names) set.delete(n);
        this.className = Array.from(set).join(" ");
      },
      contains: (name) => this.className.split(" ").includes(name),
    };
  }
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
Object.defineProperty(globalThis, "navigator", {
  value: { clipboard: { writeText: () => Promise.resolve() } },
  configurable: true,
});

let mockRejections = new Map();
globalThis.fetch = async (url) => {
  const path = String(url);
  for (const [prefix, refusal] of mockRejections.entries()) {
    if (path.includes(prefix)) {
      return {
        ok: false,
        status: refusal.status || 409,
        text: async () => JSON.stringify({ error: refusal.error }),
      };
    }
  }
  return { ok: true, status: 200, text: async () => "{}" };
};

const statuses = ["in-progress", "review", "blocked", "proposed", "backlog", "someday", "done", "rejected", "archived"];
const { renderTasks } = await import("./js/tasks.js");

function find(node, predicate) {
  if (!node) return null;
  if (predicate(node)) return node;
  for (const child of node.children || []) {
    const match = find(child, predicate);
    if (match) return match;
  }
  return null;
}

// 1. Row-level Ship refusal with long message and run ID
const shipTask = {
  id: "ORB-12971",
  title: "Ship task with in-flight run conflict",
  status: "backlog",
  history: [],
  artifacts: [],
  status_transitions: [],
};

let currentTasks = [shipTask];
const shipContext = {
  getTasks: () => currentTasks,
  getTasksMeta: () => null,
  getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["backlog"]),
  statusOrder: statuses,
  fmtAbsTime: (v) => v,
  refreshDashboard: () => Promise.resolve(),
};

const longShipError = "task ORB-12971 already has an in-flight run (jrun-20260925-0551-c2); wait for it to finish or cancel it";
mockRejections.set("/api/workflows/ship", { status: 409, error: longShipError });

renderTasks(currentTasks, shipContext);

const shipBtn = find(get("tasks-body"), (n) => n.className && n.className.includes("task-quick ship"));
assert.ok(shipBtn, "Ship quick action button must be rendered on backlog task");

shipBtn.listeners.click({ stopPropagation: () => {} });
await new Promise(setImmediate);

const shipErr = find(get("tasks-body"), (n) => n.className && n.className.includes("task-quick-error"));
assert.ok(shipErr, "task-quick-error must be rendered in task list after ship refusal");

const expectedShipText = `ship failed: ${longShipError}`;
assert.equal(
  shipErr.textContent,
  expectedShipText,
  "Full error message must be rendered in DOM without truncation",
);

// Assert the message is not clamped to an ellipsized line
assert.notEqual(shipErr.getAttribute("title"), longShipError, "Error must not be hidden only in a hover title attribute");

const rowWithShipErr = find(get("tasks-body"), (n) => n.dataset && n.dataset.key === "task-ORB-12971");
assert.ok(rowWithShipErr, "task row node must exist");
// Assert run id link is rendered
const runLink = find(shipErr, (n) => n.className && n.className.includes("task-quick-error-link"));
assert.ok(runLink, "Run ID link must be rendered inside quick error");
assert.equal(runLink.textContent, "jrun-20260925-0551-c2");
assert.equal(runLink.href, "#runs?run_id=jrun-20260925-0551-c2");

// Assert clicking quick error stops propagation to allow text selection without toggling row
let propagationStopped = false;
shipErr.listeners.click({ stopPropagation: () => { propagationStopped = true; } });
assert.ok(propagationStopped, "Quick error element must stop event propagation for text selection");

// 2. Refused Approve quick action shows its full message the same way
const approveTask = {
  id: "ORB-12972",
  title: "Proposed task refused approval",
  status: "proposed",
  history: [],
  artifacts: [],
  status_transitions: [],
};

currentTasks = [approveTask];
const approveContext = {
  getTasks: () => currentTasks,
  getTasksMeta: () => null,
  getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["proposed"]),
  statusOrder: statuses,
  fmtAbsTime: (v) => v,
  refreshDashboard: () => Promise.resolve(),
};

const longApproveError = "status transition refused: governance policy requires an explicit approval note for critical tasks";
mockRejections.set("/approve", { status: 403, error: longApproveError });

renderTasks(currentTasks, approveContext);

const approveBtn = find(get("tasks-body"), (n) => n.className && n.className.includes("task-quick approve"));
assert.ok(approveBtn, "Approve quick action button must be rendered on proposed task");

approveBtn.listeners.click({ stopPropagation: () => {} });
await new Promise(setImmediate);

const approveErr = find(get("tasks-body"), (n) => n.className && n.className.includes("task-quick-error"));
assert.ok(approveErr, "task-quick-error must be rendered in task list after approve refusal");

const expectedApproveText = `approve failed: ${longApproveError}`;
assert.equal(
  approveErr.textContent,
  expectedApproveText,
  "Full approve error message must be rendered without truncation",
);

const rowWithApproveErr = find(get("tasks-body"), (n) => n.dataset && n.dataset.key === "task-ORB-12972");
// 3. Detail panel's Ship error renders above detail columns without displacing them
const detailTask = {
  id: "ORB-12973",
  title: "Detail task to test panel ship error",
  status: "backlog",
  history: [],
  artifacts: [],
  status_transitions: [],
};

currentTasks = [detailTask];
const detailContext = {
  getTasks: () => currentTasks,
  getTasksMeta: () => null,
  getSearchQuery: () => "",
  getActiveStatuses: () => new Set(["backlog"]),
  statusOrder: statuses,
  fmtAbsTime: (v) => v,
  refreshDashboard: () => Promise.resolve(),
};

renderTasks(currentTasks, detailContext);

// Expand the row to mount detail panel
const rowToExpand = find(get("tasks-body"), (n) => n.dataset && n.dataset.key === "task-ORB-12973");
rowToExpand.listeners.click();
renderTasks(currentTasks, detailContext);

const detailPanel = find(get("tasks-body"), (n) => n.className && n.className.includes("row-detail split-layout"));
assert.ok(detailPanel, "Detail panel must be rendered when row is expanded");

const detailShipBtn = find(detailPanel, (n) => n.className && n.className.includes("action ship"));
assert.ok(detailShipBtn, "Detail panel must contain Ship button");

const detailShipError = "task ORB-12973 already in flight";
mockRejections.set("/api/workflows/ship", { status: 409, error: detailShipError });

detailShipBtn.listeners.click({ stopPropagation: () => {} });
await new Promise(setImmediate);

const detailActionError = find(detailPanel, (n) => n.className && n.className.includes("action-error"));
assert.ok(detailActionError, "Detail panel must prepend action-error when ship fails");
assert.ok(detailActionError.textContent.includes(detailShipError));

"##,
    );
}
