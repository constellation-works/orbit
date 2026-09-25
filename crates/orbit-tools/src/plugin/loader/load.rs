use super::*;

/// Read `<root>/plugin.yaml`, validate its structure, resolve schema `$ref`s
/// inside the root, and locate the backend command.
pub fn load_plugin_dir(root: &Path) -> Result<LoadedPlugin, PluginLoadError> {
    let root = std::fs::canonicalize(root).map_err(|error| {
        PluginLoadError::io(
            format!("plugin root '{}': {error}", root.display()),
            error.kind(),
        )
    })?;
    refuse_plugin_tree_symlinks(&root)?;
    let manifest_path = root.join(MANIFEST_FILE_NAME);
    let bytes = std::fs::read(&manifest_path).map_err(|error| {
        PluginLoadError::io(
            format!("cannot read {}: {error}", manifest_path.display()),
            error.kind(),
        )
    })?;
    let manifest: PluginManifest = serde_yaml::from_slice(&bytes).map_err(|error| {
        PluginManifestError::new(
            manifest_field_from_yaml_error(&error),
            format!("invalid {MANIFEST_FILE_NAME}: {error}"),
        )
    })?;
    manifest.validate_structure()?;
    let manifest_digest = manifest_digest(&bytes);

    let backend_command = resolve_backend_command(&root, &manifest.spec.backend.command)?;

    let mut tools = Vec::with_capacity(manifest.spec.tools.len());
    for (index, tool) in manifest.spec.tools.iter().enumerate() {
        let input_schema_field = format!("spec.tools[{index}].input_schema");
        let input_schema = match &tool.input_schema {
            Some(schema) => resolve_schema(&root, schema, &input_schema_field)?,
            None => serde_json::json!({ "type": "object", "properties": {} }),
        };
        orbit_types::plugin::validate_plugin_cli_flags(&input_schema, &input_schema_field)?;
        // Re-checked here rather than only on the manifest: a `{ $ref }`
        // schema has no properties until it is read from the plugin root.
        if let Some(cli) = tool.cli.as_ref() {
            orbit_types::plugin::validate_plugin_cli_positionals(
                &tool.name,
                Some(&input_schema),
                &cli.positional,
                &format!("spec.tools[{index}].cli.positional"),
            )?;
        }
        // The input schema is compiled for its diagnostic only: nothing
        // validates a call's input against it, but a schema that cannot
        // compile is one the plugin's own backend is promised and no call
        // could ever satisfy, so it refuses the plugin here.
        if tool.input_schema.is_some() {
            compile_tool_schema(&tool.name, input_schema.clone(), &input_schema_field)?;
        }
        let output_schema = tool
            .output_schema
            .as_ref()
            .map(|schema| {
                let field = format!("spec.tools[{index}].output_schema");
                let resolved = resolve_schema(&root, schema, &field)?;
                compile_tool_schema(&tool.name, resolved, &field)
            })
            .transpose()?;
        tools.push(ResolvedPluginTool {
            verb: tool.name.clone(),
            description: tool.description.clone(),
            execution_kind: tool.execution_kind,
            mcp_scope: tool.mcp_scope,
            parameters: params_from_input_schema(&input_schema),
            input_schema,
            input_schema_declared: tool.input_schema.is_some(),
            output_schema,
        });
    }

    let definitions = resolve_definitions(&root, &manifest)?;
    let skills = resolve_skills(&root, &manifest)?;
    let (config_schema, config_defaults) = resolve_config_section(&root, &manifest)?;
    let tests = resolve_tests(&root, &manifest)?;

    Ok(LoadedPlugin {
        root,
        manifest,
        manifest_digest,
        backend_command,
        tools,
        definitions,
        skills,
        config_schema,
        config_defaults,
        tests,
    })
}

/// Read every `spec.tests` golden file and validate its structure (§5).
///
/// A golden that names a tool the manifest does not declare is a manifest
/// error: `orbit plugin test` could never run it, and a conformance suite
/// that silently skips a case certifies nothing.
fn resolve_tests(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<Vec<LoadedPluginTestFile>, PluginLoadError> {
    let paths = resolve_patterns(root, &manifest.spec.tests, "spec.tests")?;
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        let display = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let field = format!("spec.tests ({display})");
        let bytes = std::fs::read(&path).map_err(|error| {
            PluginManifestError::new(&field, format!("cannot be read: {error}"))
        })?;
        let file: PluginTestFile = serde_yaml::from_slice(&bytes)
            .map_err(|error| PluginManifestError::new(&field, format!("is invalid: {error}")))?;
        file.validate(&field)?;
        for case in &file.tests {
            if !manifest
                .spec
                .tools
                .iter()
                .any(|tool| tool.name == case.tool)
            {
                return Err(PluginManifestError::new(
                    &field,
                    format!(
                        "test '{}' calls tool '{}', which this manifest does not declare",
                        case.name, case.tool
                    ),
                )
                .into());
            }
        }
        files.push(LoadedPluginTestFile { path, file });
    }
    Ok(files)
}

/// Resolve every `spec.definitions` pattern against the plugin root.
fn resolve_definitions(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<PluginDefinitionFiles, PluginLoadError> {
    let Some(declared) = manifest.spec.definitions.as_ref() else {
        return Ok(PluginDefinitionFiles::default());
    };
    Ok(PluginDefinitionFiles {
        activities: resolve_patterns(root, &declared.activities, "spec.definitions.activities")?,
        jobs: resolve_patterns(root, &declared.jobs, "spec.definitions.jobs")?,
        routines: resolve_patterns(root, &declared.routines, "spec.definitions.routines")?,
        auto_tasks: resolve_patterns(root, &declared.auto_tasks, "spec.definitions.auto_tasks")?,
    })
}

/// Expand one list of manifest patterns into files inside the plugin root.
///
/// A pattern is either a literal path — which must exist — or a directory
/// plus a `*` wildcard in its final component. A wildcard whose directory is
/// absent matches nothing: a manifest may declare the conventional layout for
/// a kind it ships none of.
fn resolve_patterns(
    root: &Path,
    patterns: &[String],
    field: &str,
) -> Result<Vec<PathBuf>, PluginLoadError> {
    let mut resolved = Vec::new();
    for (index, pattern) in patterns.iter().enumerate() {
        let field = format!("{field}[{index}]");
        let pattern = pattern.trim();
        let (directory, file_pattern) = match pattern.rsplit_once('/') {
            Some((directory, file)) => (root.join(directory), file.to_string()),
            None => (root.to_path_buf(), pattern.to_string()),
        };
        if !file_pattern.contains('*') {
            let path = contained_path(root, &directory.join(&file_pattern), &field)?;
            if !path.is_file() {
                return Err(PluginManifestError::new(
                    field,
                    format!("'{pattern}' does not name a file inside the plugin root"),
                )
                .into());
            }
            resolved.push(path);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut matched = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !wildcard_matches(&file_pattern, name) {
                continue;
            }
            let path = contained_path(root, &entry.path(), &field)?;
            if path.is_file() {
                matched.push(path);
            }
        }
        matched.sort();
        resolved.extend(matched);
    }
    resolved.dedup();
    Ok(resolved)
}

/// Match one `*`-wildcard file pattern against a file name. `*` matches any
/// run of characters, including none; every other character is literal.
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    let mut segments = pattern.split('*');
    let Some(first) = segments.next() else {
        return false;
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let segments: Vec<&str> = segments.collect();
    let Some((last, middle)) = segments.split_last() else {
        return rest.is_empty();
    };
    for segment in middle {
        match rest.find(segment) {
            Some(at) => rest = &rest[at + segment.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

/// Canonicalise `candidate` and refuse anything outside the plugin root.
fn contained_path(root: &Path, candidate: &Path, field: &str) -> Result<PathBuf, PluginLoadError> {
    let resolved = std::fs::canonicalize(candidate).map_err(|error| {
        PluginManifestError::new(
            field,
            format!("'{}' cannot be read: {error}", candidate.display()),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(PluginManifestError::new(
            field,
            format!(
                "'{}' escapes the plugin root {}",
                resolved.display(),
                root.display()
            ),
        )
        .into());
    }
    Ok(resolved)
}

/// Resolve `spec.skills[]`: each entry is a directory holding a `SKILL.md`.
fn resolve_skills(root: &Path, manifest: &PluginManifest) -> Result<Vec<PathBuf>, PluginLoadError> {
    let mut skills = Vec::with_capacity(manifest.spec.skills.len());
    for (index, declared) in manifest.spec.skills.iter().enumerate() {
        let field = format!("spec.skills[{index}]");
        let path = contained_path(root, &root.join(declared.trim()), &field)?;
        if !path.join("SKILL.md").is_file() {
            return Err(PluginManifestError::new(
                field,
                format!("'{declared}' must be a directory containing SKILL.md"),
            )
            .into());
        }
        skills.push(path);
    }
    Ok(skills)
}

/// Read `spec.config.schema` and flatten `spec.config.defaults`.
///
/// A schema that does not compile, or defaults the schema itself rejects, is
/// a manifest error: the plugin would otherwise install a `[plugins.<ns>]`
/// section no value can satisfy (§4.9).
fn resolve_config_section(
    root: &Path,
    manifest: &PluginManifest,
) -> Result<(Option<Value>, BTreeMap<String, Value>), PluginLoadError> {
    let Some(config) = manifest.spec.config.as_ref() else {
        return Ok((None, BTreeMap::new()));
    };
    let schema = match config.schema.as_deref() {
        Some(relative) => {
            let path = contained_path(root, &root.join(relative.trim()), "spec.config.schema")?;
            let bytes = std::fs::read(&path).map_err(|error| {
                PluginManifestError::new(
                    "spec.config.schema",
                    format!("'{relative}' cannot be read: {error}"),
                )
            })?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
                PluginManifestError::new(
                    "spec.config.schema",
                    format!("'{relative}' is not valid JSON: {error}"),
                )
            })?;
            if !value.is_object() {
                return Err(PluginManifestError::new(
                    "spec.config.schema",
                    format!("'{relative}' must contain a JSON Schema object"),
                )
                .into());
            }
            Some(value)
        }
        None => None,
    };
    let defaults: BTreeMap<String, Value> = config
        .defaults
        .as_ref()
        .and_then(|defaults| defaults.as_object())
        .map(|defaults| {
            defaults
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    if let Some(schema) = &schema {
        let compiled = jsonschema::JSONSchema::compile(schema).map_err(|error| {
            PluginManifestError::new(
                "spec.config.schema",
                format!("is not a compilable JSON Schema: {error}"),
            )
        })?;
        let rendered = Value::Object(
            defaults
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        if let Err(errors) = compiled.validate(&rendered) {
            let details = errors.map(|error| error.to_string()).collect::<Vec<_>>();
            return Err(PluginManifestError::new(
                "spec.config.defaults",
                format!("are rejected by spec.config.schema: {}", details.join("; ")),
            )
            .into());
        }
    }
    Ok((schema, defaults))
}

/// serde_yaml names an unknown key in its message; surface the closest
/// dotted path it reports so the diagnostic points at a field.
fn manifest_field_from_yaml_error(error: &serde_yaml::Error) -> String {
    let message = error.to_string();
    if let Some(rest) = message.split("unknown field `").nth(1)
        && let Some((field, _)) = rest.split_once('`')
    {
        return field.to_string();
    }
    if let Some(rest) = message.split("missing field `").nth(1)
        && let Some((field, _)) = rest.split_once('`')
    {
        return field.to_string();
    }
    "manifest".to_string()
}

fn resolve_backend_command(root: &Path, command: &str) -> Result<PathBuf, PluginLoadError> {
    let raw = Path::new(command);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        PluginManifestError::new(
            "spec.backend.command",
            format!("'{}' is not present: {error}", candidate.display()),
        )
    })?;
    if !raw.is_absolute() && !resolved.starts_with(root) {
        return Err(PluginManifestError::new(
            "spec.backend.command",
            format!(
                "'{command}' resolves to {} outside the plugin root {}",
                resolved.display(),
                root.display()
            ),
        )
        .into());
    }
    Ok(resolved)
}

/// Resolve a top-level `{ $ref: <path> }` against the plugin root, refusing
/// any target that leaves it. An inline schema is returned as-is.
fn resolve_schema(root: &Path, schema: &Value, field: &str) -> Result<Value, PluginLoadError> {
    let Some(reference) = schema.get("$ref") else {
        return Ok(schema.clone());
    };
    let Some(reference) = reference.as_str() else {
        return Err(
            PluginManifestError::new(format!("{field}.$ref"), "must be a string path").into(),
        );
    };
    let raw = Path::new(reference);
    if raw.is_absolute() {
        return Err(PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' is absolute; a $ref must stay inside the plugin root"),
        )
        .into());
    }
    let candidate = root.join(raw);
    let resolved = std::fs::canonicalize(&candidate).map_err(|error| {
        PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' cannot be read: {error}"),
        )
    })?;
    if !resolved.starts_with(root) {
        return Err(PluginManifestError::new(
            format!("{field}.$ref"),
            format!(
                "'{reference}' escapes the plugin root {} (resolves to {})",
                root.display(),
                resolved.display()
            ),
        )
        .into());
    }
    let bytes = std::fs::read(&resolved).map_err(|error| {
        PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' cannot be read: {error}"),
        )
    })?;
    let value: Value = match resolved.extension().and_then(|value| value.to_str()) {
        Some("yaml" | "yml") => serde_yaml::from_slice(&bytes).map_err(|error| {
            PluginManifestError::new(
                format!("{field}.$ref"),
                format!("'{reference}' is not valid YAML: {error}"),
            )
        })?,
        _ => serde_json::from_slice(&bytes).map_err(|error| {
            PluginManifestError::new(
                format!("{field}.$ref"),
                format!("'{reference}' is not valid JSON: {error}"),
            )
        })?,
    };
    if !value.is_object() {
        return Err(PluginManifestError::new(
            format!("{field}.$ref"),
            format!("'{reference}' must contain a JSON Schema object"),
        )
        .into());
    }
    Ok(value)
}

/// Compile one resolved tool schema, naming the tool in the refusal.
///
/// A nested `$ref` that resolves to nothing and an invalid keyword are both
/// found here: §4.9 refuses the plugin for either, at load, rather than
/// letting every call fail.
fn compile_tool_schema(
    verb: &str,
    schema: Value,
    field: &str,
) -> Result<CompiledSchema, PluginLoadError> {
    CompiledSchema::compile(schema)
        .map_err(|error| PluginManifestError::new(field, format!("tool '{verb}': {error}")).into())
}
