//! Consolidated key families [ORB-16775]: every deprecated spelling loads to
//! the same effective configuration as its replacement, warns by name, and
//! keeps the layer that decided it.

use tempfile::tempdir;

use super::operation::{capture_logs, warned_deprecated};
use super::review_layers_write_config;
use orbit_config::ConfigRoots;
use orbit_config::ConfigValueSourceKind;
use orbit_config::OperationLayerSource;
use orbit_config::ResolvedConfig;
use orbit_config::load_effective_config;

/// One config pair written as files: the global then the workspace body.
struct Layers {
    global: &'static str,
    workspace: &'static str,
}

/// Every effective `(key, value, layer)` row `config show` reports for
/// `layers`, beside the load's log.
fn effective(
    layers: &Layers,
) -> (
    Vec<(String, serde_json::Value, ConfigValueSourceKind)>,
    String,
) {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    review_layers_write_config(global.path(), layers.global);
    review_layers_write_config(workspace.path(), layers.workspace);
    let roots = ConfigRoots::new(global.path(), workspace.path());
    let (config, logs) = capture_logs(|| load_effective_config(&roots));
    let rows = config
        .unwrap_or_else(|error| panic!("{} / {} loads: {error}", layers.global, layers.workspace))
        .values()
        .iter()
        .map(|row| (row.key.clone(), row.value.clone(), row.source.kind()))
        .collect();
    (rows, logs)
}

fn load_error(global_body: &str, workspace_body: &str) -> String {
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    review_layers_write_config(global.path(), global_body);
    review_layers_write_config(workspace.path(), workspace_body);
    ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect_err("the load is refused")
        .to_string()
}

#[test]
fn every_deprecated_spelling_loads_like_its_replacement_and_warns() {
    let cases: &[(&str, Layers, Layers, &[&str])] = &[
        (
            "the review crew",
            Layers {
                global: "",
                workspace: "[operation]\nreview_crew = [\"sol\", \"grok\"]\n",
            },
            Layers {
                global: "",
                workspace: "[workflow.purpose_crews]\nreview = [\"sol\", \"grok\"]\n",
            },
            &["operation.review_crew"],
        ),
        (
            "the final recovery pool",
            Layers {
                global: "[workflow]\nfinal_recovery_crews = [\"sol:100\", \"grok:20\"]\n",
                workspace: "",
            },
            Layers {
                global: "[workflow.purpose_crews]\nfinal_recovery = [\"sol:100\", \"grok:20\"]\n",
                workspace: "",
            },
            &["workflow.final_recovery_crews"],
        ),
        (
            "the sweep pools naming one pool",
            Layers {
                global: "",
                workspace: "[workflow.purpose_crews]\nci_failure_sweep = [\"sol\"]\n\
                            dependabot = [\"sol\"]\nmerged_head_repair = [\"sol\"]\n",
            },
            Layers {
                global: "",
                workspace: "[workflow.purpose_crews]\nsweep_tasks = [\"sol\"]\n",
            },
            &[
                "workflow.purpose_crews.ci_failure_sweep",
                "workflow.purpose_crews.dependabot",
                "workflow.purpose_crews.merged_head_repair",
            ],
        ),
        (
            "before-landing review",
            Layers {
                global: "[review]\nbefore_landing = true\n",
                workspace: "",
            },
            Layers {
                global: "[review]\ntiming = \"before_landing\"\n",
                workspace: "",
            },
            &["review.before_landing"],
        ),
        (
            "a workspace before-PR switch over a global before-landing one",
            Layers {
                global: "[review]\nbefore_landing = true\n",
                workspace: "[review]\nbefore_pr = true\nbefore_landing = false\n",
            },
            Layers {
                global: "[review]\ntiming = \"before_landing\"\n",
                workspace: "[review]\ntiming = \"before_pr\"\n",
            },
            &["review.before_pr", "review.before_landing"],
        ),
        (
            "strict worker containment",
            Layers {
                global: "[machine]\nworker_containment = true\nworker_containment_strict = true\n",
                workspace: "",
            },
            Layers {
                global: "[machine]\nworker_containment = \"strict\"\n",
                workspace: "",
            },
            &[
                "machine.worker_containment_strict",
                "machine.worker_containment",
            ],
        ),
        (
            "boolean worker containment",
            Layers {
                global: "[machine]\nworker_containment = false\n",
                workspace: "",
            },
            Layers {
                global: "[machine]\nworker_containment = \"off\"\n",
                workspace: "",
            },
            &["machine.worker_containment"],
        ),
        (
            "a login-only shell probe",
            Layers {
                global: "",
                workspace: "[workflow.validation_env]\ninteractive = false\n",
            },
            Layers {
                global: "",
                workspace: "[workflow.validation_env]\nshell_probe = \"login\"\n",
            },
            &["workflow.validation_env.interactive"],
        ),
        (
            "a shell probe switched off globally",
            Layers {
                global: "[workflow.validation_env]\nlogin_shell = false\n",
                workspace: "",
            },
            Layers {
                global: "[workflow.validation_env]\nshell_probe = \"off\"\n",
                workspace: "",
            },
            &["workflow.validation_env.login_shell"],
        ),
        (
            "the live files on the delivery hosts",
            Layers {
                global: "[workflow]\nfinal_recovery_crews = [\"sol\"]\n",
                workspace: "[operation]\nreview_crew = [\"sol\", \"grok\"]\n\n\
                            [review]\nbefore_pr = false\nbefore_landing = false\n",
            },
            Layers {
                global: "[workflow.purpose_crews]\nfinal_recovery = [\"sol\"]\n",
                workspace: "[workflow.purpose_crews]\nreview = [\"sol\", \"grok\"]\n\n\
                            [review]\ntiming = \"off\"\n",
            },
            &[
                "workflow.final_recovery_crews",
                "operation.review_crew",
                "review.before_pr",
                "review.before_landing",
            ],
        ),
    ];
    for (name, old, new, deprecated) in cases {
        let (old_values, logs) = effective(old);
        let (new_values, new_logs) = effective(new);
        assert_eq!(old_values, new_values, "{name}: effective configuration");
        for key in *deprecated {
            assert!(
                warned_deprecated(&logs, key),
                "{name}: warns {key}:\n{logs}"
            );
            assert!(
                !warned_deprecated(&new_logs, key),
                "{name}: the replacement does not warn {key}:\n{new_logs}"
            );
        }
    }
}

/// A run captures the layer that decided its review timing and an upgrade
/// resume compares it, so a workspace that only switched the inherited half
/// off keeps the lower layer's decision.
#[test]
fn a_workspace_stating_only_an_undeciding_half_keeps_the_lower_layer() {
    let load = |global_body: &str, workspace_body: &str| {
        let global = tempdir().expect("global tempdir");
        let workspace = tempdir().expect("workspace tempdir");
        review_layers_write_config(global.path(), global_body);
        review_layers_write_config(workspace.path(), workspace_body);
        ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
            .expect("config loads")
            .operation
    };

    let built_in = load("", "[review]\nbefore_landing = false\n");
    assert!(!built_in.review_before_pr.value && !built_in.review_before_landing.value);
    assert_eq!(
        built_in.review_before_pr.source,
        OperationLayerSource::BuiltIn
    );
    assert_eq!(
        built_in.review_before_landing.source,
        OperationLayerSource::BuiltIn
    );

    let global = load(
        "[review]\nbefore_pr = true\n",
        "[review]\nbefore_landing = false\n",
    );
    assert!(global.review_before_pr.value);
    assert_eq!(global.review_before_pr.source, OperationLayerSource::Global);

    // The half that decides is stated here, so the workspace owns the timing.
    let workspace = load("", "[review]\nbefore_pr = false\n");
    assert_eq!(
        workspace.review_before_pr.source,
        OperationLayerSource::Workspace
    );
}

#[test]
fn deprecated_spellings_that_cannot_merge_are_refused_with_the_keys_named() {
    let error = load_error(
        "",
        "[workflow.purpose_crews]\nci_failure_sweep = [\"sol\"]\ndependabot = [\"grok\"]\n",
    );
    assert!(
        error.contains("workflow.purpose_crews.ci_failure_sweep")
            && error.contains("workflow.purpose_crews.dependabot")
            && error.contains("workflow.purpose_crews.sweep_tasks"),
        "{error}"
    );

    let error = load_error(
        "[machine]\nworker_containment = false\nworker_containment_strict = true\n",
        "",
    );
    assert!(
        error.contains("machine.worker_containment_strict"),
        "{error}"
    );

    // The new key in the same file wins over its deprecated spelling.
    let global = tempdir().expect("global tempdir");
    let workspace = tempdir().expect("workspace tempdir");
    review_layers_write_config(
        workspace.path(),
        "[operation]\nreview_crew = \"grok\"\n\n[workflow.purpose_crews]\nreview = \"sol\"\n",
    );
    let config = ResolvedConfig::load(&ConfigRoots::new(global.path(), workspace.path()))
        .expect("config loads");
    assert_eq!(config.operation.review_crew.value, ["sol"]);
}
