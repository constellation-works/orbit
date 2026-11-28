//! Plugin pages end to end (docs/design/plugin-pages/2_design.md §1, §4):
//! a page validates, installs, is listed with its state, and a page file
//! edited after install turns that page inactive while the plugin's tools
//! and panels keep serving.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{Value, json};

use super::{CheckedPluginOutput, Fixture, write_status_plugin};
use crate::bound_server::{Announcement, BoundServer};

const PAGES_SPEC: &str = concat!(
    "  web:\n",
    "    panels:\n",
    "      - id: status\n",
    "        source: tool:status\n",
    "    pages:\n",
    "      - id: overview\n",
    "        title: Overview\n",
    "        root: web/overview\n",
    "        scripts: [app.js]\n",
    "        styles: [app.css]\n",
    "        kit: 1\n",
    "        tools: [status]\n",
);

fn write_page(root: &Path) {
    let page = root.join("web/overview");
    std::fs::create_dir_all(&page).expect("page dir");
    std::fs::write(page.join("index.html"), "<main></main>\n").expect("index");
    std::fs::write(page.join("app.js"), "orbit.call('status', {});\n").expect("script");
    std::fs::write(page.join("app.css"), "main { margin: 0; }\n").expect("style");
}

fn only_page(shown: &Value) -> &Value {
    let pages = shown["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 1, "{shown}");
    &pages[0]
}

#[test]
fn a_page_installs_lists_and_goes_inactive_alone_when_its_files_change() {
    if !orbit_exec::macos_sandbox_test_guard(
        &orbit_common::Env::capture(),
        "a_page_installs_lists_and_goes_inactive_alone_when_its_files_change",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let source = fixture.source("pages");
    write_status_plugin(&source, "pages", PAGES_SPEC);
    write_page(&source.join(".orbit-plugin"));

    let validated = fixture.json(["plugin", "validate", source.to_str().expect("utf8")]);
    assert_eq!(validated["pages"][0]["id"], "overview", "{validated}");
    let recorded = validated["pages"][0]["digest"]
        .as_str()
        .expect("digest")
        .to_string();

    let added = fixture
        .command(["plugin", "add"])
        .arg(&source)
        .arg("--enable")
        .success_output();
    let added = String::from_utf8_lossy(&added.stdout);
    assert!(
        added.contains("Pages") && added.contains("overview (Overview): calls pages.status"),
        "the consent output lists the page: {added}"
    );

    let shown = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(shown["status"], "active", "{shown}");
    let page = only_page(&shown);
    assert_eq!(page["id"], "overview");
    assert_eq!(page["title"], "Overview");
    assert_eq!(page["tools"], serde_json::json!(["pages.status"]));
    assert_eq!(page["kit"], 1);
    assert_eq!(page["state"], "active", "{shown}");
    assert!(page["diagnostic"].is_null());

    // Edit a listed file in the installed tree.
    let install = Path::new(shown["install_path"].as_str().expect("install path"));
    std::fs::write(
        install.join("web/overview/app.css"),
        "main { margin: 1px; }\n",
    )
    .expect("edit the installed style");

    let shown = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(shown["status"], "active", "the plugin still loads: {shown}");
    let page = only_page(&shown);
    assert_eq!(page["state"], "inactive", "{shown}");
    let diagnostic = page["diagnostic"].as_str().expect("diagnostic");
    assert!(
        diagnostic.contains(&recorded),
        "names the recorded digest: {diagnostic}"
    );
    let found = diagnostic
        .split("hash to ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .expect("names the found digest");
    assert_eq!(found.len(), 64, "{diagnostic}");
    assert_ne!(found, recorded);
    assert_eq!(shown["tools"][0]["active"], true, "{shown}");
    assert_eq!(shown["panels"][0]["id"], "status", "{shown}");

    // The plugin's tool still runs.
    fixture
        .command(["tool", "run", "pages.status", "--input", "{}"])
        .assert()
        .success();

    // The same forced reinstall a changed manifest needs re-records it.
    fixture
        .command(["plugin", "add"])
        .arg(&source)
        .arg("--force")
        .assert()
        .success();
    let shown = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(only_page(&shown)["state"], "active", "{shown}");
}

#[test]
fn a_replaced_page_action_needs_fresh_cli_consent() {
    let fixture = Fixture::new();
    let source = fixture.source("pages");
    write_status_plugin(
        &source,
        "pages",
        &PAGES_SPEC.replace(
            "        tools: [status]\n",
            "        tools: [status]\n        actions: [settle]\n",
        ),
    );
    // `tools` is the manifest's last key, so a tool appended is in it.
    let manifest = source.join(".orbit-plugin/plugin.yaml");
    let mut raw = std::fs::read_to_string(&manifest)
        .expect("manifest")
        .replace("    type: exec\n", "    type: exec\n    sandbox: none\n");
    raw.push_str(
        "    - name: settle\n      description: Settle a request.\n      execution_kind: \
         mutating\n      mcp_scope: workspace\n",
    );
    std::fs::write(&manifest, &raw).expect("write manifest");
    write_page(&source.join(".orbit-plugin"));

    // The documented preview: a plain `add` lists the actions and `show`
    // reads them, with no consent recorded yet.
    let previewed = fixture
        .command(["plugin", "add"])
        .arg(&source)
        .success_output();
    let previewed = String::from_utf8_lossy(&previewed.stdout);
    assert!(
        previewed.contains("actions: pages.settle"),
        "the preview lists the page's actions: {previewed}"
    );
    let shown = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(
        only_page(&shown)["actions"],
        json!([{"tool": "pages.settle", "consented": false, "active": false}]),
        "the preview leaves the action witness unchanged: {shown}"
    );

    // `enable` reports the consent it has just recorded.
    let added = fixture
        .command(["plugin", "enable", "pages", "--grant", "unsandboxed"])
        .success_output();
    let added = String::from_utf8_lossy(&added.stdout);
    assert!(
        added.contains("actions: pages.settle"),
        "the consent output lists the page's actions: {added}"
    );
    let shown = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(
        only_page(&shown)["actions"],
        json!([{"tool": "pages.settle", "consented": true, "active": true}]),
        "{shown}"
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit"));
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let mut server = BoundServer::spawn(
        command
            .current_dir(&fixture.work)
            .env("HOME", &fixture.home)
            .env("USERPROFILE", &fixture.home)
            .args(["web", "serve", "--port", "0", "--no-open", "--operator"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
        Announcement::Dashboard,
    )
    .expect("spawn dashboard");
    let origin = format!(
        "http://{}",
        server
            .wait_for_address(Duration::from_secs(30))
            .expect("dashboard bound")
    );
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("HTTP client");
    let post = |path: &str, body: Value| {
        let response = client
            .post(format!("{origin}/api/plugins/pages/{path}"))
            .header("origin", &origin)
            .json(&body)
            .send()
            .expect("dashboard request");
        let status = response.status().as_u16();
        let body = response.json::<Value>().expect("dashboard JSON");
        (status, body)
    };
    let confirm = || {
        post(
            "pages/overview/actions/confirm",
            json!({"tool": "settle", "input": {}}),
        )
    };
    let (status, action) = confirm();
    assert_eq!(status, 200, "{action}");
    assert_eq!(action["description"], "Settle a request.");

    // The page ID, action verb, page bundle and ordinary grant set are unchanged.
    std::fs::write(
        &manifest,
        raw.replace("Settle a request.", "Settle a reviewed request."),
    )
    .expect("replace the action definition");
    fixture.add(&source, &["--force"]);
    let replaced = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(replaced["granted"], shown["granted"]);
    assert_eq!(replaced["status"], "active", "{replaced}");
    assert_eq!(only_page(&replaced)["state"], "active", "{replaced}");
    assert_eq!(
        only_page(&replaced)["actions"],
        json!([{"tool": "pages.settle", "consented": false, "active": false}]),
        "{replaced}"
    );
    let (status, read) = post(
        "pages/overview/call",
        json!({"tool": "status", "input": {}}),
    );
    assert_eq!(status, 200, "{read}");
    let (status, refusal) = confirm();
    assert_eq!(status, 403, "{refusal}");
    assert_eq!(refusal["code"], "tool_refused");

    let (status, disabled) = post("disable", json!({"scope": "host"}));
    assert_eq!(status, 200, "{disabled}");
    let (status, enabled) = post("enable", json!({"scope": "host"}));
    assert_eq!(status, 200, "{enabled}");
    let (status, refusal) = confirm();
    assert_eq!(status, 403, "{refusal}");
    assert_eq!(refusal["code"], "tool_refused");

    fixture.success(["plugin", "enable", "pages", "--grant", "unsandboxed"]);
    let (status, action) = confirm();
    assert_eq!(status, 200, "{action}");
    assert_eq!(action["description"], "Settle a reviewed request.");
}
