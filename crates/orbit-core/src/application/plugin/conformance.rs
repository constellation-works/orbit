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
use orbit_tools::plugin::{
    LoadedPlugin, McpBackend, McpExpectedTool, PluginBackend, PluginBackendSpec, PluginTool,
    PluginToolBinding, PluginValidationPolicy, load_plugin_dir, manifest_refusal,
    refuse_covering_fs_write_roots, validate_loaded_plugin,
};
use orbit_tools::{Tool, ToolContext};
use orbit_types::plugin::{
    PluginBackendType, PluginGrant, PluginManifest, PluginNetworkPermission, PluginProvenance,
    PluginSandbox, PluginTestCase, parse_grants, plugin_tool_name,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::plugin_host::{host_version, unmet_requirement};

/// One golden's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginTestOutcome {
    pub name: String,
    /// Canonical tool name the case called.
    pub tool: String,
    pub passed: bool,
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
    /// `--grant` names, parsed with the same rules as `orbit plugin enable`.
    /// Empty means the flag was omitted.
    pub grants: Vec<String>,
    /// `--accept-requested`: run under the manifest's full requested profile.
    pub accept_requested: bool,
}

pub fn test_plugin_dir(
    runtime: &OrbitRuntime,
    dir: &Path,
    options: &PluginTestOptions,
) -> Result<PluginTestReport, OrbitError> {
    let plugin = load_plugin_dir(dir)?;
    validate_loaded_plugin(&plugin, &PluginValidationPolicy::host_default())
        .map_err(manifest_refusal)?;
    if let Some(message) = unmet_requirement(&plugin) {
        return Err(OrbitError::InvalidInput(message));
    }
    let cases: Vec<&PluginTestCase> = plugin
        .tests
        .iter()
        .flat_map(|file| file.tests.iter())
        .collect();
    if cases.is_empty() {
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
    let global_root = sandbox_root.path().join("global");
    let workspace_root = sandbox_root.path().join("workspace");
    let state_dir = global_root.join("state/plugins").join(plugin.namespace());
    for dir in [&global_root, &workspace_root, &state_dir] {
        std::fs::create_dir_all(dir)
            .map_err(|error| OrbitError::Io(format!("create {}: {error}", dir.display())))?;
    }
    refuse_covering_fs_write_roots(&plugin, &global_root, &state_dir).map_err(manifest_refusal)?;

    let backend = conformance_backend(&plugin, &global_root, &state_dir);
    let mut results = Vec::with_capacity(cases.len());
    for case in cases {
        results.push(run_case(&plugin, &backend, &workspace_root, case));
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
    let (certified, note) = record_certification(runtime, &plugin, &report);
    report.certified = certified;
    report.certification_note = note;
    Ok(report)
}

/// Run one golden and compare the output to what the manifest promises.
fn run_case(
    plugin: &LoadedPlugin,
    backend: &PluginBackend,
    workspace_root: &Path,
    case: &PluginTestCase,
) -> PluginTestOutcome {
    let first_party = plugin.manifest.claims_first_party_namespace();
    let name = plugin_tool_name(plugin.namespace(), &case.tool, first_party);
    let Some(resolved) = plugin.tools.iter().find(|tool| tool.verb == case.tool) else {
        return PluginTestOutcome {
            name: case.name.clone(),
            tool: name,
            passed: false,
            detail: format!("the manifest declares no tool '{}'", case.tool),
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
        case.input.clone()
    };
    match tool.execute(&context, input) {
        Ok(output) if output == case.expect.output => PluginTestOutcome {
            name: case.name.clone(),
            tool: name,
            passed: true,
            detail: String::new(),
        },
        Ok(output) => PluginTestOutcome {
            name: case.name.clone(),
            tool: name,
            passed: false,
            detail: format!(
                "expected {}, got {}",
                compact(&case.expect.output),
                compact(&output)
            ),
        },
        Err(error) => PluginTestOutcome {
            name: case.name.clone(),
            tool: name,
            passed: false,
            detail: error.to_string(),
        },
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
        Vec::new()
    } else {
        parse_grants(&options.grants).map_err(OrbitError::InvalidInput)?
    };
    if options.accept_requested {
        return Ok(());
    }
    let missing: Vec<PluginGrant> = consent_required_grants(&plugin.manifest)
        .into_iter()
        .filter(|grant| !consented.contains(grant))
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

/// The backend a conformance run uses: the manifest's own permissions, with
/// every grant it requests treated as recorded.
fn conformance_backend(
    plugin: &LoadedPlugin,
    global_root: &Path,
    state_dir: &Path,
) -> PluginBackend {
    let grants: Vec<PluginGrant> = plugin.manifest.required_grants();
    let spec = Arc::new(PluginBackendSpec {
        provenance: PluginProvenance {
            name: plugin.namespace().to_string(),
            version: plugin.manifest.metadata.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            grants: grants
                .iter()
                .map(|grant| grant.as_str().to_string())
                .collect(),
        },
        plugin_root: plugin.root.clone(),
        state_dir: state_dir.to_path_buf(),
        global_root: global_root.to_path_buf(),
        command: plugin.backend_command.clone(),
        args: plugin.manifest.spec.backend.args.clone(),
        timeout_ms: plugin.manifest.spec.backend.timeout_ms,
        sandbox: plugin.manifest.spec.backend.sandbox,
        permissions: plugin.manifest.spec.permissions.clone(),
        programs: plugin.manifest.spec.requires.programs.clone(),
        config_defaults: plugin
            .config_defaults
            .iter()
            .filter_map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_string())))
            .collect(),
        grants,
    });
    match plugin.manifest.spec.backend.backend_type {
        PluginBackendType::Exec => PluginBackend::Exec(spec),
        PluginBackendType::Mcp => {
            let expected = plugin
                .tools
                .iter()
                .map(|tool| McpExpectedTool {
                    verb: tool.verb.clone(),
                    input_schema: tool
                        .input_schema_declared
                        .then(|| tool.input_schema.clone()),
                })
                .collect();
            PluginBackend::Mcp(Arc::new(McpBackend::new(spec, expected)))
        }
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
