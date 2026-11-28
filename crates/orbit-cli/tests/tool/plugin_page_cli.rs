//! Plugin pages end to end (docs/design/plugin-pages/2_design.md §1, §4):
//! a page validates, installs, is listed with its state, and a page file
//! edited after install turns that page inactive while the plugin's tools
//! and panels keep serving.

use std::path::Path;

use serde_json::Value;

use super::{CheckedPluginOutput, Fixture, write_status_plugin};

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
fn enabling_a_plugin_whose_page_names_actions_lists_them_and_records_consent() {
    if !orbit_exec::macos_sandbox_test_guard(
        &orbit_common::Env::capture(),
        "enabling_a_plugin_whose_page_names_actions_lists_them_and_records_consent",
    ) {
        return;
    }
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
    let mut raw = std::fs::read_to_string(&manifest).expect("manifest");
    raw.push_str(
        "    - name: settle\n      description: Settle a request.\n      execution_kind: \
         mutating\n      mcp_scope: workspace\n",
    );
    std::fs::write(&manifest, raw).expect("write manifest");
    write_page(&source.join(".orbit-plugin"));

    let added = fixture
        .command(["plugin", "add"])
        .arg(&source)
        .arg("--enable")
        .success_output();
    let added = String::from_utf8_lossy(&added.stdout);
    assert!(
        added.contains("actions: pages.settle"),
        "the consent output lists the page's actions: {added}"
    );
    let shown = fixture.json(["plugin", "show", "pages"]);
    assert_eq!(
        only_page(&shown)["actions"],
        serde_json::json!([{"tool": "pages.settle", "consented": true, "active": true}]),
        "{shown}"
    );
}
