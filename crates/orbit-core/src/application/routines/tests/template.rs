//! Shipped-template shape classification.

use super::super::seed::{
    DEFAULT_ROUTINE_FILES, RETIRED_ROUTINE_FILES, SUPERSEDED_ROUTINE_TEMPLATES,
};
use super::super::template::{ShippedShape, shipped_shape_of};
use super::materialize::{current_template, render};

/// Shape classification itself: every shipped, superseded, and retired
/// template is recognised, and an operator's own routine is not.
#[test]
fn shipped_shapes_are_classified_by_template_owned_fields() {
    for (stem, template) in DEFAULT_ROUTINE_FILES {
        let body = render(template, stem, "workspace");
        assert_eq!(
            shipped_shape_of(stem, &body),
            Some(ShippedShape::Current),
            "{stem} must be recognised as its current shipped shape"
        );
        assert_eq!(
            shipped_shape_of(stem, &body.replace("enabled: false", "enabled: true")),
            Some(ShippedShape::Current),
            "{stem} opted in is still the current shape"
        );
    }
    for (stem, template) in SUPERSEDED_ROUTINE_TEMPLATES {
        assert_eq!(
            shipped_shape_of(stem, &render(template, stem, "workspace")),
            Some(ShippedShape::Superseded)
        );
    }
    for (stem, template) in RETIRED_ROUTINE_FILES {
        assert_eq!(
            shipped_shape_of(stem, &render(template, stem, "workspace")),
            Some(ShippedShape::Retired)
        );
    }

    // A template's own fields changed: not a shipped shape.
    let edited = render(
        current_template("dependabot_alert_sweep"),
        "dependabot_alert_sweep",
        "workspace",
    )
    .replace(r#"cron: "25 3 * * *""#, r#"cron: "*/5 * * * *""#);
    assert_eq!(shipped_shape_of("dependabot_alert_sweep", &edited), None);
    // Nor is a file that does not parse as a routine.
    assert_eq!(
        shipped_shape_of("dependabot_alert_sweep", "not: a routine\n"),
        None
    );
}
