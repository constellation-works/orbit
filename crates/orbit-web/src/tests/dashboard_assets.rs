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
