// Delivery convention templates are refused when the configuration loads.

/// An unknown placeholder fails the load naming the key and the placeholder,
/// and a template missing what Orbit reads back from history is refused.
#[test]
fn delivery_templates_refuse_unknown_placeholders_and_missing_markers_at_load() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let load_workspace = |body: &str| {
        fs::write(workspace.path().join("config.toml"), body).expect("write workspace config");
        ResolvedConfig::load(&roots).map_err(|error| error.to_string())
    };

    let refusals = [
        (
            "[pr]\ntitle_template = \"{{ticket}}: {{title}}\"\n",
            ["pr.title_template", "{{ticket}}"],
        ),
        (
            "[commit]\nbody_template = \"{{ext.}}\"\n",
            ["commit.body_template", "{{ext.}}"],
        ),
        (
            "[commit]\nsubject_template = \"{{type}}: {{title}} {{task_id}}\"\n",
            ["commit.subject_template", "[{{task_id}}]"],
        ),
        (
            "[branch]\nname_template = \"{{prefix}}/{{task_id}}\"\n",
            ["branch.name_template", "{{hash}}"],
        ),
        (
            "[branch]\nname_template = \"{{prefix}}/{{title}}-{{task_id}}-{{hash}}\"\n",
            ["branch.name_template", "{{title}}"],
        ),
        (
            "[branch]\nname_template = \"..{{task_id}}-{{hash}}\"\n",
            ["branch.name_template", "'..'"],
        ),
        (
            "[pr]\nreviewers = [\"octo\", \"--admin\"]\n",
            ["pr.reviewers", "'--admin'"],
        ),
        (
            "[pr]\nbody_template = \"../outside.md\"\n",
            ["pr.body_template", ".."],
        ),
        (
            "[pr]\nscreenshot_section = \"sometimes\"\n",
            ["pr.screenshot_section", "'sometimes'"],
        ),
    ];
    for (body, needles) in refusals {
        let error = load_workspace(body).expect_err(body);
        assert!(
            needles.iter().all(|needle| error.contains(needle)),
            "{body} -> {error}"
        );
    }

    let config = load_workspace(
        "[pr]\ntitle_template = \"{{ type }}: {{title}} ({{ext.jira}})\"\n\
         screenshot_section = \"always\"\n\
         [commit]\nsubject_template = \"{{type}}({{ext.jira}}): {{title}} [{{task_id}}]\"\n\
         [branch]\nname_template = \"{{prefix}}/{{ext.jira}}-{{task_id}}-{{hash}}\"\n",
    )
    .expect("known placeholders load");
    let conventions = &config.pr.conventions;
    assert!(conventions.pr_title.is_some() && conventions.commit_subject.is_some());
    assert_eq!(conventions.screenshot_section.as_str(), "always");
    assert_eq!(
        conventions.branch_name.source(),
        "{{prefix}}/{{ext.jira}}-{{task_id}}-{{hash}}"
    );
}

/// The reviewable-lines exclusion is empty unless a workspace sets it, and
/// it is not the non-code class: an invalid glob is refused by name.
#[test]
fn non_reviewable_paths_default_empty_and_refuse_an_invalid_glob() {
    let global = tempfile::tempdir().expect("global tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let load_workspace = |body: &str| {
        fs::write(workspace.path().join("config.toml"), body).expect("write workspace config");
        ResolvedConfig::load(&roots).map_err(|error| error.to_string())
    };

    let defaults = load_workspace("").expect("defaults load");
    assert!(
        defaults.snapshot.workflow_non_reviewable_paths.is_empty(),
        "an unset exclusion counts every non-rename line"
    );
    assert!(
        !defaults.snapshot.workflow_non_code_paths.is_empty(),
        "docs and config stay in the non-code class, not the exclusion"
    );

    let configured =
        load_workspace("[workflow]\nnon_reviewable_paths = [\"**/*.golden\", \"**/*.lock\"]\n")
            .expect("globs load");
    assert_eq!(
        configured.snapshot.workflow_non_reviewable_paths,
        vec!["**/*.golden".to_string(), "**/*.lock".to_string()]
    );
    assert!(
        !configured.snapshot.workflow_non_code_paths.is_empty(),
        "setting the exclusion does not clear the non-code class"
    );

    let refused = load_workspace("[workflow]\nnon_reviewable_paths = [\"../generated\"]\n")
        .expect_err("a parent glob is refused");
    assert!(
        refused.contains("workflow.non_reviewable_paths"),
        "{refused}"
    );
}
