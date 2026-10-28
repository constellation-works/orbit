use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use orbit_common::OrbitError;
use orbit_common::fs::io::atomic_write_text;
use orbit_tools::plugin::{
    DeliveredPluginSecret, LoadedPlugin, LoadedPluginTestFile, PluginBackend, PluginSecretSource,
    PluginTool, PluginToolBinding, PluginValidationPolicy, load_plugin_dir, manifest_refusal,
    physical_with_missing_tail, refuse_covering_fs_write_roots, resolve_declared_programs,
    resolve_plugin_root, validate_loaded_plugin,
};
use orbit_tools::{Tool, ToolContext};
use orbit_types::plugin::{
    FIXTURE_SECRET_VERSION, PluginGrant, PluginGrantSet, PluginManifest, PluginNetworkPermission,
    PluginProvenance, PluginSandbox, PluginTestCase, PluginTestExpectation, parse_grants,
    plugin_tool_name,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::plugin::backend::build_plugin_backend;
use crate::runtime::plugin::config::plugin_config_section;
use crate::runtime::plugin::requirements::{host_version, unmet_requirement};

use super::model::{PluginTestDeferral, PluginTestOptions, PluginTestOutcome, PluginTestReport};
use super::{gate, golden};

/// Run a plugin source's conformance goldens. `dir` is a checkout holding
/// `.orbit-plugin/` or that directory itself.
pub fn test_plugin_dir(
    runtime: &OrbitRuntime,
    dir: &Path,
    options: &PluginTestOptions,
) -> Result<PluginTestReport, OrbitError> {
    let mut plugin = load_plugin_dir(&resolve_plugin_root(dir)?)?;
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
    let consented = parse_consent(options)?;

    // One temp root stands in for both the global root and the workspace.
    // `{{workspace}}` and `{{plugin_state}}` render under it; any other root
    // is opened where it resolves, which is why consent below is decided on
    // the resolved roots rather than the manifest's spelling.
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
        orbit_common::fs::io::create_private_dir_all(dir)
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
    let fixture_secrets = Arc::new(FixtureSecrets::default());
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
        // The goldens' fixtures, swapped in per case; never the host store.
        Some(Arc::clone(&fixture_secrets) as Arc<dyn PluginSecretSource>),
    );
    refuse_covering_fs_write_roots(backend.spec(), None).map_err(manifest_refusal)?;
    // The profile a call from the conformance workspace compiles, so consent
    // reads the same rendered, resolved write set the sandbox will open. It
    // also repeats the protected-path refusal against the real workspace.
    let profile = backend.spec().sandbox_profile(Some(&workspace_root))?;
    let external_writes = writes_outside(&profile.write, &profile.write_files, &sandbox_path);
    authorize_conformance_run(
        &plugin,
        options,
        &consented,
        &external_writes,
        &requested_grants,
    )?;
    if let Some(reason) = gate::seatbelt_deferral(&profile, options, plugin.namespace())? {
        return deferred_report(&plugin, &case_locations, reason, requested_grants, options);
    }
    let mut results = Vec::with_capacity(case_locations.len());
    let mut changed_files = std::collections::BTreeSet::new();
    for (file_index, case_index) in case_locations {
        fixture_secrets.supply(&plugin.tests[file_index].file.tests[case_index].secrets);
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
                    output: golden::template_golden_value(&output, &workspace_root, &plugin.root),
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
        deferral: None,
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
    if let Some(gate) = &options.validation_gate {
        gate::write_validation_summary(gate, &report)?;
    }
    Ok(report)
}

/// The report of a run whose every selected case was deferred to a host
/// replay: no backend started, nothing certified.
fn deferred_report(
    plugin: &LoadedPlugin,
    case_locations: &[(usize, usize)],
    reason: String,
    requested_grants: String,
    options: &PluginTestOptions,
) -> Result<PluginTestReport, OrbitError> {
    let first_party = plugin.manifest.claims_first_party_namespace();
    let mut notices = Vec::with_capacity(case_locations.len());
    let results = case_locations
        .iter()
        .map(|&(file_index, case_index)| {
            let case = &plugin.tests[file_index].file.tests[case_index];
            let notice = orbit_exec::seatbelt_deferral_notice(
                &format!("{}/{}", plugin.namespace(), case.name),
                &reason,
            );
            notices.push(notice.clone());
            PluginTestOutcome {
                name: case.name.clone(),
                tool: plugin_tool_name(plugin.namespace(), &case.tool, first_party),
                passed: false,
                deferred: true,
                updated: false,
                detail: notice,
            }
        })
        .collect();
    let report = PluginTestReport {
        name: plugin.namespace().to_string(),
        version: plugin.manifest.metadata.version.clone(),
        root: plugin.root.to_string_lossy().into_owned(),
        manifest_digest: plugin.manifest_digest.clone(),
        orbit_version: host_version().to_string(),
        results,
        certified: false,
        certification_note: "not recorded: every case was deferred to a host replay".to_string(),
        requested_grants,
        deferral: Some(PluginTestDeferral { reason, notices }),
    };
    if let Some(gate) = &options.validation_gate {
        gate::write_validation_summary(gate, &report)?;
    }
    Ok(report)
}

/// The secret source of a conformance run: the current case's `secrets`
/// map, replaced before each case. One backend serves every case (an `mcp`
/// server is spawned once), and delivery reads the source per call, so the
/// case that is running is the case whose fixtures a call carries.
#[derive(Default)]
struct FixtureSecrets(Mutex<BTreeMap<String, DeliveredPluginSecret>>);

impl FixtureSecrets {
    fn supply(&self, secrets: &BTreeMap<String, String>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = secrets
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    DeliveredPluginSecret {
                        value: value.clone(),
                        version: FIXTURE_SECRET_VERSION.to_string(),
                    },
                )
            })
            .collect();
    }
}

impl PluginSecretSource for FixtureSecrets {
    fn read(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, DeliveredPluginSecret>, OrbitError> {
        let supplied = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(names
            .iter()
            .filter_map(|name| {
                supplied
                    .get(name)
                    .map(|secret| (name.clone(), secret.clone()))
            })
            .collect())
    }
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
                deferred: false,
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
        golden::render_golden_value(&case.input, workspace_root, &plugin.root)
    };
    match tool.execute(&context, input) {
        Ok(output) => {
            let (passed, detail) = match &case.expect {
                PluginTestExpectation::Output { output: expected } => {
                    let expected =
                        golden::render_golden_value(expected, workspace_root, &plugin.root);
                    (
                        output == expected,
                        (output != expected).then(|| {
                            format!(
                                "expected {}, got {}",
                                golden::compact(&expected),
                                golden::compact(&output)
                            )
                        }),
                    )
                }
                PluginTestExpectation::Error { error } => (
                    false,
                    Some(format!(
                        "expected plugin error code '{}', got output {}",
                        error.code,
                        golden::compact(&output)
                    )),
                ),
            };
            CaseExecution {
                outcome: PluginTestOutcome {
                    name: case.name.clone(),
                    tool: name,
                    passed,
                    deferred: false,
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
                                == Some(&golden::render_golden_value(
                                    detail,
                                    workspace_root,
                                    &plugin.root,
                                ))
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
                        golden::compact(&serde_json::to_value(expected).unwrap_or(Value::Null)),
                        payload.map_or_else(|| error.to_string(), golden::compact)
                    ),
                    PluginTestExpectation::Output { output } => format!(
                        "expected {}, got error: {}",
                        golden::compact(&golden::render_golden_value(
                            output,
                            workspace_root,
                            &plugin.root
                        )),
                        error
                    ),
                }
            };
            CaseExecution {
                outcome: PluginTestOutcome {
                    name: case.name.clone(),
                    tool: name,
                    passed,
                    deferred: false,
                    updated: false,
                    detail,
                },
                actual_output: None,
            }
        }
    }
}

/// The `--grant` list as consent, parsed before any scratch state exists so
/// an unknown name is refused first.
fn parse_consent(options: &PluginTestOptions) -> Result<PluginGrantSet, OrbitError> {
    if options.grants.is_empty() {
        Ok(PluginGrantSet::default())
    } else {
        Ok(parse_grants(&options.grants)?)
    }
}

/// Refuse a conformance run that would apply an unconfined backend, a write
/// root outside the scratch directory, `network: any`, or `env_pass` unless
/// the caller consented. Other requested grants stay on the profile the
/// manifest asked for.
fn authorize_conformance_run(
    plugin: &LoadedPlugin,
    options: &PluginTestOptions,
    consented: &PluginGrantSet,
    external_writes: &[PathBuf],
    requested_grants: &str,
) -> Result<(), OrbitError> {
    if options.accept_requested {
        return Ok(());
    }
    let missing: Vec<PluginGrant> =
        consent_required_grants(&plugin.manifest, !external_writes.is_empty())
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
    let external = if missing.contains(&PluginGrant::Fs) {
        format!(
            " `fs.write` resolves outside the conformance scratch directory to: {}.",
            external_writes
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    } else {
        String::new()
    };
    Err(OrbitError::InvalidInput(format!(
        "plugin '{}' requests grants `orbit plugin test` will not apply without consent. \
         Requested grants: {requested_grants}.{external} Re-run with `--accept-requested` to \
         test under the requested profile, or `--grant {missing_names}`",
        plugin.namespace()
    )))
}

/// Grants a conformance run will not apply on its own, in canonical order.
fn consent_required_grants(manifest: &PluginManifest, external_writes: bool) -> Vec<PluginGrant> {
    let mut grants = Vec::new();
    if external_writes {
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

/// The effective write roots and files that do not physically resolve
/// inside `scratch`, each read the way the sandbox compiles it: existing
/// ancestors canonicalized (symlinks and `..` followed), a missing tail kept.
fn writes_outside(roots: &[PathBuf], files: &[PathBuf], scratch: &Path) -> Vec<PathBuf> {
    let scratch = physical_with_missing_tail(scratch);
    roots
        .iter()
        .chain(files)
        .map(|path| physical_with_missing_tail(path))
        .filter(|path| !path.starts_with(&scratch))
        .collect()
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
