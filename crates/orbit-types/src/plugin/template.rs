//! The template variables a manifest may use in a path: `{{workspace}}`,
//! `{{plugin_root}}`, `{{plugin_state}}` and `{{config.<key>}}`, and nothing
//! else (design §2).

use std::collections::BTreeMap;

use super::manifest::PluginManifestError;

/// Names a manifest path may reference. `config` is keyed by `<key>` from
/// `{{config.<key>}}`; a key that is absent is a rendering error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginTemplateVars {
    pub workspace: Option<String>,
    pub plugin_root: String,
    pub plugin_state: String,
    pub config: BTreeMap<String, String>,
}

/// Every `{{…}}` reference in `text`, in order.
pub fn template_references(text: &str) -> Vec<String> {
    let mut references = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        references.push(after[..end].trim().to_string());
        rest = &after[end + 2..];
    }
    references
}

/// Whether `reference` is one of the four allowed forms.
pub fn is_allowed_template_reference(reference: &str) -> bool {
    matches!(reference, "workspace" | "plugin_root" | "plugin_state")
        || reference
            .strip_prefix("config.")
            .is_some_and(|key| !key.is_empty() && !key.contains('.'))
}

/// Reject any reference outside the allowed set; `field` names the manifest
/// key for the diagnostic.
pub fn validate_template(text: &str, field: &str) -> Result<(), PluginManifestError> {
    for reference in template_references(text) {
        if !is_allowed_template_reference(&reference) {
            return Err(PluginManifestError::new(
                field,
                format!(
                    "'{text}' uses the template variable '{{{{{reference}}}}}'; only \
                     {{{{workspace}}}}, {{{{plugin_root}}}}, {{{{plugin_state}}}} and \
                     {{{{config.<key>}}}} are allowed"
                ),
            ));
        }
    }
    Ok(())
}

/// Substitute every reference. A reference the caller cannot resolve — no
/// workspace in this context, or a config key the plugin never declared —
/// is an error naming it, never an empty string spliced into a path.
pub fn render_template(
    text: &str,
    vars: &PluginTemplateVars,
    field: &str,
) -> Result<String, PluginManifestError> {
    validate_template(text, field)?;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let reference = after[..end].trim();
        let value = match reference {
            "workspace" => vars.workspace.clone().ok_or_else(|| {
                PluginManifestError::new(
                    field,
                    format!("'{text}' needs {{{{workspace}}}}, but this call has no workspace"),
                )
            })?,
            "plugin_root" => vars.plugin_root.clone(),
            "plugin_state" => vars.plugin_state.clone(),
            other => {
                let key = other.strip_prefix("config.").unwrap_or(other);
                vars.config.get(key).cloned().ok_or_else(|| {
                    PluginManifestError::new(
                        field,
                        format!(
                            "'{text}' references {{{{config.{key}}}}}, but the plugin declares \
                             no default for config key '{key}'"
                        ),
                    )
                })?
            }
        };
        out.push_str(&value);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}
