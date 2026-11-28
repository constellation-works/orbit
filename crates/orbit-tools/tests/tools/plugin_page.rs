//! `spec.web.pages[]` at load: what `orbit plugin validate` and install
//! refuse by page, what the digest covers, and how the serving loader keeps
//! a failed page on the page (docs/design/plugin-pages/2_design.md §1).
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use orbit_tools::plugin::{
    MAX_PLUGIN_PAGE_BYTES, MAX_PLUGIN_PAGE_FILES, MAX_PLUGIN_PAGE_NAME_BYTES,
    PluginValidationPolicy, load_plugin_dir, load_plugin_dir_without_tests, page_digest_diagnostic,
    plugin_pages_unchanged, validate_loaded_plugin,
};

const PAGE_ROOT: &str = "web/overview";

const VALID_PAGE: &str = concat!(
    "    pages:\n",
    "      - id: overview\n",
    "        title: Graph\n",
    "        root: web/overview\n",
    "        scripts: [app.js]\n",
    "        styles: [app.css]\n",
    "        kit: 1\n",
    "        tools: [status]\n",
);

fn graph_example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/graph-example/.orbit-plugin")
}

fn copy_tree(source: &Path, target: &Path) {
    for entry in std::fs::read_dir(source).expect("read fixture dir") {
        let entry = entry.expect("entry");
        let destination = target.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            std::fs::create_dir_all(&destination).expect("mkdir");
            copy_tree(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), &destination).expect("copy");
        }
    }
}

/// The design example with `pages` placed under `spec.web` and one valid
/// page's files written.
fn example_with_pages(pages: &str) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    copy_tree(&graph_example(), temp.path());
    let manifest = temp.path().join("plugin.yaml");
    let raw = std::fs::read_to_string(&manifest).expect("read manifest");
    let anchor = "    links:\n";
    assert!(
        raw.contains(anchor),
        "fixture no longer has a links section"
    );
    std::fs::write(
        &manifest,
        raw.replacen(anchor, &format!("{pages}{anchor}"), 1),
    )
    .expect("write manifest");
    let page = temp.path().join(PAGE_ROOT);
    std::fs::create_dir_all(&page).expect("page dir");
    std::fs::write(page.join("index.html"), "<main id=\"app\"></main>\n").expect("index");
    std::fs::write(page.join("app.js"), "orbit.call('status', {});\n").expect("script");
    std::fs::write(page.join("app.css"), "main { margin: 0; }\n").expect("style");
    temp
}

fn page_file(temp: &tempfile::TempDir, name: &str) -> PathBuf {
    temp.path().join(PAGE_ROOT).join(name)
}

/// The refusal `orbit plugin validate` reports, which must name the page.
fn refusal(temp: &tempfile::TempDir) -> String {
    let error = load_plugin_dir(temp.path())
        .and_then(|plugin| {
            validate_loaded_plugin(&plugin, &PluginValidationPolicy::host_default())?;
            Ok(plugin)
        })
        .expect_err("the page is refused");
    error.to_string()
}

fn assert_refused(temp: &tempfile::TempDir, page: &str, expect: &str, case: &str) {
    let message = refusal(temp);
    assert!(
        message.contains(&format!("page '{page}'")),
        "{case}: the refusal names the page: {message}"
    );
    assert!(
        message
            .to_ascii_lowercase()
            .contains(&expect.to_ascii_lowercase()),
        "{case}: expected {expect:?} in: {message}"
    );
}

#[test]
fn a_valid_page_loads_with_its_files_and_digest() {
    let temp = example_with_pages(VALID_PAGE);
    let plugin = load_plugin_dir(temp.path()).expect("loads");
    validate_loaded_plugin(&plugin, &PluginValidationPolicy::host_default()).expect("valid");

    assert_eq!(plugin.pages.len(), 1);
    let page = &plugin.pages[0];
    assert_eq!(page.id, "overview");
    assert_eq!(page.title, "Graph");
    assert_eq!(page.tools, ["status"]);
    assert_eq!(page.kit, Some(1));
    let bundle = page.bundle.as_ref().expect("bundle read");
    assert_eq!(bundle.markup, b"<main id=\"app\"></main>\n");
    assert_eq!(bundle.scripts[0].name, "app.js");
    assert_eq!(bundle.styles[0].name, "app.css");
    assert_eq!(bundle.digest.len(), 64);
    assert_eq!(
        page_digest_diagnostic("graph", Some(&bundle.digest), page),
        None,
        "a page whose files hash to the recorded digest is served"
    );
}

#[test]
fn a_page_names_mutating_tools_only_under_actions() {
    let temp = example_with_pages(&VALID_PAGE.replace(
        "tools: [status]",
        "tools: [status]\n        actions: [maintain]",
    ));
    let plugin = load_plugin_dir(temp.path()).expect("loads");
    validate_loaded_plugin(&plugin, &PluginValidationPolicy::host_default()).expect("valid");
    assert_eq!(plugin.pages[0].tools, ["status"]);
    assert_eq!(plugin.pages[0].actions, ["maintain"]);
}

#[test]
fn validate_refuses_each_unsafe_page_by_name() {
    let mutate = |pages: &str, edit: &dyn Fn(&tempfile::TempDir)| {
        let temp = example_with_pages(pages);
        edit(&temp);
        temp
    };
    let none = |_: &tempfile::TempDir| {};
    let with_tools = |tools: &str| VALID_PAGE.replace("tools: [status]", tools);

    let size_bound = format!("at most {MAX_PLUGIN_PAGE_BYTES} bytes (2 MiB)");
    let file_count_bound = format!("at most {MAX_PLUGIN_PAGE_FILES} files");
    let name_bytes_bound = format!("at most {MAX_PLUGIN_PAGE_NAME_BYTES} bytes");
    let cases: Vec<(&str, tempfile::TempDir, &str)> = vec![
        (
            "a tool of another plugin",
            mutate(&with_tools("tools: [other.status]"), &none),
            "does not declare",
        ),
        (
            "a mutating tool",
            mutate(&with_tools("tools: [maintain]"), &none),
            "read_only",
        ),
        (
            "an action of another plugin",
            mutate(
                &with_tools("tools: [status]\n        actions: [other.maintain]"),
                &none,
            ),
            "names action 'other.maintain', which this plugin does not declare",
        ),
        (
            "a read-only tool listed as an action",
            mutate(
                &with_tools("tools: [status]\n        actions: [status]"),
                &none,
            ),
            "list it under the page's `tools`",
        ),
        (
            "an action listed twice",
            mutate(
                &with_tools("tools: [status]\n        actions: [maintain, maintain]"),
                &none,
            ),
            "lists action 'maintain' more than once",
        ),
        (
            "a missing index.html",
            mutate(VALID_PAGE, &|temp| {
                std::fs::remove_file(page_file(temp, "index.html")).unwrap()
            }),
            "'index.html' is missing",
        ),
        (
            "a missing listed script",
            mutate(VALID_PAGE, &|temp| {
                std::fs::remove_file(page_file(temp, "app.js")).unwrap()
            }),
            "'app.js' is missing",
        ),
        (
            "a missing listed style",
            mutate(VALID_PAGE, &|temp| {
                std::fs::remove_file(page_file(temp, "app.css")).unwrap()
            }),
            "'app.css' is missing",
        ),
        (
            "a script not ending in .js or .mjs",
            mutate(&VALID_PAGE.replace("[app.js]", "[app.ts]"), &none),
            "must end in `.js` or `.mjs`",
        ),
        (
            "a style not ending in .css",
            mutate(&VALID_PAGE.replace("[app.css]", "[app.scss]"), &none),
            "must end in `.css`",
        ),
        (
            "index.html holding a script element",
            mutate(VALID_PAGE, &|temp| {
                std::fs::write(page_file(temp, "index.html"), "<main></main><SCRIPT src=x>")
                    .unwrap()
            }),
            "`<script` element",
        ),
        (
            "index.html holding a link element",
            mutate(VALID_PAGE, &|temp| {
                std::fs::write(page_file(temp, "index.html"), "<Link rel=stylesheet>").unwrap()
            }),
            "`<link` element",
        ),
        (
            "a script containing </script",
            mutate(VALID_PAGE, &|temp| {
                std::fs::write(page_file(temp, "app.js"), "const s = '</Script>';").unwrap()
            }),
            "contains `</script`",
        ),
        (
            "a script containing <!--",
            mutate(VALID_PAGE, &|temp| {
                std::fs::write(page_file(temp, "app.js"), "const s = '<!--';").unwrap()
            }),
            "contains `<!--`",
        ),
        (
            "a style containing </style",
            mutate(VALID_PAGE, &|temp| {
                std::fs::write(page_file(temp, "app.css"), "a{} </STYLE>").unwrap()
            }),
            "contains `</style`",
        ),
        (
            "a file listed twice",
            mutate(&VALID_PAGE.replace("[app.js]", "[app.js, ./app.js]"), &none),
            "more than once",
        ),
        (
            "a root outside the plugin root",
            mutate(
                &VALID_PAGE.replace("root: web/overview", "root: ../outside"),
                &none,
            ),
            "'..'",
        ),
        (
            "an absolute root",
            mutate(
                &VALID_PAGE.replace("root: web/overview", "root: /etc"),
                &none,
            ),
            "relative path inside the plugin root",
        ),
        (
            "a listed file outside the plugin root",
            mutate(&VALID_PAGE.replace("[app.js]", "[../../plugin.js]"), &none),
            "'..'",
        ),
        (
            "an unknown kit version",
            mutate(&VALID_PAGE.replace("kit: 1", "kit: 2"), &none),
            "kit version 2",
        ),
        (
            "a duplicate page id",
            mutate(
                &format!("{VALID_PAGE}{}", VALID_PAGE.replacen("    pages:\n", "", 1)),
                &none,
            ),
            "declared more than once",
        ),
        (
            "a page over the size bound",
            mutate(VALID_PAGE, &|temp| {
                let big = vec![b'a'; usize::try_from(MAX_PLUGIN_PAGE_BYTES).unwrap()];
                std::fs::write(page_file(temp, "app.js"), big).unwrap()
            }),
            &size_bound,
        ),
        (
            "a page listing too many files",
            mutate(
                &VALID_PAGE.replace(
                    "styles: [app.css]",
                    &format!(
                        "styles: [{}]",
                        (0..=MAX_PLUGIN_PAGE_FILES)
                            .map(|index| format!("s{index}.css"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                &none,
            ),
            &file_count_bound,
        ),
        (
            "a page whose listed file names exceed the name-bytes bound",
            mutate(
                &VALID_PAGE.replace(
                    "styles: [app.css]",
                    &format!(
                        "styles: [{}]",
                        (0..(MAX_PLUGIN_PAGE_NAME_BYTES / 2_625 + 1))
                            .map(|index| format!("s{index:02620}.css"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ),
                &none,
            ),
            &name_bytes_bound,
        ),
    ];
    for (case, temp, expect) in &cases {
        assert_refused(temp, "overview", expect, case);
    }
}

#[cfg(unix)]
#[test]
fn a_symlinked_page_file_is_refused_naming_the_page() {
    let temp = example_with_pages(VALID_PAGE);
    let script = page_file(&temp, "app.js");
    std::fs::remove_file(&script).unwrap();
    std::os::unix::fs::symlink(temp.path().join("plugin.yaml"), &script).unwrap();
    assert_refused(&temp, "overview", "symbolic link", "a symlinked script");
}

#[test]
fn files_the_manifest_does_not_list_change_neither_digest_nor_state() {
    let temp = example_with_pages(VALID_PAGE);
    let before = load_plugin_dir(temp.path()).expect("loads");
    let recorded = before.pages[0].digest().expect("digest").to_string();

    // Unlisted files, including ones that would be refused if listed.
    std::fs::write(page_file(&temp, "notes.txt"), "draft").unwrap();
    std::fs::write(page_file(&temp, "unused.js"), "'</script>'").unwrap();

    let after = load_plugin_dir(temp.path()).expect("unlisted files are ignored");
    assert_eq!(after.pages[0].digest(), Some(recorded.as_str()));
    assert!(plugin_pages_unchanged(&before));
    assert_eq!(
        page_digest_diagnostic("graph", Some(&recorded), &after.pages[0]),
        None
    );
}

#[test]
fn a_changed_listed_file_names_both_digests_and_leaves_the_plugin_loading() {
    let temp = example_with_pages(VALID_PAGE);
    let installed = load_plugin_dir_without_tests(temp.path()).expect("loads");
    let recorded = installed.pages[0].digest().expect("digest").to_string();

    std::fs::write(page_file(&temp, "app.css"), "main { margin: 1px; }\n").unwrap();
    assert!(
        !plugin_pages_unchanged(&installed),
        "a cached load notices the edited page file"
    );
    let reloaded = load_plugin_dir_without_tests(temp.path()).expect("the plugin still loads");
    assert_eq!(reloaded.tools.len(), installed.tools.len());
    let found = reloaded.pages[0].digest().expect("digest").to_string();
    assert_ne!(found, recorded);
    let diagnostic =
        page_digest_diagnostic("graph", Some(&recorded), &reloaded.pages[0]).expect("inactive");
    assert!(
        diagnostic.contains(&recorded) && diagnostic.contains(&found),
        "names both digests: {diagnostic}"
    );
    assert!(diagnostic.contains("page 'overview'"), "{diagnostic}");
}

#[test]
fn the_serving_loader_keeps_a_failed_page_on_the_page() {
    let temp = example_with_pages(VALID_PAGE);
    std::fs::write(page_file(&temp, "app.js"), "'</script>'").unwrap();

    load_plugin_dir(temp.path()).expect_err("validate and install refuse it");
    let served = load_plugin_dir_without_tests(temp.path()).expect("serving keeps the plugin");
    let error = served.pages[0]
        .bundle
        .as_ref()
        .expect_err("the page failed")
        .to_string();
    assert!(error.contains("page 'overview'"), "{error}");
    let diagnostic = page_digest_diagnostic("graph", Some("recorded"), &served.pages[0])
        .expect("the page is inactive");
    assert!(diagnostic.contains("</script"), "{diagnostic}");
}
