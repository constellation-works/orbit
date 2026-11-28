//! Plugin pages served by the dashboard (docs/design/plugin-pages/2_design.md
//! §2–§3): one document under the frame policy, and the declared read-only
//! tool calls a page makes, behind the unchanged origin guard.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use orbit_core::adapter::command::{PluginAddOptions, PluginEnableOptions};
use orbit_core::app::plugin;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::support::{Fixture, Server, error_code, isolated, json_ok};

const WS: &str = "workspace=ws_http_fixture";

/// Distinctive bytes per file, with characters an escaper would rewrite.
const MARKUP: &str = "<main id=\"page\">Tom &amp; \"Jerry\" <b>bold</b> &lt;raw&gt;</main>";

const APP_CSS: &str = "main > b { content: \"&<>\"; color: red; }";

const APP_JS: &str = "window.appRan = \"a & b < c > d\";";

const MOD_MJS: &str = "export const moduleRan = 'm & n';";

const PLAIN_JS: &str = "window.plainRan = 1;";

const KIT_STYLE_MARK: &str = "Orbit plugin-page kit stylesheet";

const KIT_SCRIPT_MARK: &str = "Orbit plugin-page kit, bridge version 1";

const PAGES: &str = concat!(
    "  web:\n",
    "    pages:\n",
    "      - id: overview\n",
    "        title: Overview\n",
    "        root: web/overview\n",
    "        scripts: [app.js, mod.mjs]\n",
    "        styles: [app.css]\n",
    "        kit: 1\n",
    "        tools: [status]\n",
    "      - id: plain\n",
    "        root: web/plain\n",
    "        scripts: [plain.js]\n",
    "        tools: [status]\n",
);

fn write_executable(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A backend that records each run in `<control>/calls`, blocks on the
/// `<control>/hold` FIFO while it exists until the test releases it, and
/// echoes its request; with `<control>/big` it returns more than a panel may.
fn backend(control: &Path) -> String {
    let control = control.display();
    format!(
        "#!/bin/sh\nrequest=$(cat)\necho run >> '{control}/calls'\nif [ -p '{control}/hold' ]; \
         then read _ < '{control}/hold'; fi\nif [ -f \
         '{control}/big' ]; then\n  blob=$(head -c 300000 /dev/zero | tr '\\0' a)\n  printf \
         '{{\"ok\":true,\"output\":{{\"blob\":\"%s\"}}}}\\n' \"$blob\"\nelse\n  printf \
         '{{\"ok\":true,\"output\":{{\"request\":%s}}}}\\n' \"$request\"\nfi\n"
    )
}

fn install(fixture: &Fixture, namespace: &str, spec: &str) -> PathBuf {
    let control = fixture.path(&format!("control-{namespace}"));
    fs::create_dir_all(&control).unwrap();
    let source = fixture.path(&format!("source-{namespace}/.orbit-plugin"));
    write_executable(&source.join("bin/backend.sh"), &backend(&control));
    for (dir, file, body) in [
        ("web/overview", "index.html", MARKUP),
        ("web/overview", "app.css", APP_CSS),
        ("web/overview", "app.js", APP_JS),
        ("web/overview", "mod.mjs", MOD_MJS),
        ("web/overview", "unlisted.js", "window.unlisted = 1;"),
        ("web/plain", "index.html", MARKUP),
        ("web/plain", "plain.js", PLAIN_JS),
    ] {
        fs::create_dir_all(source.join(dir)).unwrap();
        fs::write(source.join(dir).join(file), body).unwrap();
    }
    fs::write(
        source.join("plugin.yaml"),
        format!(
            "schemaVersion: 2\nkind: Plugin\nmetadata:\n  name: {namespace}\n  version: 0.1.0\n  \
             description: Page fixture.\nspec:\n  backend:\n    type: exec\n    command: \
             bin/backend.sh\n    sandbox: none\n  tools:\n    - name: status\n      description: \
             Read status.\n      execution_kind: read_only\n      mcp_scope: workspace\n      \
             input_schema:\n        type: object\n        properties:\n          limit:\n            \
             type: integer\n        additionalProperties: false\n    - name: other-read\n      \
             description: Read something else.\n      execution_kind: read_only\n      \
             mcp_scope: workspace\n    - name: mutate\n      description: Change something.\n      \
             execution_kind: mutating\n      mcp_scope: workspace\n{spec}"
        ),
    )
    .unwrap();
    plugin::add_plugin(
        &fixture.runtime,
        source.to_str().unwrap(),
        &PluginAddOptions::default(),
    )
    .unwrap();
    plugin::enable_plugin(
        &fixture.runtime,
        namespace,
        &PluginEnableOptions {
            grants: vec!["unsandboxed".into()],
            force: false,
        },
    )
    .unwrap();
    control
}

fn page_path(namespace: &str, page: &str) -> String {
    format!("/api/plugins/{namespace}/pages/{page}/?{WS}")
}

fn call_path(namespace: &str, page: &str) -> String {
    format!("/api/plugins/{namespace}/pages/{page}/call?{WS}")
}

fn position(body: &str, needle: &str) -> usize {
    body.find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is not in the document:\n{body}"))
}

/// Every `script` element's content, in document order.
fn script_contents(body: &str) -> Vec<&str> {
    let mut contents = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<script") {
        let open_end = start + rest[start..].find('>').unwrap() + 1;
        let close = open_end + rest[open_end..].find("</script>").unwrap();
        contents.push(&rest[open_end..close]);
        rest = &rest[close + "</script>".len()..];
    }
    contents
}

fn expected_policy(scripts: &[&str]) -> String {
    let hashes: Vec<String> = scripts
        .iter()
        .map(|script| {
            format!(
                "'sha256-{}'",
                base64::engine::general_purpose::STANDARD.encode(Sha256::digest(script))
            )
        })
        .collect();
    format!(
        "sandbox allow-scripts; default-src 'none'; script-src {}; style-src 'unsafe-inline'; \
         img-src data: blob:; font-src data:; connect-src 'none'; form-action 'none'; \
         base-uri 'none'; frame-ancestors 'self'",
        hashes.join(" ")
    )
}

fn header<'a>(response: &'a reqwest::blocking::Response, name: &str) -> &'a str {
    response
        .headers()
        .get(name)
        .unwrap_or_else(|| panic!("no {name} header"))
        .to_str()
        .unwrap()
}

fn not_found_naming(server: &Server, path: &str, reason: &str) {
    let response = server.get(path);
    let status = response.status().as_u16();
    let body = response.text().unwrap();
    assert_eq!(status, 404, "{path}: {body}");
    assert!(body.contains(reason), "{path} names `{reason}`: {body}");
}

#[test]
fn a_page_is_one_document_under_its_frame_policy_and_only_while_active() {
    isolated(
        "plugin_page::a_page_is_one_document_under_its_frame_policy_and_only_while_active",
        || {
            let fixture = Fixture::new();
            install(&fixture, "pages", PAGES);
            let server = fixture.server(false);

            let response = server.get(&page_path("pages", "overview"));
            assert_eq!(response.status().as_u16(), 200);
            assert_eq!(
                header(&response, "content-type"),
                "text/html; charset=utf-8"
            );
            assert_eq!(header(&response, "x-content-type-options"), "nosniff");
            let policy = header(&response, "content-security-policy").to_string();
            let body = response.text().unwrap();

            // Kit stylesheet, listed style, markup, kit script, listed
            // scripts: each file's bytes unchanged and in that order.
            let order = [
                position(&body, KIT_STYLE_MARK),
                position(&body, &format!("<style>{APP_CSS}</style>")),
                position(&body, MARKUP),
                position(&body, KIT_SCRIPT_MARK),
                position(&body, &format!("<script>{APP_JS}</script>")),
                position(
                    &body,
                    &format!("<script type=\"module\">{MOD_MJS}</script>"),
                ),
            ];
            assert!(
                order.windows(2).all(|pair| pair[0] < pair[1]),
                "{order:?}\n{body}"
            );
            assert!(
                !body.contains("window.unlisted"),
                "an unlisted file is never written"
            );
            let scripts = script_contents(&body);
            assert_eq!(scripts.len(), 3, "kit, app.js and mod.mjs: {body}");
            assert!(scripts[0].contains(KIT_SCRIPT_MARK));
            assert_eq!(&scripts[1..], &[APP_JS, MOD_MJS]);
            // The kit passes the refusals a page script is held to.
            for refused in ["</script", "<!--"] {
                assert!(!scripts[0].to_ascii_lowercase().contains(refused));
            }
            assert_eq!(policy, expected_policy(&scripts));

            // A page without `kit` gets no kit and no hash for it.
            let response = server.get(&page_path("pages", "plain"));
            assert_eq!(response.status().as_u16(), 200);
            let policy = header(&response, "content-security-policy").to_string();
            let body = response.text().unwrap();
            assert!(!body.contains(KIT_STYLE_MARK) && !body.contains(KIT_SCRIPT_MARK));
            assert!(!body.contains("<style>"), "{body}");
            let scripts = script_contents(&body);
            assert_eq!(scripts, [PLAIN_JS]);
            assert_eq!(policy, expected_policy(&scripts));

            not_found_naming(
                &server,
                &page_path("pages", "missing"),
                "declares no page 'missing'",
            );
            // No route serves a page file or the kit.
            for path in [
                format!("/api/plugins/pages/pages/overview/app.js?{WS}"),
                format!("/api/plugins/pages/pages/overview/index.html?{WS}"),
                format!("/api/plugins/pages/pages/overview/assets/app.js?{WS}"),
                "/static/plugin-kit/v1/kit.js".to_string(),
                "/static/plugin-kit/v1/kit.css".to_string(),
            ] {
                let response = server.get(&path);
                let status = response.status().as_u16();
                assert_eq!(status, 404, "{path}: {}", response.text().unwrap());
            }

            // The origin guard is unchanged: cross-site and opaque-origin
            // requests are refused, the dashboard's own navigation admitted.
            let path = page_path("pages", "overview");
            for (name, value) in [("sec-fetch-site", "cross-site"), ("origin", "null")] {
                let response = server
                    .request("GET", &path)
                    .header(name, value)
                    .send()
                    .unwrap();
                assert_eq!(response.status().as_u16(), 403, "{name}: {value}");
            }
            let response = server
                .request("GET", &path)
                .header("sec-fetch-site", "same-origin")
                .header("sec-fetch-dest", "iframe")
                .send()
                .unwrap();
            assert_eq!(response.status().as_u16(), 200);
            drop(server);

            // A page file edited after install fails its digest check.
            let installed = plugin::show_plugin(&fixture.runtime, "pages").unwrap();
            fs::write(
                Path::new(&installed.install_path).join("web/overview/app.js"),
                "window.tampered = 1;",
            )
            .unwrap();
            let server = fixture.server(false);
            not_found_naming(&server, &page_path("pages", "overview"), "recorded");
            assert_eq!(
                server.get(&page_path("pages", "plain")).status().as_u16(),
                200
            );
            drop(server);

            // A plugin disabled for the workspace serves no page.
            plugin::disable_plugin_in_workspace(&fixture.runtime, "pages").unwrap();
            let server = fixture.server(false);
            not_found_naming(
                &server,
                &page_path("pages", "plain"),
                "disabled in this workspace",
            );
        },
    );
}

fn call(server: &Server, page: &str, body: Value) -> reqwest::blocking::Response {
    server.send("POST", &call_path("pages", page), body)
}

fn run_count(control: &Path) -> usize {
    fs::read_to_string(control.join("calls"))
        .map(|text| text.lines().count())
        .unwrap_or(0)
}

/// Make every backend that runs from now on wait for [`release`].
fn hold(control: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(control.join("hold"))
        .status()
        .unwrap();
    assert!(status.success(), "mkfifo: {status}");
}

/// Let every held backend finish: each waiting reader meets end of file
/// when the only writer closes, and a backend that starts later finds no
/// FIFO to wait on.
fn release(control: &Path) {
    let fifo = control.join("hold");
    let writer = fs::OpenOptions::new().write(true).open(&fifo).unwrap();
    fs::remove_file(&fifo).unwrap();
    drop(writer);
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How many page calls the server shared with one already in flight.
fn shared_calls(trace: &Path) -> usize {
    fs::read_to_string(trace)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["span"]["name"] == "page_call_shared")
        .count()
}

#[test]
fn a_page_runs_only_its_declared_read_only_tools_within_its_bounds() {
    isolated(
        "plugin_page::a_page_runs_only_its_declared_read_only_tools_within_its_bounds",
        || {
            let fixture = Fixture::new();
            let control = install(&fixture, "pages", PAGES);
            install(&fixture, "other", "");
            let trace = fixture.path("page-call-trace.jsonl");
            let server = fixture
                .server_with_envs(false, &[("ORBIT_HTTP_PAGE_CALL_TRACE", trace.as_os_str())]);

            // A declared read-only tool runs with its input, through the
            // audited dispatch.
            let output = json_ok(call(
                &server,
                "overview",
                json!({"tool": "status", "input": {"limit": 5}}),
            ));
            assert!(
                output["output"]["request"]
                    .to_string()
                    .contains("\"limit\":5"),
                "{output}"
            );
            assert_eq!(run_count(&control), 1);
            let events = json_ok(server.get(&format!("/api/audit?{WS}&since=24h&limit=500")));
            assert!(
                events
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|event| event["tool_name"] == "pages.status"
                        && event["status"] == "success"),
                "the call is audited as a call to pages.status: {events:#}"
            );

            // Each refusal runs nothing.
            for (body, status, code) in [
                (
                    json!({"tool": "other-read", "input": {}}),
                    403,
                    "tool_refused",
                ),
                (
                    json!({"tool": "other.status", "input": {}}),
                    403,
                    "tool_refused",
                ),
                (json!({"tool": "mutate", "input": {}}), 403, "tool_refused"),
                (
                    json!({"tool": "status", "input": {"limit": "five"}}),
                    400,
                    "input_invalid",
                ),
                (
                    json!({"tool": "status", "input": {"extra": 1}}),
                    400,
                    "input_invalid",
                ),
            ] {
                error_code(call(&server, "overview", body.clone()), status, code);
                assert_eq!(run_count(&control), 1, "{body} ran nothing");
            }
            assert_eq!(run_count(&fixture.path("control-other")), 0);
            // The call route sits behind the same guard as the document.
            for (name, value) in [("sec-fetch-site", "cross-site"), ("origin", "null")] {
                let response = server
                    .request("POST", &call_path("pages", "overview"))
                    .header("origin", &server.origin)
                    .header(name, value)
                    .json(&json!({"tool": "status", "input": {}}))
                    .send()
                    .unwrap();
                assert_eq!(response.status().as_u16(), 403, "{name}: {value}");
            }
            assert_eq!(run_count(&control), 1);

            // Two identical calls in flight share one execution.
            hold(&control);
            std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    json_ok(call(
                        &server,
                        "overview",
                        json!({"tool": "status", "input": {"limit": 9}}),
                    ))
                });
                wait_until("the first call to run", || run_count(&control) == 2);
                let second = scope.spawn(|| {
                    json_ok(call(
                        &server,
                        "overview",
                        json!({"tool": "status", "input": {"limit": 9}}),
                    ))
                });
                wait_until("the second call to share the first", || {
                    shared_calls(&trace) == 1
                });
                release(&control);
                assert_eq!(first.join().unwrap(), second.join().unwrap());
            });
            assert_eq!(run_count(&control), 2, "the shared call ran once");

            // A page has at most four executions at once.
            hold(&control);
            std::thread::scope(|scope| {
                let running: Vec<_> = (1..=4)
                    .map(|limit| {
                        let server = &server;
                        scope.spawn(move || {
                            json_ok(call(
                                server,
                                "overview",
                                json!({"tool": "status", "input": {"limit": limit}}),
                            ))
                        })
                    })
                    .collect();
                wait_until("four executions", || run_count(&control) == 6);
                let busy = error_code(
                    call(
                        &server,
                        "overview",
                        json!({"tool": "status", "input": {"limit": 5}}),
                    ),
                    429,
                    "busy",
                );
                assert!(
                    busy["error"].as_str().unwrap().contains("4 calls"),
                    "{busy}"
                );
                release(&control);
                for handle in running {
                    handle.join().unwrap();
                }
            });
            assert_eq!(run_count(&control), 6, "the refused call ran nothing");

            // Output over the panel cap is truncated as a panel's is.
            fs::write(control.join("big"), "").unwrap();
            let output = json_ok(call(&server, "overview", json!({"tool": "status"})));
            assert_eq!(output["truncated"], true, "{}", output["diagnostic"]);
            assert!(output["output"].as_str().unwrap().len() <= 128 * 1024);
        },
    );
}
