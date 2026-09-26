//! `orbit plugin test <dir>`: run a plugin's `spec.tests` goldens through the
//! real protocol and record the Orbit version they passed on (design §5).
//!
//! A temp directory stands in for the Orbit global root and the workspace, so
//! template paths (`{{workspace}}`, `{{plugin_root}}`, `{{plugin_state}}`,
//! `{{config.<key>}}`) do not touch the operator's Orbit state. The backend
//! runs under the profile the manifest requests for those template paths,
//! `network: loopback`, and `orbit_tools`: the run answers whether the plugin
//! would work once those grants are recorded.
//!
//! The run refuses, and the error prints the requested grant set, when the
//! manifest asks for an unconfined backend (`sandbox: none`), an absolute
//! `fs.write` root that is not a template, `network: any`, or any `env_pass`,
//! unless the caller passes `--accept-requested` or a `--grant` list that
//! names each of those grants. With that consent the run uses the requested
//! profile. Consent applies to this run only; it does not record a host grant.
//!
//! Certification is written only when this host has the same plugin
//! installed at the same manifest digest: a directory that differs from the
//! installed tree says nothing about the tree the host would run.

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_tools::plugin::{
    LoadedPlugin, LoadedPluginTestFile, PluginBackend, PluginTool, PluginToolBinding,
    PluginValidationPolicy, load_plugin_dir, manifest_refusal, refuse_covering_fs_write_roots,
    resolve_declared_programs, validate_loaded_plugin,
};
use orbit_tools::{Tool, ToolContext};
use orbit_types::plugin::{
    PluginGrant, PluginGrantSet, PluginManifest, PluginNetworkPermission, PluginProvenance,
    PluginSandbox, PluginTestCase, PluginTestExpectation, parse_grants, plugin_tool_name,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::plugin::backend::build_plugin_backend;
use crate::runtime::plugin::config::plugin_config_section;
use crate::runtime::plugin::requirements::{host_version, unmet_requirement};

/// One golden's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginTestOutcome {
    pub name: String,
    /// Canonical tool name the case called.
    pub tool: String,
    pub passed: bool,
    /// Whether `--update-goldens` replaced this case's expected output.
    pub updated: bool,
    /// Why it failed, or empty when it passed.
    pub detail: String,
}

/// What `orbit plugin test <dir>` found.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginTestReport {
    pub name: String,
    pub version: String,
    pub root: String,
    pub manifest_digest: String,
    /// The Orbit version the suite ran against.
    pub orbit_version: String,
    pub results: Vec<PluginTestOutcome>,
    /// Whether the certification was written to this host's plugin record,
    /// and why it was not when it was not.
    pub certified: bool,
    pub certification_note: String,
    /// The manifest's requested grant set, in the same spelling the refusal
    /// prints. `none` when the manifest requests nothing.
    pub requested_grants: String,
}

impl PluginTestReport {
    pub fn passed(&self) -> bool {
        !self.results.is_empty() && self.results.iter().all(|result| result.passed)
    }

    /// Names of the failing cases, for a one-line summary.
    pub fn failures(&self) -> Vec<&str> {
        self.results
            .iter()
            .filter(|result| !result.passed)
            .map(|result| result.name.as_str())
            .collect()
    }
}

/// How `orbit plugin test` was asked to treat a manifest's requested profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginTestOptions {
    /// Treat a directory that is not installed at this manifest digest as a
    /// verified first-party checkout, like `orbit plugin validate --first-party`.
    pub first_party: bool,
    /// `--grant` names, parsed with the same rules as `orbit plugin enable`.
    /// Empty means the flag was omitted.
    pub grants: Vec<String>,
    /// `--accept-requested`: run under the manifest's full requested profile.
    pub accept_requested: bool,
    /// `--case <name>`: run exactly one named golden.
    pub case: Option<String>,
    /// Replace mismatched output expectations with the actual output.
    pub update_goldens: bool,
}

pub fn test_plugin_dir(
    runtime: &OrbitRuntime,
    dir: &Path,
    options: &PluginTestOptions,
) -> Result<PluginTestReport, OrbitError> {
    let mut plugin = load_plugin_dir(dir)?;
    let first_party_verified = match runtime.stores().plugins().get_plugin(plugin.namespace())? {
        Some(installed) if installed.manifest_digest == plugin.manifest_digest => {
            installed.first_party
        }
        _ => options.first_party,
    };
    let policy =
        PluginValidationPolicy::host_default().with_first_party_verified(first_party_verified);
    validate_loaded_plugin(&plugin, &policy).map_err(manifest_refusal)?;
    if let Some(message) = unmet_requirement(&plugin) {
        return Err(OrbitError::InvalidInput(message));
    }
    let case_locations = selected_case_locations(&plugin, options.case.as_deref())?;
    if case_locations.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "plugin '{}' declares no `spec.tests` goldens; a plugin is certified by the tests it \
             ships (see `orbit plugin scaffold` for the shape)",
            plugin.namespace()
        )));
    }
    let requested_grants = format_requested_grants(&plugin.manifest);
    authorize_conformance_run(&plugin, options, &requested_grants)?;

    // One temp root stands in for both the global root and the workspace.
    // Template paths render under it. An absolute write root is opened as
    // declared, which is why that shape needs consent before this point.
    let sandbox_root = tempfile::tempdir()
        .map_err(|error| OrbitError::Io(format!("create the conformance workspace: {error}")))?;
    // Resolved once, because `sandbox-exec` matches the resolved path: a
    // macOS temp dir sits behind the `/var` -> `/private/var` alias, and a
    // profile spelled through it would miss the state-tree deny and the
    // write grants alike.
    let sandbox_path = sandbox_root
        .path()
        .canonicalize()
        .map_err(|error| OrbitError::Io(format!("resolve the conformance workspace: {error}")))?;
    let global_root = sandbox_path.join("global");
    let workspace_root = sandbox_path.join("workspace");
    let state_dir = global_root.join("state/plugins").join(plugin.namespace());
    // The shared backend spawn creates state_dir before confinement, just as
    // it does for installed plugins. Keep this scratch root fresh until then.
    for dir in [&global_root, &workspace_root] {
        std::fs::create_dir_all(dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    }
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))?;
    // A conformance run exercises the profile the *manifest* asks for, so
    // every grant it builds is the unscoped form. Operator consent above
    // decides whether the run happens, not how wide its profile is.
    let grants = PluginGrantSet::from_grants(plugin.manifest.required_grants());
    let backend = build_plugin_backend(
        &plugin,
        PluginProvenance {
            name: plugin.namespace().to_string(),
            version: plugin.manifest.metadata.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            grants: grants.to_recorded(),
        },
        &state_dir,
        &global_root,
        grants,
        plugin_config_section(&plugin, &config.plugins),
        // No operator has consented yet, so the programs resolve against this
        // process's `PATH`, exactly as `orbit plugin enable` from here would
        // record them.
        resolve_declared_programs(
            &plugin.manifest.spec.requires.programs,
            std::env::var_os("PATH").as_deref(),
        )
        .0,
    );
    refuse_covering_fs_write_roots(backend.spec(), None).map_err(manifest_refusal)?;
    let mut results = Vec::with_capacity(case_locations.len());
    let mut changed_files = std::collections::BTreeSet::new();
    for (file_index, case_index) in case_locations {
        let execution = run_case(
            &plugin,
            &backend,
            &workspace_root,
            &plugin.tests[file_index].file.tests[case_index],
        );
        let mut outcome = execution.outcome;
        if options.update_goldens
            && let Some(output) = execution.actual_output
            && !outcome.passed
        {
            plugin.tests[file_index].file.tests[case_index].expect =
                PluginTestExpectation::Output {
                    output: template_golden_value(&output, &workspace_root, &plugin.root),
                };
            changed_files.insert(file_index);
            outcome.passed = true;
            outcome.updated = true;
            outcome.detail = "updated expected output".to_string();
        }
        results.push(outcome);
    }
    for file_index in changed_files {
        write_golden_file(&plugin.tests[file_index])?;
    }

    let mut report = PluginTestReport {
        name: plugin.namespace().to_string(),
        version: plugin.manifest.metadata.version.clone(),
        root: plugin.root.to_string_lossy().into_owned(),
        manifest_digest: plugin.manifest_digest.clone(),
        orbit_version: host_version().to_string(),
        results,
        certified: false,
        certification_note: String::new(),
        requested_grants,
    };
    let (certified, note) = if options.case.is_some() {
        (
            false,
            "not recorded: a filtered --case run does not certify the full suite".to_string(),
        )
    } else {
        record_certification(runtime, &plugin, &report)
    };
    report.certified = certified;
    report.certification_note = note;
    Ok(report)
}

fn selected_case_locations(
    plugin: &LoadedPlugin,
    selected: Option<&str>,
) -> Result<Vec<(usize, usize)>, OrbitError> {
    let all: Vec<(usize, usize)> = plugin
        .tests
        .iter()
        .enumerate()
        .flat_map(|(file_index, loaded)| {
            loaded
                .file
                .tests
                .iter()
                .enumerate()
                .map(move |(case_index, _)| (file_index, case_index))
        })
        .collect();
    let Some(selected) = selected else {
        return Ok(all);
    };
    let matches: Vec<(usize, usize)> = all
        .into_iter()
        .filter(|(file_index, case_index)| {
            plugin.tests[*file_index].file.tests[*case_index].name == selected
        })
        .collect();
    match matches.len() {
        0 => Err(OrbitError::InvalidInput(format!(
            "plugin '{}' has no conformance case named '{selected}'",
            plugin.namespace()
        ))),
        1 => Ok(matches),
        _ => Err(OrbitError::InvalidInput(format!(
            "plugin '{}' declares conformance case '{selected}' in more than one file; case names \
             must be unique across the suite to use --case",
            plugin.namespace()
        ))),
    }
}

fn write_golden_file(loaded: &LoadedPluginTestFile) -> Result<(), OrbitError> {
    let mut encoded = serde_yaml::to_string(&loaded.file)
        .map_err(|error| OrbitError::Io(format!("encode {}: {error}", loaded.path.display())))?;
    if !encoded.ends_with('\n') {
        encoded.push('\n');
    }
    atomic_write_text(&loaded.path, &encoded)
        .map_err(|error| OrbitError::Io(format!("write {}: {error}", loaded.path.display())))
}

struct CaseExecution {
    outcome: PluginTestOutcome,
    actual_output: Option<Value>,
}

/// Run one golden and compare the output to what the manifest promises.
fn run_case(
    plugin: &LoadedPlugin,
    backend: &PluginBackend,
    workspace_root: &Path,
    case: &PluginTestCase,
) -> CaseExecution {
    let first_party = plugin.manifest.claims_first_party_namespace();
    let name = plugin_tool_name(plugin.namespace(), &case.tool, first_party);
    let Some(resolved) = plugin.tools.iter().find(|tool| tool.verb == case.tool) else {
        return CaseExecution {
            outcome: PluginTestOutcome {
                name: case.name.clone(),
                tool: name,
                passed: false,
                updated: false,
                detail: format!("the manifest declares no tool '{}'", case.tool),
            },
            actual_output: None,
        };
    };
    let binding = Arc::new(PluginToolBinding {
        provenance: backend.spec().provenance.clone(),
        execution_kind: resolved.execution_kind,
        diagnostic: None,
    });
    let tool = PluginTool {
        name: name.clone(),
        verb: resolved.verb.clone(),
        description: resolved.description.clone(),
        parameters: resolved.parameters.clone(),
        input_schema: resolved
            .input_schema_declared
            .then(|| resolved.input_schema.clone()),
        execution_kind: resolved.execution_kind,
        output_schema: resolved.output_schema.clone(),
        binding,
        backend: backend.clone(),
    };
    let context = ToolContext {
        cwd: Some(workspace_root.to_string_lossy().into_owned()),
        workspace_root: Some(workspace_root.to_path_buf()),
        // `requires.programs` is checked against this list the same way a
        // call from an activity is; a conformance run grants exactly what
        // the manifest declares it spawns.
        proc_allowed_programs: plugin.manifest.spec.requires.programs.clone(),
        ..ToolContext::default()
    };
    let input = if case.input.is_null() {
        Value::Object(Default::default())
    } else {
        render_golden_value(&case.input, workspace_root, &plugin.root)
    };
    match tool.execute(&context, input) {
        Ok(output) => {
            let (passed, detail) = match &case.expect {
                PluginTestExpectation::Output { output: expected } => {
                    let expected = render_golden_value(expected, workspace_root, &plugin.root);
                    (
                        output == expected,
                        (output != expected).then(|| {
                            format!("expected {}, got {}", compact(&expected), compact(&output))
                        }),
                    )
                }
                PluginTestExpectation::Error { error } => (
                    false,
                    Some(format!(
                        "expected plugin error code '{}', got output {}",
                        error.code,
                        compact(&output)
                    )),
                ),
            };
            CaseExecution {
                outcome: PluginTestOutcome {
                    name: case.name.clone(),
                    tool: name,
                    passed,
                    updated: false,
                    detail: detail.unwrap_or_default(),
                },
                actual_output: Some(output),
            }
        }
        Err(error) => {
            let payload = match &error {
                OrbitError::RemoteTool { payload, .. } => Some(payload),
                _ => None,
            };
            let passed = match (&case.expect, payload) {
                (PluginTestExpectation::Error { error: expected }, Some(actual)) => {
                    actual["code"] == expected.code
                        && expected
                            .retryable
                            .is_none_or(|retryable| actual["retryable"] == retryable)
                        && expected.detail.as_ref().is_none_or(|detail| {
                            actual.get("detail")
                                == Some(&render_golden_value(detail, workspace_root, &plugin.root))
                        })
                }
                _ => false,
            };
            let detail = if passed {
                String::new()
            } else {
                match &case.expect {
                    PluginTestExpectation::Error { error: expected } => format!(
                        "expected plugin error {}, got {}",
                        compact(&serde_json::to_value(expected).unwrap_or(Value::Null)),
                        payload.map_or_else(|| error.to_string(), compact)
                    ),
                    PluginTestExpectation::Output { output } => format!(
                        "expected {}, got error: {}",
                        compact(&render_golden_value(output, workspace_root, &plugin.root)),
                        error
                    ),
                }
            };
            CaseExecution {
                outcome: PluginTestOutcome {
                    name: case.name.clone(),
                    tool: name,
                    passed,
                    updated: false,
                    detail,
                },
                actual_output: None,
            }
        }
    }
}

fn render_golden_value(value: &Value, workspace: &Path, plugin_root: &Path) -> Value {
    match value {
        Value::String(text) => Value::String(
            text.replace("{{workspace}}", &workspace.to_string_lossy())
                .replace("{{plugin_root}}", &plugin_root.to_string_lossy()),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_golden_value(item, workspace, plugin_root))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        render_golden_value(value, workspace, plugin_root),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn template_golden_value(value: &Value, workspace: &Path, plugin_root: &Path) -> Value {
    match value {
        Value::String(text) => {
            let plugin_root = plugin_root.to_string_lossy();
            let workspace = workspace.to_string_lossy();
            Value::String(
                text.replace(plugin_root.as_ref(), "{{plugin_root}}")
                    .replace(workspace.as_ref(), "{{workspace}}"),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| template_golden_value(item, workspace, plugin_root))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        template_golden_value(value, workspace, plugin_root),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

/// Refuse a conformance run that would apply an unconfined backend, an
/// absolute non-template write root, `network: any`, or `env_pass` unless
/// the caller consented. Other requested grants stay on the profile the
/// manifest asked for.
fn authorize_conformance_run(
    plugin: &LoadedPlugin,
    options: &PluginTestOptions,
    requested_grants: &str,
) -> Result<(), OrbitError> {
    let consented = if options.grants.is_empty() {
        PluginGrantSet::default()
    } else {
        parse_grants(&options.grants).map_err(OrbitError::InvalidInput)?
    };
    if options.accept_requested {
        return Ok(());
    }
    let missing: Vec<PluginGrant> = consent_required_grants(&plugin.manifest)
        .into_iter()
        .filter(|grant| !consented.contains(*grant))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let missing_names = missing
        .iter()
        .map(|grant| grant.as_str())
        .collect::<Vec<_>>()
        .join(",");
    Err(OrbitError::InvalidInput(format!(
        "plugin '{}' requests grants `orbit plugin test` will not apply without consent. \
         Requested grants: {requested_grants}. Re-run with `--accept-requested` to test under \
         the requested profile, or `--grant {missing_names}`",
        plugin.namespace()
    )))
}

/// Grants a conformance run will not apply on its own, in canonical order.
fn consent_required_grants(manifest: &PluginManifest) -> Vec<PluginGrant> {
    let mut grants = Vec::new();
    if manifest
        .spec
        .permissions
        .fs
        .write
        .iter()
        .any(|path| is_absolute_non_template_write(path))
    {
        grants.push(PluginGrant::Fs);
    }
    if manifest.spec.permissions.network == PluginNetworkPermission::Any {
        grants.push(PluginGrant::Network);
    }
    if !manifest.spec.permissions.env_pass.is_empty() {
        grants.push(PluginGrant::EnvPass);
    }
    if manifest.spec.backend.sandbox == PluginSandbox::None {
        grants.push(PluginGrant::Unsandboxed);
    }
    grants
}

/// An `fs.write` entry the sandbox would open as given: absolute, and not a
/// `{{...}}` template. Template roots render inside the temp workspace.
fn is_absolute_non_template_write(path: &str) -> bool {
    let trimmed = path.trim();
    !trimmed.contains("{{") && Path::new(trimmed).is_absolute()
}

/// The requested grant set, one entry per grant the manifest actually asks
/// for. Details come from the manifest (`write=/var/plugin-cache`, `any`).
fn format_requested_grants(manifest: &PluginManifest) -> String {
    let parts: Vec<String> = manifest
        .grant_requests()
        .into_iter()
        .filter_map(|request| {
            request
                .requested
                .map(|detail| format!("{} ({detail})", request.grant.as_str()))
        })
        .collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join("; ")
    }
}

/// Write "certified for <version>" onto this host's record, when the suite
/// passed and the record describes the very tree that was tested.
fn record_certification(
    runtime: &OrbitRuntime,
    plugin: &LoadedPlugin,
    report: &PluginTestReport,
) -> (bool, String) {
    if !report.passed() {
        return (
            false,
            format!(
                "not certified: {} of {} test(s) failed",
                report.failures().len(),
                report.results.len()
            ),
        );
    }
    let installed = match runtime.stores().plugins().get_plugin(plugin.namespace()) {
        Ok(Some(installed)) => installed,
        Ok(None) => {
            return (
                false,
                format!(
                    "not recorded: plugin '{}' is not installed on this host; run `orbit plugin \
                     add` first to record the certification",
                    plugin.namespace()
                ),
            );
        }
        Err(error) => return (false, format!("not recorded: {error}")),
    };
    if installed.manifest_digest != plugin.manifest_digest {
        return (
            false,
            format!(
                "not recorded: the installed '{}' has manifest digest {} and this directory has \
                 {}; reinstall with `orbit plugin add --force` to certify the installed tree",
                plugin.namespace(),
                installed.manifest_digest,
                plugin.manifest_digest
            ),
        );
    }
    match runtime
        .stores()
        .plugins()
        .set_plugin_certification(plugin.namespace(), Some(&report.orbit_version))
    {
        Ok(true) => (
            true,
            format!(
                "certified for {} and recorded on this host",
                report.orbit_version
            ),
        ),
        Ok(false) => (
            false,
            format!(
                "not recorded: no plugin record named '{}' to write to",
                plugin.namespace()
            ),
        ),
        Err(error) => (false, format!("not recorded: {error}")),
    }
}
