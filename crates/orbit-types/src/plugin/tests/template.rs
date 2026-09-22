use super::super::template::{PluginTemplateVars, render_template, validate_template};

#[test]
fn an_unterminated_reference_is_refused_rather_than_rendered_literally() {
    let error = validate_template("{{workspace", "spec.permissions.fs.read[0]")
        .expect_err("an unclosed '{{' must be refused");
    assert!(error.message.contains("unterminated"), "{}", error.message);

    let vars = PluginTemplateVars {
        workspace: Some("/work".to_string()),
        ..PluginTemplateVars::default()
    };
    render_template("{{workspace", &vars, "spec.permissions.fs.read[0]")
        .expect_err("render_template must not splice the unclosed tail into its output");
}

#[test]
fn a_terminated_reference_still_validates_and_renders() {
    validate_template("{{workspace}}/data", "spec.permissions.fs.read[0]")
        .expect("a closed reference is valid");
    let vars = PluginTemplateVars {
        workspace: Some("/work".to_string()),
        ..PluginTemplateVars::default()
    };
    assert_eq!(
        render_template("{{workspace}}/data", &vars, "spec.permissions.fs.read[0]")
            .expect("renders"),
        "/work/data"
    );
}
