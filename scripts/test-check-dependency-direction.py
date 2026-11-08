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
            "crates/orbit-core/src/application/auto_tasks",
            "crates/orbit-core/src/application/review",
            "crates/orbit-core/src/application/distributed",
            "crates/orbit-core/src/application/pull_drain",
            "crates/orbit-core/src/application/admission",
            "crates/orbit-core/src/application/workspace_auto",
            "crates/orbit-core/src/application/backlog_forecast",
            "crates/orbit-core/src/application/codebase",
            "crates/orbit-core/src/application/resident",
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

    def check_rows(self, rows, rejected, shared=None, base=None):
        """Run the guard once over independent `(relative, source)` rows.

        The guard reads each source file on its own and reports every hit as
        `<path>:<line>:`, so a rejected row must be named by its own path, which
        the exit status alone could not show. Accepted rows pass together only
        if each passes. Rows must reach one detector chain: the guard's `||`
        chains stop at the first detector with a hit, and a row hidden that way
        fails here by name. Each file keeps its row's directory depth, which is
        what a `super::` chain resolves against. `shared` holds exact-path files
        every row needs, such as a method's defining module. `base` is the source
        directory rows are relative to, by default orbit-core's application.
        """
        base = base or self.app
        paths = {}
        for index, (relative, source) in enumerate(rows):
            path = Path(os.path.normpath(base / relative))
            if path.name != "mod.rs":
                path = path.with_name(f"{path.stem}_row{index}.rs")
            self.assertNotIn(path, paths, relative)
            paths[path] = (relative, source)
        written = [Path(os.path.normpath(base / relative)) for relative in shared or {}]
        try:
            for path, source in zip(written, (shared or {}).values()):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            for path, (_, source) in paths.items():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            result = subprocess.run(["bash", str(self.guard)], env=self.env,
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.stderr, "", result.stderr)
            for path, row in paths.items():
                with self.subTest(row=row):
                    if rejected:
                        self.assertIn(f"{path}:", result.stdout)
                    else:
                        self.assertNotIn(f"{path}:", result.stdout)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        finally:
            for path in [*written, *paths]:
                path.unlink(missing_ok=True)

    def test_inherent_calls_from_task_into_upper_modules_rejected(self):
        # Each owner defines its own name, so a row's call is reported only when
        # the guard derives that owner's inherent names.
        owners = ("automation", "job", "review", "distributed", "pull_drain",
                  "admission", "backlog_forecast", "sweep_filing", "workspace_auto")
        self.check_rows(
            [("task/worker.rs",
              "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
              f"        self.fresh_{owner}_method();\n    }}\n}}\n") for owner in owners],
            rejected=True,
            shared={f"{owner}/state.rs":
                    f"impl OrbitRuntime {{\n    pub(crate) fn fresh_{owner}_method(&self) {{}}\n}}\n"
                    for owner in owners})

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

    def test_task_reaches_codebase_only_through_its_port(self):
        # [ORB-22625] Task imported codebase's block classifier and called its
        # done-landing hook directly, climbing task -> codebase -> distributed.
        # One detector per call: the guard's chains stop at the first hit.
        for rows in (
            [("task/blocked_recovery/readout.rs",
              "use crate::application::codebase::{BlockCause, classify_block};\n")],
            [("task/probe.rs", "use crate::application::{\n    codebase::Probe,\n};\n")],
            [("task/mod.rs", "use super::codebase::Probe;\n"),
             ("task/blocked_recovery/probe.rs",
              "use super::super::super::{codebase::Probe};\n")],
        ):
            self.check_rows(rows, rejected=True)
        self.check_rows([
            ("task/resolves.rs",
             "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
             "        self.fresh_codebase_method();\n    }\n}\n"),
        ], rejected=True, shared={
            "codebase/diff_stats.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_codebase_method(&self) {}\n}\n"})
        self.check_files({
            "task/mod.rs": "pub(in crate::application) trait TaskCodebasePort {\n"
                           "    fn record_landed_probe(&self);\n}\n",
            "task/resolves.rs":
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.record_landed_probe();\n    }\n}\n",
            "codebase/diff_stats.rs":
                "impl TaskCodebasePort for OrbitRuntime {\n    fn record_landed_probe(&self) {}\n}\n",
            "codebase/facts/block_cause.rs":
                "use crate::application::distributed::LeafSettlement;\n"
                "pub(in crate::application) use crate::application::task::classify_block;\n",
        }, rejected=False)

    def test_task_reaches_resident_only_through_its_port(self):
        # [ORB-22955] Task imported resident's comment redaction check and
        # called its grant-answer and comment-admission inherent methods,
        # closing a task <-> resident cycle. One detector per call: the
        # guard's chains stop at the first hit.
        for rows in (
            [("task/update/fields.rs", "use crate::application::resident;\n")],
            [("task/probe.rs", "use crate::application::{\n    resident::Probe,\n};\n")],
            [("task/mod.rs", "use super::resident::Probe;\n"),
             ("task/update/probe.rs", "use super::super::super::{resident::Probe};\n")],
        ):
            self.check_rows(rows, rejected=True)
        self.check_rows([
            ("task/decision/answering.rs",
             "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
             "        self.apply_resident_grant_answer();\n    }\n}\n"),
            ("task/update/locked.rs",
             "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
             "        self.validate_resident_comment_write();\n    }\n}\n"),
        ], rejected=True, shared={
            "resident/grant.rs":
                "impl OrbitRuntime {\n    pub(crate) fn apply_resident_grant_answer(&self) {}\n}\n",
            "resident/comments.rs":
                "impl OrbitRuntime {\n    pub(crate) fn validate_resident_comment_write(&self) {}\n}\n"})
        self.check_files({
            "task/resident_port.rs": "pub(in crate::application) trait TaskResidentPort {\n"
                                     "    fn admit_locked_resident_comment(&self);\n}\n",
            "task/update/locked.rs":
                "use crate::application::task::TaskResidentPort;\n"
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.admit_locked_resident_comment();\n    }\n}\n",
            "resident/comments.rs":
                "use crate::application::task::TaskUpdateParams;\n"
                "impl OrbitRuntime {\n    pub(crate) fn validate_resident_comment_write(&self) {}\n}\n",
            "resident/task_port.rs":
                "use crate::application::task::TaskResidentPort;\n"
                "impl TaskResidentPort for OrbitRuntime {\n"
                "    fn admit_locked_resident_comment(&self) {\n"
                "        self.validate_resident_comment_write();\n    }\n}\n",
        }, rejected=False)

    def test_lower_modules_cannot_reach_the_forecast(self):
        lowers = ("job", "automation", "review", "distributed", "pull_drain",
                  "admission", "workspace_auto")
        self.check_rows(
            [(f"{lower}/probe.rs",
              "impl OrbitRuntime {\n    pub(crate) fn probe(&self) {\n"
              "        self.fresh_forecast_method();\n    }\n}\n") for lower in lowers],
            rejected=True,
            shared={"backlog_forecast/runtime.rs":
                    "impl OrbitRuntime {\n    pub(crate) fn fresh_forecast_method(&self) {}\n}\n"})
        self.check_rows(
            [(f"{lower}/probe.rs", "use crate::application::backlog_forecast::Probe;\n")
             for lower in lowers],
            rejected=True)

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

    def test_review_cannot_name_codebase(self):
        # Selective-review retirement removed this edge, but the guard still
        # accepted its reintroduction. Exercise each detector independently.
        for rows in (
            [("review/admission.rs", "use crate::application::codebase::Probe;\n"),
             ("review/gate/context.rs",
              "fn probe() { crate::application::codebase::probe(); }\n")],
            [("review/admission.rs", "use crate::application::{codebase::Probe};\n"),
             ("review/gate/context.rs",
              "use crate::application::{task::{Other}, codebase::{Probe}};\n")],
            [("review/mod.rs", "use super::{codebase::Probe};\n"),
             ("review/admission.rs", "use super::super::codebase::Probe;\n"),
             ("review/gate/context.rs",
              "use super::{super::{super::{codebase::Probe}}};\n"),
             ("review/gate/admit/context.rs",
              "use super::super::super::super::super::{application::codebase::Probe};\n")],
        ):
            with self.subTest(rows=rows):
                self.check_rows(rows, rejected=True)

    def test_review_cannot_call_indexed_codebase_methods(self):
        # Derive ownership from the defining impl, including future methods;
        # the forecast's exact-line allowance must never apply to review.
        self.check_rows([
            ("review/admission.rs", f"fn probe(runtime: &OrbitRuntime) {{ {call}; }}\n")
            for call in (
                "runtime.fresh_codebase_method()",
                "OrbitRuntime::fresh_codebase_method(runtime)",
                "Self::fresh_codebase_method(runtime)",
                "<crate::OrbitRuntime>::fresh_codebase_method(runtime)",
                "runtime.task_riskiness_model()",
            )
        ], rejected=True, shared={
            "codebase/riskiness.rs":
                "impl OrbitRuntime {\n"
                "    pub(crate) fn fresh_codebase_method(&self) {}\n"
                "    pub(crate) fn task_riskiness_model(&self) {}\n}\n",
        })

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

    def test_task_and_job_reach_experiment_only_through_their_ports(self):
        # [ORB-23015] Task show, the provider-limit gate and job's crew
        # admission called experiment's inherent methods, and claimed-leaf
        # authorization named its free capture validation: a hidden cycle, as
        # experiment reads tasks, their crew source and the claimed leaf back.
        method = {
            "experiment/run_policy.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_pool_method(&self) {}\n}\n",
        }
        self.check_rows([
            ("task/provider_limit/gate.rs",
             "impl OrbitRuntime {\n    fn gate(&self) {\n"
             "        self.fresh_pool_method();\n    }\n}\n"),
            ("job/crew_pools/admission.rs",
             "impl OrbitRuntime {\n    fn admit(&self) {\n"
             "        self.fresh_pool_method();\n    }\n}\n"),
        ], rejected=True, shared=method)
        for module in ("task", "job"):
            for relative, source in (
                (f"{module}/claimed.rs", "use crate::application::experiment::Probe;\n"),
                (f"{module}/claimed.rs", "use crate::application::{experiment::Probe};\n"),
                (f"{module}/claimed.rs", "use super::super::experiment::Probe;\n"),
                (f"{module}/crew_pools/claims.rs",
                 "use super::super::super::{experiment::Probe};\n"),
            ):
                with self.subTest(relative=relative, source=source):
                    self.check_files({relative: source}, rejected=True)
        self.check_files({
            **method,
            "task/experiment_port.rs":
                "pub(in crate::application) trait TaskExperimentPort {\n"
                "    fn task_has_experiment_crew_pool(&self);\n}\n",
            "job/experiment_port.rs":
                "pub(in crate::application) trait JobExperimentPort {\n"
                "    fn job_experiment_crew_pool(&self);\n}\n",
            "experiment/task_port.rs":
                "impl TaskExperimentPort for OrbitRuntime {\n"
                "    fn task_has_experiment_crew_pool(&self) {\n"
                "        self.fresh_pool_method();\n    }\n}\n",
            "experiment/job_port.rs":
                "impl JobExperimentPort for OrbitRuntime {\n"
                "    fn job_experiment_crew_pool(&self) {\n"
                "        self.fresh_pool_method();\n    }\n}\n",
            "task/provider_limit/gate.rs":
                "use crate::application::task::TaskExperimentPort;\n"
                "impl OrbitRuntime {\n    fn gate(&self) {\n"
                "        self.task_has_experiment_crew_pool();\n    }\n}\n",
            "job/crew_pools/admission.rs":
                "use crate::application::job::JobExperimentPort;\n"
                "impl OrbitRuntime {\n    fn admit(&self) {\n"
                "        self.job_experiment_crew_pool();\n    }\n}\n",
            "experiment/assignment.rs":
                "use crate::application::task::TaskRecords;\n",
        }, rejected=False)

    def test_lower_modules_reach_landing_only_through_owned_port(self):
        # [ORB-22708] Task and job resolved claims through landing, and review
        # dispatched it directly; the missing guard admitted these cycles.
        method = {
            "landing/handoff.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_landing_method(&self) {}\n}\n",
        }
        for module in ("task", "job", "review", "distributed"):
            self.check_files({
                **method,
                f"{module}/probe.rs":
                    "impl OrbitRuntime {\n    fn probe(&self) {\n"
                    "        self.fresh_landing_method();\n    }\n}\n",
            }, rejected=True)
            for relative, source in (
                (f"{module}/probe.rs", "use crate::application::landing::Probe;\n"),
                (f"{module}/deep/probe.rs",
                 "fn probe() { crate::application::landing::step(); }\n"),
                (f"{module}/probe.rs",
                 "use crate::application::{\n    landing::Probe,\n};\n"),
                (f"{module}/mod.rs", "use super::landing::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::landing::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::{landing::Probe};\n"),
                (f"{module}/deep/probe.rs", "use super::super::super::{landing::Probe};\n"),
            ):
                with self.subTest(relative=relative, source=source):
                    self.check_files({relative: source}, rejected=True)
        self.check_files({
            **method,
            "task/handoff_claim.rs":
                "impl OrbitRuntime {\n    pub(crate) fn landing_claim(&self) {}\n}\n",
            "job/pr_repair/admission.rs":
                "impl OrbitRuntime {\n    fn admit(&self) { self.landing_claim(); }\n}\n",
            "review/landing_dispatch_port.rs":
                "pub(in crate::application) trait ReviewLandingDispatchPort {\n"
                "    fn dispatch_recorded_landing_authority(&self);\n}\n",
            "review/handoff.rs":
                "impl OrbitRuntime {\n    fn accept(&self) {\n"
                "        self.dispatch_recorded_landing_authority();\n    }\n}\n",
            "landing/review_port.rs":
                "impl ReviewLandingDispatchPort for OrbitRuntime {\n"
                "    fn dispatch_recorded_landing_authority(&self) {\n"
                "        self.fresh_landing_method();\n    }\n}\n",
            "landing/context.rs":
                "impl OrbitRuntime {\n    fn land(&self) { self.landing_claim(); }\n}\n",
            "landing/dispatch.rs": "use crate::application::job::pipeline;\n",
            "landing/identity.rs": "use crate::application::distributed::claim_receipt;\n",
        }, rejected=False)

    def test_landing_store_reads_do_not_allow_runtime_calls(self):
        method = {
            "landing/attempts.rs":
                "impl OrbitRuntime {\n    pub fn landing_attempts(&self) {}\n}\n",
        }
        self.check_files({
            **method,
            "task/external_merge/candidate.rs":
                "fn probe() {\n    let attempt = tasks\n"
                "        .landing_attempts()\n        .unwrap();\n}\n",
            "task/external_merge/rescued.rs":
                "fn probe() {\n    if tasks.landing_attempts()?.iter().any(|attempt| {\n"
                "        true\n    }) {}\n}\n",
            "review/handoff.rs":
                "fn probe() {\n    self.stores().tasks().landing_attempts()?\n}\n",
        }, rejected=False)
        for relative in ("task/external_merge/candidate.rs",
                         "task/external_merge/rescued.rs", "review/handoff.rs"):
            with self.subTest(relative=relative):
                self.check_files({
                    **method,
                    relative: "impl OrbitRuntime {\n    fn probe(&self) {\n"
                              "        self.landing_attempts();\n    }\n}\n",
                }, rejected=True)

    def test_automation_and_auto_tasks_cannot_reach_routines(self):
        # [ORB-22623] `routines` sits above both; automation's clock-settings
        # read and auto_tasks' display state were back-edges into it.
        method = {
            "routines/status/report.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_routine_method(&self) {}\n}\n",
        }
        for module in ("automation", "auto_tasks"):
            self.check_files({
                **method,
                f"{module}/probe.rs":
                    "impl OrbitRuntime {\n    fn probe(&self) {\n"
                    "        self.fresh_routine_method();\n    }\n}\n",
            }, rejected=True)
            for relative, source in (
                (f"{module}/probe.rs", "use crate::application::routines::Probe;\n"),
                (f"{module}/deep/probe.rs",
                 "fn probe(root: &Path) { crate::application::routines::clock::load(root); }\n"),
                (f"{module}/probe.rs", "use crate::application::{\n    routines::Probe,\n};\n"),
                (f"{module}/mod.rs", "use super::routines::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::routines::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::{routines::Probe};\n"),
                (f"{module}/deep/probe.rs", "use super::super::super::{routines::Probe};\n"),
            ):
                with self.subTest(relative=relative, source=source):
                    self.check_files({relative: source}, rejected=True)
        self.check_files({
            **method,
            "automation/condition/evaluate.rs":
                "fn cadence(root: &Path) { crate::application::contracts::load_clock_settings(root); }\n",
            "automation/pilot_presence.rs":
                "use orbit_automation::routines::loader::declared_routine_names;\n",
            "auto_tasks/listing.rs":
                "use crate::application::contracts::ScheduleDisplayState;\n",
            "routines/sweep.rs":
                "use crate::application::auto_tasks::scheduler::run;\n"
                "use crate::application::automation::SourceCache;\n"
                "use crate::application::backlog_forecast::record;\n",
        }, rejected=False)

    def test_resident_reaches_routine_status_only_through_its_port(self):
        # Resident's status lookup closed routines -> automation -> resident
        # -> routines. Exercise each detector separately so no hit masks one.
        for rows in (
            [("resident/probe.rs", "use crate::application::routines::Probe;\n"),
             ("resident/deep/probe.rs",
              "fn probe() { crate::application::routines::status::checkout_routine_statuses(); }\n")],
            [("resident/probe.rs", "use crate::application::{\n    routines::Probe,\n};\n")],
            [("resident/mod.rs", "use super::routines::Probe;\n"),
             ("resident/probe.rs", "use super::super::routines::Probe;\n"),
             ("resident/grouped.rs", "use super::super::{routines::Probe};\n"),
             ("resident/deep/probe.rs", "use super::super::super::{routines::Probe};\n")],
        ):
            self.check_rows(rows, rejected=True)
        method = {
            "routines/status/report.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_routine_method(&self) {}\n}\n",
        }
        self.check_rows([
            (f"resident/call_{index}.rs", f"fn probe() {{ {call}; }}\n")
            for index, call in enumerate((
                "self.fresh_routine_method()", "runtime.fresh_routine_method()",
                "OrbitRuntime::fresh_routine_method(runtime)",
                "Self::fresh_routine_method(runtime)",
                "<OrbitRuntime>::fresh_routine_method(runtime)",
                "<crate::OrbitRuntime>::fresh_routine_method(runtime)",
            ))
        ], rejected=True, shared=method)
        self.check_files({
            **method,
            "resident/routine_status_port.rs":
                "pub(in crate::application) trait ResidentRoutineStatusPort {\n"
                "    fn disabled_resident_cycle_routine_name(&self) -> Option<String>;\n}\n",
            "resident/mandate_reads.rs":
                "use super::ResidentRoutineStatusPort;\n"
                "impl OrbitRuntime {\n    pub(crate) fn resident_cycle_routine_disabled(&self) {\n"
                "        self.disabled_resident_cycle_routine_name();\n    }\n}\n",
            "routines/resident_port.rs":
                "use crate::application::resident::ResidentRoutineStatusPort;\n"
                "impl ResidentRoutineStatusPort for OrbitRuntime {\n"
                "    fn disabled_resident_cycle_routine_name(&self) -> Option<String> {\n"
                "        self.fresh_routine_method();\n        None\n    }\n}\n",
        }, rejected=False)

    def test_contracts_cannot_reach_routines_or_auto_tasks(self):
        # [ORB-22623] The moved leaves stay below the modules that read them.
        # Grouped relative imports escaped the guard; each super chain must
        # reach application from the fixture's actual module depth.
        for target in ("routines::clock::set_clock_cadence", "auto_tasks::AutoTaskListingRow"):
            for relative, source in (
                ("contracts/clock_settings.rs", f"use crate::application::{target};\n"),
                ("contracts/clock_settings.rs", f"use crate::application::{{{target}}};\n"),
                ("contracts/mod.rs", f"use super::{target};\n"),
                ("contracts/mod.rs", f"use super::{{{target}}};\n"),
                ("contracts/mod.rs", f"use super::{{\n    {target},\n}};\n"),
                ("contracts/clock_settings.rs", f"use super::super::{{{target}}};\n"),
                ("contracts/clock_settings/nested.rs",
                 f"use super::{{super::{{super::{{\n    {target},\n}}}}}};\n"),
            ):
                with self.subTest(relative=relative, source=source):
                    self.check_files({relative: source}, rejected=True)
        self.check_files({
            "contracts/mod.rs": "pub use self::clock_settings::ClockSettings;\n",
            "contracts/clock_settings.rs": "use super::{ScheduleDisplayState};\n",
            "contracts/clock_settings/nested.rs":
                "use super::super::{ScheduleDisplayState};\n",
        }, rejected=False)
        # [ORB-22674] A chain that climbs one step past application, to the crate
        # root, and then descends `application::<higher>` names the same module
        # as a chain that lands on application, so it is refused at each depth.
        for relative, source in (
            ("contracts/mod.rs", "use super::super::application::routines::Probe;\n"),
            ("contracts/mod.rs", "use super::super::{application::routines::Probe};\n"),
            ("contracts/mod.rs", "use super::super::{\n    application::routines::Probe,\n};\n"),
            ("contracts/clock_settings.rs",
             "use super::super::super::application::routines::Probe;\n"),
            ("contracts/clock_settings.rs",
             "use super::super::super::{application::auto_tasks::Probe};\n"),
            ("contracts/clock_settings.rs",
             "use super::super::super::{\n    application::routines::Probe,\n};\n"),
            ("contracts/clock_settings.rs",
             "fn probe() { super::super::super::application::routines::clock::load(); }\n"),
            ("contracts/clock_settings/nested.rs",
             "use super::super::super::super::application::routines::Probe;\n"),
            ("contracts/clock_settings/nested.rs",
             "use super::super::super::super::{application::routines::Probe};\n"),
            ("contracts/clock_settings/nested.rs",
             "use super::super::super::super::{\n    application::auto_tasks::Probe,\n};\n"),
        ):
            with self.subTest(relative=relative, source=source):
                self.check_files({relative: source}, rejected=True)

    def test_moved_clock_and_display_contracts_stay_in_contracts(self):
        # [ORB-22623] A second definition would bring the back-edge's owner back.
        for source in (
            "pub struct ClockSettings;\n",
            "pub fn load_clock_settings() {}\n",
            "pub enum ScheduleDisplayState {}\n",
        ):
            with self.subTest(source=source):
                self.check_files({"routines/clock/settings.rs": source}, rejected=True)
        self.check_files({
            "routines/clock/settings.rs":
                "pub use crate::application::contracts::{ClockSettings, load_clock_settings};\n",
            "routines/status/mod.rs":
                "pub use crate::application::contracts::ScheduleDisplayState;\n",
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
            self.check_rows(
                [(caller, "impl OrbitRuntime {\n    pub(crate) fn drive(&self) {\n"
                          f"        {call};\n    }}\n}}\n") for call in forms],
                rejected=True,
                shared={owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n"})

    def test_bare_and_other_type_calls_to_inherent_names_accepted(self):
        for owner, caller in (
            ("job/state.rs", "task/worker.rs"),
            ("../adapter/state.rs", "task_quality/runtime.rs"),
        ):
            self.check_rows(
                [(caller, f"fn drive(runtime: &OrbitRuntime) {{\n    {call};\n}}\n") for call in (
                    "fresh_owner_method(runtime)",
                    "<Other>::fresh_owner_method(runtime)",
                    "Vec::<OrbitRuntime>::fresh_owner_method(runtime)",
                )],
                rejected=False,
                shared={owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n"})

    def test_paths_into_bootstrap_and_composition_rejected(self):
        self.check_rows([
            (relative, f"use {tree};\n")
            for relative in ("workspace/sync.rs", "../adapter/tool_host/operation.rs")
            for module in ("bootstrap", "composition")
            for tree in (
                f"crate::{module}::Probe",
                f"crate::{{{module}::Probe}}",
                f"crate::{{runtime::{{Other}}, /* note */ {module}::Probe}}",
                f"super::super::super::{module}::Probe",
                f"super::super::super::{{{module}::Probe}}",
            )
        ], rejected=True)

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
            self.check_rows([(relative, f"use {tree};\n") for relative, tree in (
                ("job/worker.rs", f"super::super::{{{module}::Probe}}"),
                ("job/worker.rs", f"super::super::{{\n    RunOwnerLiveness,\n    {module}::{{Probe}},\n}}"),
                ("job/mod.rs", f"super::{{{module}::Probe}}"),
                ("job/pr_repair/adoption.rs", f"super::super::super::{{{module}::Probe}}"),
                ("job/worker.rs", f"super::{{super::{{{module}::Probe}}}}"),
            )], rejected=True)

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
            self.check_rows([
                (f"{owner}/worker.rs", f"use {tree};\n")
                for module in modules
                for comment in ("// note\n", "/* keep */", "/* outer /* nested */ keep */")
                for tree in (
                    f"super::super::{{{comment} {module}::Probe}}",
                    f"super::super::{{RunOwnerLiveness, {comment} {module}::Probe}}",
                    f"super::{comment} super::{{{module}::Probe}}",
                    f"super::super::{comment} {{{module}::Probe}}",
                    f"super::{{super::{{{comment} {module}::Probe}}}}",
                )
            ], rejected=True)

    def test_quoted_grouped_paths_accepted(self):
        rows = []
        for owner, module in (("job", "distributed"), ("task", "review")):
            tree = f"super::super::{{{module}::Probe}}"
            rows += [(f"{owner}/worker.rs", source + "\n") for source in (
                f"// use {tree};",
                f"/* use {tree}; */",
                f'const NOTE: &str = "use {tree};";',
                f'const NOTE: &str = "\\\" use {tree};";',
                f'const NOTE: &str = r"use {tree};";',
                f'const NOTE: &str = r##"\" # use {tree};"##;',
                f'const NOTE: &[u8] = br#"use {tree};"#;',
            )]
        self.check_rows(rows, rejected=False)

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
        self.check_rows([(snapshot, source + "\n") for source in (
            "use crate::resolved::default_crews;",
            "use crate::{resolved::default_crews, raw::Probe};",
            "use super::super::super::resolved::Probe;",
            "use crate::ResolvedConfig;",
            "fn probe() { crate::resolved::default_crews(); }",
            # [ORB-22452] Every crate-root re-export of resolved config from
            # lib.rs counts as the same edge, in use and inline form.
            "use crate::{CompatibilityConfigKey, CompatibilityConfigKeyDisposition, "
            "RETIRED_SYSTEM_CREW, RETIRED_SYSTEM_CREW_WARNING, "
            "compatibility_config_keys_at, compatibility_config_keys_present, "
            "disabled_crew_message, executor_host_os, names_retired_system_crew};",
            "use crate::executor_host_os;",
            "use crate::disabled_crew_message;",
            "fn probe() { crate::executor_host_os(); }",
            "fn probe() { crate::disabled_crew_message(); }",
        )], rejected=True, base=self.root / "crates/orbit-config/src")
        self.check_config(
            {snapshot: "use crate::crew_defaults::builtin_default_crews;\n"}, rejected=False)

    def test_config_crew_defaults_leaf_reaches_nothing_higher(self):
        leaf = "crew_defaults.rs"
        for source in (
            "use crate::resolved::Probe;",
            "use super::registry::Probe;",
            "use crate::raw::Probe;",
            "use toml::Value;",
            # [ORB-22452] Inline paths to crates outside std, orbit-common
            # and orbit-types count as the same edge as a use.
            "fn probe() -> toml::Value { toml::Value::Boolean(true) }",
            "fn probe() { let _ = serde_json::Value::Null; }",
            "fn probe() { let _ = serde::de::value::Error::custom(\"\"); }",
        ):
            with self.subTest(source=source):
                self.check_config({leaf: source + "\n"}, rejected=True)
        self.check_config({leaf: (
            "use std::collections::BTreeMap;\nuse orbit_common::model_defaults::{\n    A, B,\n};\n"
            "use orbit_types::identity::{Crew, CrewAssignment};\n")}, rejected=False)

    def test_config_domain_modules_cannot_name_the_registry(self):
        # [ORB-22707] The registry calls these modules' admitters, so they
        # must not reach back into it, directly or by a crate-root re-export.
        src = self.root / "crates/orbit-config/src"
        self.check_rows([(relative, source + "\n") for relative, source in (
            ("crew/ladder.rs", "use crate::ConfigSnapshot;"),
            ("crew/performance.rs", "impl crate::ConfigSnapshot {}"),
            ("operation/policy.rs", "use crate::registry::ConfigSnapshot;"),
            ("operation/layer.rs", "use super::super::registry::read_optional;"),
            ("provider_limit/policy.rs", "use crate::{ConfigSnapshot, CrewPerformancePolicy};"),
            ("provider_limit/budget.rs", "fn probe() { let _ = &crate::CONFIG_KEY_REGISTRY; }"),
        )], rejected=True, base=src)
        for leaf, source in (
            ("aliases.rs", "use crate::registry::read_optional;"),
            ("attention.rs", "use crate::registry::ConfigSnapshot;"),
            ("seeding.rs", "use crate::{describe_config_key, raw::Probe};"),
        ):
            with self.subTest(leaf=leaf):
                self.check_config({leaf: source + "\n"}, rejected=True)
        self.check_rows([(relative, source + "\n") for relative, source in (
            ("crew/ladder.rs", "use crate::config_document;"),
            ("operation/policy.rs", "use super::crew::ReviewCrewPool;"),
            ("provider_limit/policy.rs", "use crate::CrewPerformancePolicy;"),
        )], rejected=False, base=src)
        self.check_config({"aliases.rs": "use crate::config_document;\n"}, rejected=False)

    def test_config_document_leaf_admits_only_external_crates(self):
        leaf = "config_document.rs"
        for source in (
            "use crate::registry::Probe;",
            "use super::raw::Probe;",
            "fn probe() { crate::config_io::read(); }",
            "use serde_json::Value;",
        ):
            with self.subTest(source=source):
                self.check_config({leaf: source + "\n"}, rejected=True)
        self.check_config({leaf: (
            "use std::path::Path;\nuse orbit_common::OrbitError;\n"
            "use orbit_common::security::redaction::redact_home_dir;\n"
            "use serde::de::DeserializeOwned;\n")}, rejected=False)

    def check_crate_edge(self, dependency, kind, target, rejected):
        metadata = self.root / "metadata.json"
        manifest = self.root / "crates/orbit-core/Cargo.toml"
        metadata.write_text(json.dumps({
            "workspace_members": ["orbit-core"],
            "packages": [{"id": "orbit-core", "name": "orbit-core",
                          "manifest_path": str(manifest),
                          "dependencies": [{"name": dependency, "kind": kind,
                                            "target": target}]}],
        }))
        result = subprocess.run(["bash", str(self.guard)], env=self.env,
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.stderr, "", result.stderr)
        self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)

    def test_core_policy_edge_is_dev_only(self):
        # [ORB-22418] ARCHITECTURE.md records orbit-core -> orbit-policy as
        # dev-only and crates/orbit-core/Cargo.toml declares it under
        # [dev-dependencies]; the guard must reject a normal, build, or
        # target-normal edge while keeping the dev-only edge legal.
        # Production policy use stays with orbit-runtime and orbit-tools.
        for kind, target in ((None, None), ("build", None), (None, "cfg(unix)")):
            with self.subTest(kind=kind, target=target):
                self.check_crate_edge("orbit-policy", kind, target, rejected=True)
        for kind, target in (("dev", None), ("dev", "cfg(unix)")):
            with self.subTest(kind=kind, target=target):
                self.check_crate_edge("orbit-policy", kind, target, rejected=False)


if __name__ == "__main__":
    unittest.main()
