//! Pure manifest validation.

use super::super::namespace::{is_valid_namespace, is_valid_verb};
use super::super::template::validate_template;
use super::super::version::{SemverRange, Version};
use super::{
    LINK_URL_SCHEMES, MANIFEST_KIND, MANIFEST_SCHEMA_VERSION, MAX_PANEL_REFRESH_MS,
    MAX_SECRET_NAME_LEN, MIN_PANEL_REFRESH_MS, PluginExecutionKind, PluginManifest,
    PluginManifestError, PluginOrigin, is_valid_secret_name, validate_plugin_cli_flags,
    validate_plugin_cli_positionals,
};

impl PluginManifest {
    /// Structural validation that needs no filesystem: versions, kinds,
    /// namespace and verb spelling, duplicate tools, backend type, and the
    /// `cli` overrides each tool derives its `orbit <ns> <verb>` shape from.
    pub fn validate_structure(&self) -> Result<(), PluginManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(PluginManifestError::new(
                "schemaVersion",
                format!(
                    "unsupported plugin manifest schemaVersion {}; expected {MANIFEST_SCHEMA_VERSION}",
                    self.schema_version
                ),
            ));
        }
        if self.kind != MANIFEST_KIND {
            return Err(PluginManifestError::new(
                "kind",
                format!("expected '{MANIFEST_KIND}', found '{}'", self.kind),
            ));
        }
        if !is_valid_namespace(&self.metadata.name) {
            return Err(PluginManifestError::new(
                "metadata.name",
                format!(
                    "'{}' is not a valid namespace: use lowercase letters, digits, '_' or '-', \
                     starting with a letter, and not the reserved 'orbit'",
                    self.metadata.name
                ),
            ));
        }
        self.metadata
            .version
            .parse::<Version>()
            .map_err(|error| PluginManifestError::new("metadata.version", error.to_string()))?;
        if let Some(range) = &self.spec.requires.orbit {
            SemverRange::parse(range).map_err(|error| {
                PluginManifestError::new("spec.requires.orbit", error.to_string())
            })?;
        }
        if self.spec.backend.command.trim().is_empty() {
            return Err(PluginManifestError::new(
                "spec.backend.command",
                "must not be empty",
            ));
        }
        if self.spec.backend.timeout_ms == Some(0) {
            return Err(PluginManifestError::new(
                "spec.backend.timeout_ms",
                "must be greater than zero",
            ));
        }
        for (index, path) in self.spec.permissions.fs.read.iter().enumerate() {
            validate_template(path, &format!("spec.permissions.fs.read[{index}]"))?;
        }
        for (index, path) in self.spec.permissions.fs.write.iter().enumerate() {
            validate_template(path, &format!("spec.permissions.fs.write[{index}]"))?;
        }
        for (index, name) in self.spec.permissions.env_pass.iter().enumerate() {
            let field = format!("spec.permissions.env_pass[{index}]");
            if name.trim().is_empty() || name.contains('=') {
                return Err(PluginManifestError::new(
                    field,
                    format!("'{name}' is not an environment variable name"),
                ));
            }
            // `ORBIT_*` is Orbit's own execution envelope, not something a
            // manifest can request more of: the host stamps the plugin's
            // envelope unconditionally, and privilege-bearing names in this
            // namespace (`ORBIT_OPERATOR`, `ORBIT_WORKSPACE_CLAIM_TOKEN`) must
            // never reach a plugin child even by explicit request.
            if name.starts_with("ORBIT_") {
                return Err(PluginManifestError::new(
                    field,
                    format!(
                        "'{name}' is an Orbit-reserved variable and cannot be requested via \
                         env_pass; the plugin envelope provides Orbit context automatically"
                    ),
                ));
            }
        }
        if self.spec.tools.is_empty() {
            return Err(PluginManifestError::new(
                "spec.tools",
                "a plugin must declare at least one tool",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        // The subcommand each tool claims under `orbit <ns>`, which is its
        // `cli.verb` override when it declares one. Two tools claiming one
        // subcommand would register the same `orbit <ns> <verb>` twice, which
        // breaks every `orbit` invocation on the host, not just this plugin.
        let mut cli_verbs: std::collections::BTreeMap<&str, &str> =
            std::collections::BTreeMap::new();
        for (index, tool) in self.spec.tools.iter().enumerate() {
            let field = format!("spec.tools[{index}].name");
            if !is_valid_verb(&tool.name) {
                return Err(PluginManifestError::new(
                    field,
                    format!(
                        "'{}' is not a valid tool verb: use lowercase letters, digits, '_' or '-'",
                        tool.name
                    ),
                ));
            }
            if !seen.insert(tool.name.as_str()) {
                return Err(PluginManifestError::new(
                    field,
                    format!("tool '{}' is declared more than once", tool.name),
                ));
            }
            for (schema, key) in [
                (tool.input_schema.as_ref(), "input_schema"),
                (tool.output_schema.as_ref(), "output_schema"),
            ] {
                if let Some(schema) = schema
                    && !schema.is_object()
                {
                    return Err(PluginManifestError::new(
                        format!("spec.tools[{index}].{key}"),
                        "must be a JSON Schema object or `{ $ref: <path> }`",
                    ));
                }
            }
            if let Some(input_schema) = &tool.input_schema {
                validate_plugin_cli_flags(
                    input_schema,
                    &format!("spec.tools[{index}].input_schema"),
                )?;
            }
            let (cli_verb, verb_field) = match tool.cli.as_ref().and_then(|cli| cli.verb.as_deref())
            {
                Some(verb) => {
                    if !is_valid_verb(verb) {
                        return Err(PluginManifestError::new(
                            format!("spec.tools[{index}].cli.verb"),
                            format!(
                                "tool '{}' overrides its CLI verb to '{verb}', which is not a \
                                 valid subcommand: use lowercase letters, digits, '_' or '-'",
                                tool.name
                            ),
                        ));
                    }
                    (verb, format!("spec.tools[{index}].cli.verb"))
                }
                None => (tool.name.as_str(), format!("spec.tools[{index}].name")),
            };
            if let Some(previous) = cli_verbs.insert(cli_verb, tool.name.as_str()) {
                return Err(PluginManifestError::new(
                    verb_field,
                    format!(
                        "tools '{previous}' and '{}' both claim the CLI subcommand \
                         '{cli_verb}'; one `orbit <ns> {cli_verb}` cannot dispatch to two tools",
                        tool.name
                    ),
                ));
            }
            if let Some(cli) = &tool.cli {
                // A `{ $ref }` schema is a path this crate cannot read; the
                // loader repeats this check against the resolved document.
                let resolved_here = tool
                    .input_schema
                    .as_ref()
                    .is_none_or(|schema| schema.get("$ref").is_none());
                if resolved_here {
                    validate_plugin_cli_positionals(
                        &tool.name,
                        tool.input_schema.as_ref(),
                        &cli.positional,
                        &format!("spec.tools[{index}].cli.positional"),
                    )?;
                }
            }
        }
        self.validate_definition_paths()?;
        self.validate_web()?;
        self.validate_secrets()?;
        if let Some(build) = &self.spec.build {
            build.validate()?;
        }
        Ok(())
    }

    /// `spec.secrets`: each name well formed and declared once.
    fn validate_secrets(&self) -> Result<(), PluginManifestError> {
        let mut seen = std::collections::BTreeSet::new();
        for (index, secret) in self.spec.secrets.iter().enumerate() {
            let field = format!("spec.secrets[{index}].name");
            if !is_valid_secret_name(&secret.name) {
                return Err(PluginManifestError::new(
                    field,
                    format!(
                        "'{}' is not a valid secret name: start with a lowercase letter, then use \
                         lowercase letters, digits, '_' or '-', at most {MAX_SECRET_NAME_LEN} \
                         characters",
                        secret.name
                    ),
                ));
            }
            if !seen.insert(secret.name.as_str()) {
                return Err(PluginManifestError::new(
                    field,
                    format!("secret '{}' is declared more than once", secret.name),
                ));
            }
        }
        Ok(())
    }

    /// Whether `name` is one of this plugin's `spec.secrets`.
    pub fn declares_secret(&self, name: &str) -> bool {
        self.spec.secrets.iter().any(|secret| secret.name == name)
    }

    /// `spec.web` (§4.7): a panel reads exactly one declared `read_only`
    /// tool; a link is a title and a template-valid URL.
    ///
    /// A panel over a mutating tool is refused here, at the manifest, so the
    /// dashboard never has to decide at request time whether a source may
    /// be served to an unauthenticated session.
    fn validate_web(&self) -> Result<(), PluginManifestError> {
        let Some(web) = &self.spec.web else {
            return Ok(());
        };
        let mut seen = std::collections::BTreeSet::new();
        for (index, panel) in web.panels.iter().enumerate() {
            let field = format!("spec.web.panels[{index}]");
            if !is_valid_verb(&panel.id) {
                return Err(PluginManifestError::new(
                    format!("{field}.id"),
                    format!(
                        "'{}' is not a valid panel id: use lowercase letters, digits, '_' or '-'",
                        panel.id
                    ),
                ));
            }
            if !seen.insert(panel.id.as_str()) {
                return Err(PluginManifestError::new(
                    format!("{field}.id"),
                    format!("panel '{}' is declared more than once", panel.id),
                ));
            }
            let Some(verb) = panel.source_verb() else {
                return Err(PluginManifestError::new(
                    format!("{field}.source"),
                    format!(
                        "panel '{}' has source '{}'; a panel source is `tool:<verb>` naming one \
                         of this plugin's tools",
                        panel.id, panel.source
                    ),
                ));
            };
            let Some(tool) = self.spec.tools.iter().find(|tool| tool.name == verb) else {
                return Err(PluginManifestError::new(
                    format!("{field}.source"),
                    format!(
                        "panel '{}' sources tool '{verb}', which this manifest does not declare",
                        panel.id
                    ),
                ));
            };
            if tool.execution_kind != PluginExecutionKind::ReadOnly {
                return Err(PluginManifestError::new(
                    format!("{field}.source"),
                    format!(
                        "panel '{}' sources tool '{verb}', which is `execution_kind: mutating`; \
                         a dashboard panel may only read a `read_only` tool",
                        panel.id
                    ),
                ));
            }
            if let Some(refresh_ms) = panel.refresh_ms
                && !(MIN_PANEL_REFRESH_MS..=MAX_PANEL_REFRESH_MS).contains(&refresh_ms)
            {
                return Err(PluginManifestError::new(
                    format!("{field}.refresh_ms"),
                    format!(
                        "must be between {MIN_PANEL_REFRESH_MS} and {MAX_PANEL_REFRESH_MS} milliseconds"
                    ),
                ));
            }
        }
        for (index, link) in web.links.iter().enumerate() {
            let field = format!("spec.web.links[{index}]");
            if link.title.trim().is_empty() {
                return Err(PluginManifestError::new(
                    format!("{field}.title"),
                    "must not be empty",
                ));
            }
            if link.url.trim().is_empty() {
                return Err(PluginManifestError::new(
                    format!("{field}.url"),
                    "must not be empty",
                ));
            }
            // The dashboard sets a link's URL straight onto an anchor, so
            // the scheme is fixed here: a `javascript:` or `data:` tile would
            // be plugin-authored script running in the operator's session.
            if !LINK_URL_SCHEMES
                .iter()
                .any(|scheme| link.url.to_ascii_lowercase().starts_with(scheme))
            {
                return Err(PluginManifestError::new(
                    format!("{field}.url"),
                    format!(
                        "'{}' must be an http:// or https:// URL; a link tile is a plain \
                         hyperlink to a plugin-hosted UI",
                        link.url
                    ),
                ));
            }
            validate_template(&link.url, &format!("{field}.url"))?;
        }
        Ok(())
    }

    /// Every path the manifest names outside `spec.tools`: definition globs,
    /// skill directories and the config schema. All are plugin-root relative.
    fn validate_definition_paths(&self) -> Result<(), PluginManifestError> {
        if let Some(definitions) = &self.spec.definitions {
            for (patterns, key) in [
                (&definitions.activities, "activities"),
                (&definitions.jobs, "jobs"),
                (&definitions.routines, "routines"),
                (&definitions.auto_tasks, "auto_tasks"),
            ] {
                for (index, pattern) in patterns.iter().enumerate() {
                    validate_plugin_relative_path(
                        pattern,
                        &format!("spec.definitions.{key}[{index}]"),
                    )?;
                }
            }
        }
        for (index, skill) in self.spec.skills.iter().enumerate() {
            validate_plugin_relative_path(skill, &format!("spec.skills[{index}]"))?;
        }
        for (index, pattern) in self.spec.tests.iter().enumerate() {
            validate_plugin_relative_path(pattern, &format!("spec.tests[{index}]"))?;
        }
        if let Some(config) = &self.spec.config {
            if let Some(schema) = &config.schema {
                validate_plugin_relative_path(schema, "spec.config.schema")?;
            }
            if let Some(defaults) = &config.defaults
                && !defaults.is_object()
            {
                return Err(PluginManifestError::new(
                    "spec.config.defaults",
                    "must be a table of `[plugins.<ns>]` keys",
                ));
            }
        }
        Ok(())
    }

    /// Whether the manifest claims the reserved `orbit.<ns>.*` namespace.
    pub fn claims_first_party_namespace(&self) -> bool {
        self.metadata.origin == Some(PluginOrigin::Orbit)
    }

    /// `plugin:<ns>@<version>`: the provenance a seeded definition, a managed
    /// skill and the catalog layer all carry (§4.4).
    pub fn provenance(&self) -> String {
        plugin_provenance_label(&self.metadata.name, &self.metadata.version)
    }
}

/// The provenance label for a plugin at one version.
pub fn plugin_provenance_label(namespace: &str, version: &str) -> String {
    format!("plugin:{namespace}@{version}")
}

/// Reject a manifest path that would leave the plugin root before it is ever
/// joined to it: an absolute path, a `..` component, or an empty one.
///
/// Containment is still re-checked after canonicalisation at load; this is the
/// pure-data half so a manifest can be refused without touching a filesystem.
pub fn validate_plugin_relative_path(value: &str, field: &str) -> Result<(), PluginManifestError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(PluginManifestError::new(field, "must not be empty"));
    }
    if trimmed.starts_with('/') || trimmed.contains('\\') {
        return Err(PluginManifestError::new(
            field,
            format!("'{value}' must be a relative path inside the plugin root"),
        ));
    }
    if trimmed
        .split('/')
        .any(|component| component == ".." || component.is_empty())
    {
        return Err(PluginManifestError::new(
            field,
            format!("'{value}' must not contain an empty or '..' path component"),
        ));
    }
    // A `*` wildcard is only meaningful in the final component, where
    // `resolve_patterns` expands it against a directory listing. `*` in an
    // earlier component (`definitions/*/x.yaml`) is not a directory glob —
    // the loader looks up a literal directory named `*`, finds none, and the
    // pattern silently matches nothing.
    if let Some((directories, _file)) = trimmed.rsplit_once('/')
        && directories
            .split('/')
            .any(|component| component.contains('*'))
    {
        return Err(PluginManifestError::new(
            field,
            format!(
                "'{value}' uses '*' outside its final path component; a wildcard may only \
                 replace the file name, not a directory"
            ),
        ));
    }
    Ok(())
}
