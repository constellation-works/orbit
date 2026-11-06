#!/usr/bin/env python3
"""Regress relative use-tree escapes through the public dependency guard."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parent


class RelativeImportTests(unittest.TestCase):
    def setUp(self):
        scratch = SCRIPTS.parent / ".orbit/tmp"
        scratch.mkdir(parents=True, exist_ok=True)
        temporary = tempfile.TemporaryDirectory(prefix="dependency-direction-", dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        scripts = self.root / "scripts"
        scripts.mkdir()
        self.guard = scripts / "check-dependency-direction.sh"
        shutil.copy2(SCRIPTS / self.guard.name, self.guard)
        (self.root / "Cargo.toml").write_text("[workspace.dependencies]\n")
        manifest = self.root / "crates/orbit-core/Cargo.toml"
        manifest.parent.mkdir(parents=True)
        manifest.write_text('[package]\nname = "orbit-core"\n')
        metadata = self.root / "metadata.json"
        metadata.write_text(json.dumps({
            "workspace_members": ["orbit-core"],
            "packages": [{"id": "orbit-core", "name": "orbit-core",
                          "manifest_path": str(manifest), "dependencies": []}],
        }))
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        cargo = bin_dir / "cargo"
        cargo.write_text('#!/bin/sh\nexec cat "$GUARD_TEST_METADATA"\n')
        cargo.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                        GUARD_TEST_METADATA=str(metadata))
        self.app = self.root / "crates/orbit-core/src/application"
        for directory in (
            "crates/orbit-core/src/application/contracts",
            "crates/orbit-core/src/application/task",
            "crates/orbit-core/src/application/job",
            "crates/orbit-core/src/application/automation",
            "crates/orbit-core/src/application/review",
            "crates/orbit-core/src/application/distributed",
            "crates/orbit-core/src/application/pull_drain",
            "crates/orbit-core/src/application/admission",
            "crates/orbit-core/src/application/workspace_auto",
            "crates/orbit-core/src/application/backlog_forecast",
            "crates/orbit-core/src/application/codebase",
            "crates/orbit-core/src/adapter", "crates/orbit-core/src/bootstrap",
            "crates/orbit-runtime/src", "crates/orbit-automation/src",
            "crates/orbit-store/src/contracts", "crates/orbit-store/src/driver/file",
            "crates/orbit-store/src/driver/sqlite", "crates/orbit-store/src/fs",
        ):
            (self.root / directory).mkdir(parents=True, exist_ok=True)

    def check_import(self, relative, source, rejected):
        path = self.app / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source + "\nfn probe() { let _: Option<Probe> = None; }\n")
        try:
            result = subprocess.run(["bash", str(self.guard)], env=self.env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        finally:
            path.unlink()

    def check_files(self, files, rejected):
        paths = [self.app / relative for relative in files]
        try:
            for path, source in zip(paths, files.values()):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            result = subprocess.run(["bash", str(self.guard)], env=self.env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        finally:
            for path in paths:
                path.unlink(missing_ok=True)

    def test_inherent_calls_from_task_into_upper_modules_rejected(self):
        for owner in ("automation", "job", "review", "distributed", "pull_drain",
                      "admission", "backlog_forecast", "sweep_filing", "workspace_auto"):
            with self.subTest(owner=owner):
                self.check_files({
                    f"{owner}/state.rs":
                        "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n",
                    "task/worker.rs":
                        "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                        "        self.fresh_owner_method();\n    }\n}\n",
                }, rejected=True)

    def test_inherent_calls_from_task_into_single_file_upper_module_rejected(self):
        # [ORB-22289] `tag_instructions` is one file, not a directory; its
        # inherent names were never derived, so task could call them unseen.
        self.check_files({
            "tag_instructions.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n",
            "task/worker.rs":
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.fresh_owner_method();\n    }\n}\n",
        }, rejected=True)

    def test_lower_modules_cannot_reach_the_forecast(self):
        forecast_method = {
            "backlog_forecast/runtime.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_forecast_method(&self) {}\n}\n",
        }
        for lower in ("job", "automation", "review", "distributed", "pull_drain",
                      "admission", "workspace_auto"):
            with self.subTest(lower=lower, edge="call"):
                self.check_files({
                    **forecast_method,
                    f"{lower}/probe.rs":
                        "impl OrbitRuntime {\n    pub(crate) fn probe(&self) {\n"
                        "        self.fresh_forecast_method();\n    }\n}\n",
                }, rejected=True)
            with self.subTest(lower=lower, edge="path"):
                self.check_import(f"{lower}/probe.rs",
                                  "use crate::application::backlog_forecast::Probe;", True)

    def test_task_names_the_forecast_only_in_its_two_shim_lines(self):
        shim = ("pub use crate::application::backlog_forecast::BacklogForecastHistory;\n"
                "pub use crate::application::backlog_forecast::COMPUTE_BUDGET_ENV;\n")
        self.check_files({"task/mod.rs": shim}, rejected=False)
        for relative, source in (
            ("task/mod.rs", "pub use crate::application::backlog_forecast::Options;\n"),
            ("task/worker.rs", shim),
            ("task/mod.rs", "use crate::application::{backlog_forecast::BacklogForecastHistory};\n"),
            ("task/mod.rs", "pub use super::backlog_forecast::BacklogForecastHistory;\n"),
        ):
            with self.subTest(relative=relative, source=source):
                self.check_files({relative: source}, rejected=True)

    def test_forecast_codebase_edge_is_exactly_the_authorized_call(self):
        codebase = "impl OrbitRuntime {\n    pub fn task_riskiness_model(&self) {}\n    pub fn other_codebase_method(&self) {}\n}\n"
        self.check_files({
            "codebase/riskiness.rs": codebase,
            "backlog_forecast/assembly.rs":
                "fn assemble(runtime: &OrbitRuntime) {\n"
                "    let risk_model = runtime.task_riskiness_model()?;\n}\n",
        }, rejected=False)
        for relative, source in (
            ("backlog_forecast/assembly.rs",
             "fn assemble(runtime: &OrbitRuntime) {\n    runtime.other_codebase_method();\n}\n"),
            ("backlog_forecast/projection.rs",
             "fn assemble(runtime: &OrbitRuntime) {\n"
             "    let risk_model = runtime.task_riskiness_model()?;\n}\n"),
            ("backlog_forecast/assembly.rs", "use crate::application::codebase::Probe;\n"),
            ("codebase/probe.rs", "use crate::application::backlog_forecast::Probe;\n"),
        ):
            with self.subTest(relative=relative, source=source):
                self.check_files({"codebase/riskiness.rs": codebase, relative: source}, rejected=True)

    def test_distributed_reaches_experiment_only_through_its_port(self):
        # [ORB-22416] Admission's capture call into experiment was a hidden
        # cycle: experiment's readout reads distributed's claim receipt.
        method = {
            "experiment/overlay.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_capture_method(&self) {}\n}\n",
        }
        self.check_files({
            **method,
            "distributed/serve/admission.rs":
                "impl OrbitRuntime {\n    fn admit(&self) {\n"
                "        self.fresh_capture_method();\n    }\n}\n",
        }, rejected=True)
        for relative, source in (
            ("distributed/serve/admission.rs", "use crate::application::experiment::Probe;\n"),
            ("distributed/serve/admission.rs", "use crate::application::{experiment::Probe};\n"),
            ("distributed/worker.rs", "use super::super::experiment::Probe;\n"),
            ("distributed/worker.rs", "use super::super::{experiment::Probe};\n"),
            ("distributed/serve/admission.rs", "use super::super::super::{experiment::Probe};\n"),
        ):
            with self.subTest(relative=relative, source=source):
                self.check_files({relative: source}, rejected=True)
        self.check_files({
            **method,
            "distributed/experiment_port.rs":
                "pub(in crate::application) trait DistributedExperimentPort {\n"
                "    fn capture_pull_experiments(&self);\n}\n",
            "experiment/distributed_port.rs":
                "impl DistributedExperimentPort for OrbitRuntime {\n"
                "    fn capture_pull_experiments(&self) {\n"
                "        self.fresh_capture_method();\n    }\n}\n",
            "distributed/serve/admission.rs":
                "impl OrbitRuntime {\n    fn admit(&self) {\n"
                "        self.capture_pull_experiments();\n    }\n}\n",
            "experiment/readout/admission.rs":
                "use crate::application::distributed::claim_receipt;\n",
        }, rejected=False)

    def test_forecast_cannot_reach_sweep_filing(self):
        self.check_import("backlog_forecast/probe.rs",
                          "use crate::application::sweep_filing::Probe;", True)

    def test_qualified_self_inherent_calls_rejected(self):
        # [ORB-22159] `<OrbitRuntime>::name(` names the same inherent method as
        # `OrbitRuntime::name(`, but the guard's previous-token filter skipped
        # it. The existing receiver and path forms stay refused beside it.
        forms = (
            "<OrbitRuntime>::fresh_owner_method(self)",
            "<crate::OrbitRuntime>::fresh_owner_method(self)",
            "<Self>::fresh_owner_method(self)",
            "OrbitRuntime::fresh_owner_method(self)",
            "crate::OrbitRuntime::fresh_owner_method(self)",
            "Self::fresh_owner_method(self)",
            "self.fresh_owner_method()",
        )
        for owner, caller in (
            ("job/state.rs", "task/worker.rs"),
            ("../adapter/state.rs", "task_quality/runtime.rs"),
        ):
            for call in forms:
                with self.subTest(owner=owner, call=call):
                    self.check_files({
                        owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n",
                        caller: "impl OrbitRuntime {\n    pub(crate) fn drive(&self) {\n"
                                f"        {call};\n    }}\n}}\n",
                    }, rejected=True)

    def test_bare_and_other_type_calls_to_inherent_names_accepted(self):
        for owner, caller in (
            ("job/state.rs", "task/worker.rs"),
            ("../adapter/state.rs", "task_quality/runtime.rs"),
        ):
            for call in (
                "fresh_owner_method(runtime)",
                "<Other>::fresh_owner_method(runtime)",
                "Vec::<OrbitRuntime>::fresh_owner_method(runtime)",
            ):
                with self.subTest(owner=owner, call=call):
                    self.check_files({
                        owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n",
                        caller: "fn drive(runtime: &OrbitRuntime) {\n"
                                f"    {call};\n}}\n",
                    }, rejected=False)

    def test_paths_into_bootstrap_and_composition_rejected(self):
        for layer, relative in (("application", "workspace/sync.rs"),
                                ("adapter", "../adapter/tool_host/operation.rs")):
            for module in ("bootstrap", "composition"):
                for tree in (
                    f"crate::{module}::Probe",
                    f"crate::{{{module}::Probe}}",
                    f"crate::{{runtime::{{Other}}, /* note */ {module}::Probe}}",
                    f"super::super::super::{module}::Probe",
                    f"super::super::super::{{{module}::Probe}}",
                ):
                    with self.subTest(layer=layer, tree=tree):
                        self.check_import(relative, f"use {tree};", rejected=True)

    def test_inherent_calls_into_bootstrap_and_composition_rejected(self):
        for owner in ("../composition/runtime.rs", "../bootstrap/init.rs"):
            for caller in ("task_quality/runtime.rs", "../adapter/engine_host/dispatch.rs"):
                with self.subTest(owner=owner, caller=caller):
                    self.check_files({
                        owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_joiner(&self) {}\n}\n",
                        caller: "impl Probe {\n    fn drive(&self, runtime: &OrbitRuntime) {\n"
                                "        runtime.fresh_joiner();\n    }\n}\n",
                    }, rejected=True)

    def test_joiner_ports_and_their_mentions_accepted(self):
        for caller in ("task_quality/runtime.rs", "../adapter/engine_host/dispatch.rs"):
            with self.subTest(caller=caller):
                self.check_files({
                    "../composition/runtime.rs":
                        "impl OrbitRuntime {\n    fn private_joiner(&self) {}\n"
                        "    pub(in crate::bootstrap) fn narrow_joiner(&self) {}\n}\n"
                        "impl AssistanceServicePort for OrbitRuntime {\n"
                        "    fn ported_service(&self) {}\n}\n",
                    caller: "// crate::bootstrap::init is only named here.\n"
                            'const NOTE: &str = "crate::composition::x";\n'
                            "fn drive(runtime: &OrbitRuntime) {\n"
                            "    runtime.ported_service();\n    runtime.private_joiner();\n"
                            "    runtime.narrow_joiner();\n}\n",
                    "../adapter/tests/fixture.rs": "use crate::bootstrap::init::Probe;\n",
                    "task/tests/fixture.rs": "use crate::composition::Probe;\n",
                }, rejected=False)

    def test_task_port_and_task_owned_calls_accepted(self):
        self.check_files({
            "task/mod.rs": "pub(crate) trait TaskAutomationPort {\n    fn schedule_probe(&self);\n}\n",
            "task/worker.rs":
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.schedule_probe();\n        self.task_local_probe();\n    }\n}\n",
            "task/state.rs": "impl OrbitRuntime {\n    pub(crate) fn task_local_probe(&self) {}\n}\n",
            "automation/port.rs":
                "impl TaskAutomationPort for OrbitRuntime {\n    fn schedule_probe(&self) {}\n}\n",
        }, rejected=False)

    def test_grouped_relative_imports_rejected(self):
        for module in ("distributed", "pull_drain"):
            for relative, tree in (
                ("job/worker.rs", f"super::super::{{{module}::Probe}}"),
                ("job/worker.rs", f"super::super::{{\n    RunOwnerLiveness,\n    {module}::{{Probe}},\n}}"),
                ("job/mod.rs", f"super::{{{module}::Probe}}"),
                ("job/pr_repair/adoption.rs", f"super::super::super::{{{module}::Probe}}"),
                ("job/worker.rs", f"super::{{super::{{{module}::Probe}}}}"),
            ):
                with self.subTest(module=module, relative=relative, tree=tree):
                    self.check_import(relative, f"use {tree};", rejected=True)

    def test_existing_direct_imports_rejected(self):
        for module in ("distributed", "pull_drain"):
            for prefix in ("crate::application", "super::super"):
                with self.subTest(module=module, prefix=prefix):
                    self.check_import("job/worker.rs", f"use {prefix}::{module}::Probe;",
                                      rejected=True)

    def test_commented_grouped_imports_rejected(self):
        for owner, modules in (
            ("job", ("distributed", "pull_drain", "review")),
            ("task", ("job", "review", "distributed", "sweep_filing", "workspace_auto")),
        ):
            for module in modules:
                for comment in ("// note\n", "/* keep */", "/* outer /* nested */ keep */"):
                    for tree in (
                        f"super::super::{{{comment} {module}::Probe}}",
                        f"super::super::{{RunOwnerLiveness, {comment} {module}::Probe}}",
                        f"super::{comment} super::{{{module}::Probe}}",
                        f"super::super::{comment} {{{module}::Probe}}",
                        f"super::{{super::{{{comment} {module}::Probe}}}}",
                    ):
                        with self.subTest(owner=owner, module=module, tree=tree):
                            self.check_import(f"{owner}/worker.rs", f"use {tree};",
                                              rejected=True)

    def test_quoted_grouped_paths_accepted(self):
        for owner, module in (("job", "distributed"), ("task", "review")):
            tree = f"super::super::{{{module}::Probe}}"
            for source in (
                f"// use {tree};",
                f"/* use {tree}; */",
                f'const NOTE: &str = "use {tree};";',
                f'const NOTE: &str = "\\\" use {tree};";',
                f'const NOTE: &str = r"use {tree};";',
                f'const NOTE: &str = r##"\" # use {tree};"##;',
                f'const NOTE: &[u8] = br#"use {tree};"#;',
            ):
                with self.subTest(owner=owner, source=source):
                    self.check_import(f"{owner}/worker.rs", source, rejected=False)

    def test_within_job_imports_accepted(self):
        for relative, tree in (
            ("job/pr_repair/adoption.rs", "super::super::{RunOwnerLiveness, run_owner_liveness}"),
            ("job/worker.rs", "super::{distributed::Probe, pull_drain::Other}"),
            ("job/pr_repair/adoption.rs", "super::super::{distributed::Probe}"),
        ):
            with self.subTest(relative=relative, tree=tree):
                self.check_import(relative, f"use {tree};", rejected=False)

    def test_test_fixtures_accepted(self):
        for relative in ("job/tests/worker.rs", "job/tests.rs"):
            with self.subTest(relative=relative):
                self.check_import(relative,
                                  "use super::super::{distributed::Probe, pull_drain::Other};",
                                  rejected=False)

    def check_config(self, files, rejected):
        src = self.root / "crates/orbit-config/src"
        paths = [src / relative for relative in files]
        try:
            for path, source in zip(paths, files.values()):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            result = subprocess.run(["bash", str(self.guard)], env=self.env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        finally:
            for path in paths:
                path.unlink(missing_ok=True)
            shutil.rmtree(src.parent, ignore_errors=True)

    def test_config_registry_cannot_name_resolved(self):
        snapshot = "registry/settings/snapshot.rs"
        for source in (
            "use crate::resolved::default_crews;",
            "use crate::{resolved::default_crews, raw::Probe};",
            "use super::super::super::resolved::Probe;",
            "use crate::ResolvedConfig;",
            "fn probe() { crate::resolved::default_crews(); }",
        ):
            with self.subTest(source=source):
                self.check_config({snapshot: source + "\n"}, rejected=True)
        self.check_config(
            {snapshot: "use crate::crew_defaults::builtin_default_crews;\n"}, rejected=False)

    def test_config_crew_defaults_leaf_reaches_nothing_higher(self):
        leaf = "crew_defaults.rs"
        for source in (
            "use crate::resolved::Probe;",
            "use super::registry::Probe;",
            "use crate::raw::Probe;",
            "use toml::Value;",
        ):
            with self.subTest(source=source):
                self.check_config({leaf: source + "\n"}, rejected=True)
        self.check_config({leaf: (
            "use std::collections::BTreeMap;\nuse orbit_common::model_defaults::{\n    A, B,\n};\n"
            "use orbit_types::identity::{Crew, CrewAssignment};\n")}, rejected=False)


if __name__ == "__main__":
    unittest.main()
