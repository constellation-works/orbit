#!/usr/bin/env python3
"""Regress relative use-tree escapes through the public dependency guard."""

from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import zlib

import dependency_guard_batches as batches
import dependency_guard_fixture


SCRIPTS = Path(__file__).resolve().parent

# Every guard launch goes through `run_guard`, so the summary line reports how
# many the suite made.
GUARD_RUNS = [0]
GUARD_RUNS_LOCK = threading.Lock()

# `None` runs each batch at once. The concurrent runner replaces it with a dict
# of lists, and `test_deferred_batches_report_as_expected` runs the batches the
# other cases handed over in few guard runs (see dependency_guard_batches.py).
DEFERRED = None
DEFERRED_LOCK = threading.Lock()
DEFERRED_CASE = "test_deferred_batches_report_as_expected"


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
        self.app = self.root / dependency_guard_fixture.CORE_APPLICATION
        # The mandatory scan roots are shared with test-ci-fast-guards.py.
        dependency_guard_fixture.create_scan_layout(self.root)

    def test_engine_context_dispatch_contract_direction(self):
        # RuntimeHost owns the contracts below dispatch, including its null
        # host and test fixtures. Shared orbit-types paths remain legal.
        engine = self.root / "crates/orbit-engine/src"
        self.check_rows([
            (relative, source + "\n")
            for relative in ("context/hosts/runtime_host/probe.rs",
                             "context/hosts/null_host/defaults/probe.rs",
                             "context/tests/probe.rs")
            for source in (
                "use crate::activity_job::DispatchError;",
                "use crate::{context::RuntimeHost, activity_job::{DispatchError}};",
                "fn probe() { crate::activity_job::dispatch_v2_activity(); }",
                f"use {'super::' * len(Path(relative).parts)}{{activity_job::DispatchError}};",
            )
        ], rejected=True, base=engine)
        self.check_rows([
            ("context/hosts/probe.rs",
             "use crate::context::dispatch_contract::DispatchError;\n"
             "use orbit_types::workflow::activity_job::Provider;\n"
             "// crate::activity_job is above the port.\n"
             'const NOTE: &str = "crate::activity_job::DispatchError";\n'),
        ], rejected=False, base=engine)
        self.check_rejected_exact_rows(engine, [
            ("context/dispatch_contract.rs", source, 1) for source in (
                "use crate::context::RuntimeHost;\n",
                "use crate::{context::{RuntimeHost}};\n",
                "fn probe() { crate::activity_job::dispatch_v2_activity(); }\n",
            )
        ])
        self.check_files({
            "context/dispatch_contract.rs":
                "use std::path::PathBuf;\n"
                "use orbit_common::OrbitError;\n"
                "use orbit_types::workflow::activity_job::Provider;\n"
                "use serde_json::Value;\n"
                "use thiserror::Error;\n",
        }, rejected=False, base=engine)

    def test_vcs_checks_dependency_direction(self):
        # The shared classifier must not reach back into landing, and its
        # consumers must not recreate the old checks route through landing.
        vcs = self.root / "crates/orbit-engine/src/executor/automation/vcs"
        self.check_rows([
            ("checks/probe.rs", source + "\n") for source in (
                "use super::super::landing::Probe;",
                "use crate::executor::automation::vcs::{landing::Probe};",
                "use super::super::{git::Probe, landing::{Other}};",
            )
        ], rejected=True, base=vcs)
        self.check_rows([
            (f"{consumer}/probe.rs", source + "\n")
            for consumer in ("pr", "base", "claim")
            for source in (
                "use super::super::landing::checks::Probe;",
                "fn probe() { super::super::landing::checks::read(); }",
                "use super::super::{landing::{checks::Probe}};",
                "use super::super::landing::{ /* note */ checks::Probe};",
                "use super::super::landing as l;\nuse l::checks::Probe;",
                "use super::super::{landing as l};\nuse l::{checks::Probe};",
                "use crate::executor::automation::vcs::landing as l;\nfn probe() { l::checks::read(); }",
                "use super::super::landing::{self as l};\nuse l::checks::Probe;",
                "use super::super::landing as l;\nuse l as m;\nuse m::checks::Probe;",
            )
        ], rejected=True, base=vcs)
        self.check_rows([
            ("checks/probe.rs", "use super::super::pr_answers::Probe;\n"
             "// landing::checks is the retired path.\n"
             'const EVIDENCE: &str = "the landing waits";\n'),
            *[(f"{consumer}/probe.rs", "use super::super::checks::Probe;\n"
               "use super::super::landing as l;\nuse l::Probe as Other;\n")
              for consumer in ("pr", "base", "claim")],
        ], rejected=False, base=vcs)

    def test_vcs_pr_answers_dependency_direction(self):
        # The read model sits below checks, pr, landing, base, push and claim.
        # A path segment is an edge; a bare identifier or a string is not.
        vcs = self.root / "crates/orbit-engine/src/executor/automation/vcs"
        above = (
            "use super::pr::Probe;",
            "use super::checks::Probe;",
            "use super::landing::Probe;",
            "use super::base::Probe;",
            "use super::push::Probe;",
            "use super::claim::Probe;",
            "use super::{pr::Probe};",
            "use super::pr as renamed;\nfn probe() { renamed::Probe; }",
            "fn probe() { operations::pr::read(); }",
            "pub(in crate::executor::automation::vcs::pr) fn previous() {}",
        )
        named_pr = (
            "use super::pr::Probe;",
            "use super::super::pr::Probe;",
            "use super::{pr::Probe};",
            "fn probe() { crate::executor::automation::vcs::pr::Probe; }",
            "use super::pr as renamed;\nfn probe() { renamed::read(); }",
        )
        self.check_rows(
            [("pr_answers/probe.rs", source + "\n") for source in above]
            + [(consumer, source + "\n")
               for consumer in ("checks/probe.rs", "base/probe.rs", "candidate_resume/probe.rs")
               for source in named_pr],
            rejected=True, base=vcs)
        self.check_rejected_exact_rows(vcs, [
            (relative, source + "\n", 1)
            for relative in ("push.rs", "claim/published.rs")
            for source in named_pr
        ])
        self.check_rows([
            ("pr_answers/probe.rs",
             "use super::operations;\n"
             "use super::answers::PullRequestStatus;\n"
             "fn keep(base: &str) {\n"
             "    let pr = 1;\n"
             "    let checks = Vec::new();\n"
             "    let _ = (pr, checks, base);\n"
             "}\n"
             'const NOTE: &str = "pr::checks landing base push claim";\n'
             "// landing::pr\n"
             "pub(in crate::executor::automation::vcs) fn previous_candidate_sha() {}\n"),
            ("checks/probe.rs",
             "use super::super::pr_answers::PullRequestStatus;\n"
             "fn keep(pr: u64) { let _ = pr; }\n"
             'const KEY: &str = "pr";\n'),
            ("base/probe.rs", "fn keep() { let pr = 1; let _ = pr; }\n"),
            ("candidate_resume/probe.rs",
             "use super::super::pr_answers::identity::mismatches;\n"
             "use super::super::pr_answers::PullRequestStatus;\n"
             "fn keep(pr: u64) { let _ = (pr, mismatches, PullRequestStatus); }\n"
             'const KEY: &str = "pr";\n'),
        ], rejected=False, base=vcs)
        self.check_files({
            "push.rs": "fn keep(pr: u64) { let _ = pr; }\n",
            "claim/published.rs": 'fn keep() { let pr = "pr"; let _ = pr; }\n',
        }, rejected=False, base=vcs)

    def test_missing_config_scan_paths_fail_closed(self):
        src = self.root / "crates/orbit-config/src"
        for relative in ("aliases.rs", "registry"):
            with self.subTest(path=relative):
                path = src / relative
                parked = path.with_name(path.name + ".parked")
                path.rename(parked)
                try:
                    result = self.run_guard()
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertNotIn("dependency direction guard passed", result.stdout)
                finally:
                    parked.rename(path)

    def test_missing_tag_instructions_scan_path_fails_closed(self):
        path = self.app / "tag_instructions.rs"
        path.unlink()
        result = self.run_guard()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("dependency scan path missing", result.stderr)
        self.assertIn("tag_instructions.rs", result.stderr)
        self.assertNotIn("dependency direction guard passed", result.stdout)

    def test_failed_workspace_dependency_listing_does_not_pass(self):
        cargo = self.root / "bin" / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            "count_file=\"$GUARD_TEST_CARGO_COUNT\"\n"
            "count=0\n"
            "if [ -f \"$count_file\" ]; then count=$(cat \"$count_file\"); fi\n"
            "count=$((count + 1))\n"
            "printf '%s\\n' \"$count\" > \"$count_file\"\n"
            "if [ \"$count\" -ge 2 ]; then\n"
            "  echo \"metadata unavailable\" >&2\n"
            "  exit 1\n"
            "fi\n"
            "exec cat \"$GUARD_TEST_METADATA\"\n"
        )
        cargo.chmod(0o755)
        count_file = self.root / "cargo-count"
        env = dict(self.env, GUARD_TEST_CARGO_COUNT=str(count_file))
        result = self.run_guard(env)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("workspace dependency listing failed", result.stdout + result.stderr)
        self.assertNotIn("dependency direction guard passed", result.stdout)

    def test_helper_only_scan_roots_fail_closed_when_missing(self):
        # super_chains_into and inherent_calls_into rglob a module. A missing
        # path must fail closed, the way the ripgrep roots already do. These
        # two are not ripgrep roots: routines is only an inherent defining
        # module, and orbit-core runtime is only a helper scan target.
        cases = (
            self.app / "routines",
            self.root / "crates/orbit-core/src/runtime",
        )
        for path in cases:
            with self.subTest(path=path.name):
                path.rmdir()
                try:
                    result = self.run_guard()
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn("dependency scan path missing", result.stderr)
                    self.assertIn(path.name, result.stderr)
                    self.assertNotIn("dependency direction guard passed", result.stdout)
                finally:
                    path.mkdir()

    def test_missing_scan_paths_fail_closed(self):
        for relative in ("task", "contracts", "automation"):
            with self.subTest(path=relative):
                path = self.app / relative
                path.rmdir()
                try:
                    result = self.run_guard()
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertNotIn("dependency direction guard passed", result.stdout)
                finally:
                    path.mkdir()

    def run_guard(self, env=None):
        """Run the public guard script once over the case's fixture tree."""
        with GUARD_RUNS_LOCK:
            GUARD_RUNS[0] += 1
        return subprocess.run(["bash", str(self.guard)], env=env or self.env,
                              capture_output=True, text=True, check=False)

    def defer_batch(self, kind, *details):
        """Hand a batch to the deferred run instead of running the guard now.

        Returns False, running nothing, when the concurrent runner is not
        collecting, so the caller checks the batch itself.
        """
        if DEFERRED is None:
            return False
        sequence = getattr(self, "batch_sequence", 0)
        self.batch_sequence = sequence + 1
        batch = (batches.AcceptedBatch if kind == "accepted" else batches.RejectedBatch)(
            self.id(), sequence, *details)
        with DEFERRED_LOCK:
            DEFERRED[kind].append(batch)
        return True

    def deferred_tag(self):
        """A file-name token no other case's rows share, so rows never collide."""
        return f"{zlib.crc32(self.id().encode()):08x}_{getattr(self, 'batch_sequence', 0)}_"

    def relative_path(self, base, relative):
        return Path(os.path.normpath(base / relative)).relative_to(self.root)

    def accepted_problems(self, files):
        """Run the guard over `files` (relative to the root); name what it reports."""
        with self.fixture_files(self.root, files):
            result = self.run_guard()
            problems = []
            if result.stderr:
                problems.append(f"stderr: {result.stderr}")
            if result.returncode != 0:
                problems.append(f"exit {result.returncode}: {result.stdout}")
            problems += [f"reported {relative}" for relative in files
                         if f"{self.root / relative}:" in result.stdout]
            return problems

    def run_rejected_group(self, group):
        """Run the guard over one group's tree; return (reported claims, result)."""
        with self.fixture_files(self.root, group.files()):
            result = self.run_guard()
        return batches.reported_rows(group, self.root, result.stdout), result

    def rejected_problems(self, batch):
        """Run one rejected batch alone, as an isolated case would; name what is wrong."""
        group = batches.RejectedGroup()
        group.try_add(batch)
        reported, result = self.run_rejected_group(group)
        problems = []
        if result.stderr:
            problems.append(f"stderr: {result.stderr}")
        if result.returncode != 1:
            problems.append(f"exit {result.returncode}, expected 1: {result.stdout}")
        problems += [f"not reported: {claim[1].path}: {claim[1].source!r}"
                     for claim in group.claims if claim not in reported]
        return problems

    def test_deferred_batches_report_as_expected(self):
        """Run the batches the other cases deferred, grouped into few guard runs.

        The runner calls this case after the others finish. Accepted batches run
        together per compatible group. Rejected batches run in waves: each wave
        removes the rows the guard reported, so a row the guard's `||` chains hid
        behind an earlier detector's hit is reported in a later wave, without
        the row that hid it. A row left over when a wave reports nothing new fails.
        """
        deferred = DEFERRED or {"accepted": [], "rejected": []}
        for files, members in batches.pack_accepted(deferred["accepted"]):
            problems = self.accepted_problems(files)
            if not problems:
                continue
            # Attribute the failure: rerun each deferred batch alone.
            failed_alone = False
            for batch in members:
                with self.subTest(deferred_from=batch.test, batch=batch.sequence):
                    alone = self.accepted_problems(batch.files)
                    failed_alone |= bool(alone)
                    self.assertEqual(alone, [], alone)
            if not failed_alone:
                self.fail("accepted batches pass alone but not together: " + "; ".join(problems))
        pending = list(deferred["rejected"])
        while pending:
            reported = []
            for group in batches.pack_rejected(pending):
                found, result = self.run_rejected_group(group)
                self.assertEqual(result.stderr, "", result.stderr)
                reported += found
            if not reported:
                break
            pending = batches.unreported_batches(pending, reported)
        for batch in pending:
            with self.subTest(deferred_from=batch.test, batch=batch.sequence):
                alone = self.rejected_problems(batch)
                self.assertEqual(alone, [], alone)
                self.fail("rejected rows reported alone but not in the shared runs: "
                          f"{[row.source for row in batch.rows]}")

    def check_import(self, relative, source, rejected):
        path = self.app / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source + "\nfn probe() { let _: Option<Probe> = None; }\n")
        try:
            result = self.run_guard()
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        finally:
            path.unlink()

    @contextmanager
    def fixture_files(self, base, files):
        """Restore existing scan roots and remove newly written files after a run."""
        originals = {}
        try:
            for relative, source in files.items():
                path = base / relative
                originals[path] = path.read_bytes() if path.exists() else None
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            yield
        finally:
            for path, original in originals.items():
                if original is None:
                    path.unlink(missing_ok=True)
                else:
                    path.write_bytes(original)

    def check_files(self, files, rejected, base=None):
        base = base or self.app
        if not rejected and self.defer_batch("accepted", {
                str(self.relative_path(base, relative)): source
                for relative, source in files.items()}):
            return
        with self.fixture_files(base, files):
            result = self.run_guard()
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
            if not rejected:
                for relative in files:
                    self.assertNotIn(f"{base / relative}:", result.stdout)

    def check_rejected_exact_rows(self, base, rows, shared=None, rg_single_file=False):
        """Assert a diagnostic for each exact-path snippet at its own line.

        Rows give the path, snippet, and its diagnostic line (one-based).
        Batch independent rules or snippets that reach the same branch of a
        short-circuit chain. An explicit-file ripgrep scan omits the filename.
        """
        files = dict(shared or {})
        next_line = {}
        diagnostics = []
        for relative, source, line in rows:
            self.assertNotIn(relative, shared or {})
            start = next_line.get(relative, 1)
            self.assertTrue(source.endswith("\n"), source)
            files[relative] = files.get(relative, "") + source
            next_line[relative] = start + source.count("\n")
            diagnostics.append((relative, start + line - 1, source.splitlines()[line - 1]))
        if rg_single_file:
            self.assertEqual(len(next_line), 1)
        with self.fixture_files(base, files):
            result = self.run_guard()
            self.assertEqual(result.stderr, "", result.stderr)
            self.assertEqual(result.returncode, 1, result.stdout)
            output = set(result.stdout.splitlines())
            for relative, line, source in diagnostics:
                with self.subTest(path=relative, line=line, source=source):
                    location = f"{line}" if rg_single_file else f"{base / relative}:{line}"
                    self.assertIn(f"{location}:{source}", output, result.stdout)

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
        deferred = DEFERRED is not None
        # A deferred row's name carries its case, so rows from different cases
        # never write the same file and can share one guard run.
        tag = self.deferred_tag() if deferred else ""
        paths = {}
        for index, (relative, source) in enumerate(rows):
            path = Path(os.path.normpath(base / relative))
            if path.name != "mod.rs":
                path = path.with_name(f"{path.stem}_row{tag}{index}.rs")
            self.assertNotIn(path, paths, relative)
            paths[path] = (relative, source)
        written = [Path(os.path.normpath(base / relative)) for relative in shared or {}]
        if deferred:
            shared_files = {str(path.relative_to(self.root)): source
                            for path, source in zip(written, (shared or {}).values())}
            if rejected:
                self.defer_batch("rejected", tuple(
                    batches.Row(str(path.relative_to(self.root)), source, path.name == "mod.rs")
                    for path, (_, source) in paths.items()), shared_files)
            else:
                self.defer_batch("accepted", {
                    **shared_files,
                    **{str(path.relative_to(self.root)): source
                       for path, (_, source) in paths.items()}})
            return
        try:
            for path, source in zip(written, (shared or {}).values()):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            for path, (_, source) in paths.items():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
            result = self.run_guard()
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
                  "admission", "backlog_forecast", "sweep_filing", "workspace_auto",
                  "search", "notice", "evolution")
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

    def test_task_reaches_notice_only_through_its_port(self):
        # [ORB-23017] Task called notice's done-transition materializer
        # directly, closing a task <-> notice cycle: notice reads tasks for
        # poster authority and linked-task status. One detector per call: the
        # guard's chains stop at the first hit.
        for rows in (
            [("task/resolves.rs", "use crate::application::notice::Probe;\n")],
            [("task/probe.rs", "use crate::application::{\n    notice::Probe,\n};\n")],
            [("task/mod.rs", "use super::notice::Probe;\n"),
             ("task/resolves.rs", "use super::super::notice::Probe;\n")],
        ):
            self.check_rows(rows, rejected=True)
        self.check_rows([
            ("task/resolves.rs",
             "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
             "        self.materialize_notice_closures_for_done_task();\n    }\n}\n"),
        ], rejected=True, shared={
            "notice/close.rs":
                "impl OrbitRuntime {\n"
                "    pub(crate) fn materialize_notice_closures_for_done_task(&self) {}\n}\n"})
        self.check_files({
            "task/notice_port.rs":
                "pub(in crate::application) trait TaskNoticePort {\n"
                "    fn close_linked_notices_for_done_transition(&self);\n}\n",
            "task/resolves.rs":
                "use crate::application::task::TaskNoticePort;\n"
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.close_linked_notices_for_done_transition();\n    }\n}\n",
            "notice/close.rs":
                "impl OrbitRuntime {\n"
                "    pub(crate) fn materialize_notice_closures_for_done_task(&self) {}\n}\n"
                "impl TaskNoticePort for OrbitRuntime {\n"
                "    fn close_linked_notices_for_done_transition(&self) {\n"
                "        self.materialize_notice_closures_for_done_task();\n    }\n}\n",
            "notice/authority.rs": "use crate::application::task::TaskRecords;\n",
        }, rejected=False)

    def test_task_reaches_agent_corpus_only_through_its_port(self):
        # [ORB-23421] Task called the corpus's decision-append, digest and
        # recovery-append inherent methods directly, closing a task <->
        # agent_corpus cycle: the corpus reads decision rules, task history
        # and tasks back. One detector per call: the guard's chains stop at
        # the first hit.
        for rows in (
            [("task/decision/answering.rs", "use crate::application::agent_corpus::Probe;\n")],
            [("task/probe.rs", "use crate::application::{\n    agent_corpus::Probe,\n};\n")],
            [("task/mod.rs", "use super::agent_corpus::Probe;\n"),
             ("task/decision/probe.rs", "use super::super::super::{agent_corpus::Probe};\n")],
        ):
            self.check_rows(rows, rejected=True)
        self.check_rows([
            ("task/decision/answering.rs",
             "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
             "        self.fresh_corpus_method();\n    }\n}\n"),
        ], rejected=True, shared={
            "agent_corpus.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_corpus_method(&self) {}\n}\n"})
        self.check_files({
            "task/corpus_port.rs":
                "pub(in crate::application) trait TaskCorpusPort {\n"
                "    fn task_corpus_append_for_decision(&self);\n}\n",
            "task/decision/answering.rs":
                "use crate::application::task::TaskCorpusPort;\n"
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.task_corpus_append_for_decision();\n    }\n}\n",
            "agent_corpus.rs":
                "impl OrbitRuntime {\n    pub(crate) fn decision_corpus_append(&self) {}\n}\n"
                "impl TaskCorpusPort for OrbitRuntime {\n"
                "    fn task_corpus_append_for_decision(&self) {\n"
                "        self.decision_corpus_append();\n    }\n}\n",
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

    def test_task_cannot_name_the_forecast(self):
        shim = ("pub use crate::application::backlog_forecast::BacklogForecastHistory;\n"
                "pub use crate::application::backlog_forecast::COMPUTE_BUDGET_ENV;\n")
        # One batch per detector in the task chain (direct paths, grouped
        # trees, super chains): the guard stops at the first detector with a
        # hit, so a mixed batch could hide a row behind an earlier detector.
        # The retired task/mod.rs re-exports are no longer an exception.
        self.check_files({"task/mod.rs": shim}, rejected=True)
        self.check_rows([
            ("task/mod.rs", "pub use crate::application::backlog_forecast::Options;\n"),
            ("task/worker.rs", shim),
        ], rejected=True)
        self.check_rows([
            ("task/mod.rs", "use crate::application::{backlog_forecast::BacklogForecastHistory};\n"),
        ], rejected=True)
        self.check_rows([
            ("task/mod.rs", "pub use super::backlog_forecast::BacklogForecastHistory;\n"),
        ], rejected=True)

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
        # One batch per detector, as in the forecast shim rows above.
        self.check_rows([
            ("distributed/serve/admission.rs", "use crate::application::experiment::Probe;\n"),
        ], rejected=True)
        self.check_rows([
            ("distributed/serve/admission.rs", "use crate::application::{experiment::Probe};\n"),
        ], rejected=True)
        self.check_rows([
            ("distributed/worker.rs", "use super::super::experiment::Probe;\n"),
            ("distributed/worker.rs", "use super::super::{experiment::Probe};\n"),
            ("distributed/serve/admission.rs", "use super::super::super::{experiment::Probe};\n"),
        ], rejected=True)
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

    def test_automation_reaches_auto_tasks_only_through_its_port(self):
        # [ORB-23334] Automation read the catalog, accepted history, dedupe and
        # template/mint parameters straight from auto_tasks, whose scheduler
        # evaluates delivery and conditions through automation: a cycle.
        owner = {
            "auto_tasks/scheduler.rs":
                "impl OrbitRuntime {\n"
                "    pub fn fresh_listing_method(&self) {}\n}\n"
                "pub(crate) fn fresh_mint_params(runtime: &OrbitRuntime) {}\n",
        }
        self.check_rows(
            [("automation/condition/facts.rs",
              "impl OrbitRuntime {\n    pub(crate) fn read_facts(&self) {\n"
              f"        {call};\n    }}\n}}\n") for call in (
                "self.fresh_listing_method()",
                "OrbitRuntime::fresh_listing_method(self)",
                "<crate::OrbitRuntime>::fresh_listing_method(self)",
                "Self::fresh_listing_method(self)",
            )],
            rejected=True, shared=owner)
        # One batch per detector, as in the forecast shim rows above.
        self.check_rows([
            ("automation/task.rs",
             "fn mint(runtime: &OrbitRuntime) {\n"
             "    crate::application::auto_tasks::scheduler::fresh_mint_params(runtime);\n}\n"),
            ("automation/task.rs", "use crate::application::auto_tasks::Probe;\n"),
        ], rejected=True, shared=owner)
        self.check_rows([
            ("automation/task.rs", "use crate::application::{auto_tasks::Probe};\n"),
            ("automation/task.rs", "use crate::application::{\n    task, auto_tasks::Probe,\n};\n"),
        ], rejected=True, shared=owner)
        self.check_rows([
            ("automation/task.rs", "use super::super::auto_tasks::Probe;\n"),
            ("automation/condition/host.rs", "use super::super::super::{auto_tasks::Probe};\n"),
            ("automation/condition/host.rs",
             "fn mint(runtime: &OrbitRuntime) {\n"
             "    super::super::super::auto_tasks::scheduler::fresh_mint_params(runtime);\n}\n"),
        ], rejected=True, shared=owner)
        # A distinct port method name passes, and auto_tasks keeps its
        # downward edges into automation.
        self.check_files({
            **owner,
            "automation/auto_task_port.rs":
                "pub(in crate::application) trait AutomationAutoTaskPort {\n"
                "    fn listed_auto_task_definitions(&self);\n}\n",
            "auto_tasks/port.rs":
                "use crate::application::automation::AutomationAutoTaskPort;\n"
                "impl AutomationAutoTaskPort for OrbitRuntime {\n"
                "    fn listed_auto_task_definitions(&self) {\n"
                "        self.fresh_listing_method();\n    }\n}\n",
            "automation/inspect.rs":
                "use crate::application::automation::AutomationAutoTaskPort;\n"
                "fn inspect(runtime: &OrbitRuntime) {\n"
                "    runtime.listed_auto_task_definitions();\n}\n",
            "auto_tasks/projection.rs":
                "fn project(runtime: &OrbitRuntime) {\n"
                "    crate::application::automation::evaluate_auto_task(runtime);\n}\n",
        }, rejected=False)

    def test_tag_instructions_reaches_experiment_only_through_its_port(self):
        # [ORB-23019] tag_instructions.rs called experiment's overlay query,
        # and experiment reads tag-instruction helpers back. The module is one
        # file, so the caller scan names tag_instructions.rs. The port method
        # stays distinct from the inherent method the guard indexes.
        method = {
            "experiment/overlay.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_overlay_method(&self) {}\n}\n",
        }
        self.check_rejected_exact_rows(self.app, [
            ("tag_instructions.rs",
             "impl OrbitRuntime {\n    fn resolve(&self) {\n"
             f"        {call};\n    }}\n}}\n", 3)
            for call in (
                "self.fresh_overlay_method()",
                "OrbitRuntime::fresh_overlay_method(self)",
                "Self::fresh_overlay_method(self)",
                "<crate::OrbitRuntime>::fresh_overlay_method(self)",
            )
        ], shared=method)
        self.check_rejected_exact_rows(self.app, [
            ("tag_instructions.rs", "use crate::application::experiment::Probe;\n", 1),
        ], rg_single_file=True)
        self.check_rejected_exact_rows(self.app, [
            ("tag_instructions.rs", source, 1) for source in (
                "use crate::application::{experiment::Probe};\n",
                "use crate::application::{\n    experiment::Probe,\n};\n",
            )
        ], rg_single_file=True)
        self.check_rejected_exact_rows(self.app, [
            ("tag_instructions.rs", source, 1) for source in (
                "use super::experiment::Probe;\n",
                "use super::{experiment::Probe};\n",
                "use super::super::application::experiment::Probe;\n",
            )
        ])
        self.check_files({
            "experiment/overlay.rs":
                "use crate::application::tag_instructions::LazyRevision;\n"
                "impl OrbitRuntime {\n"
                "    pub(crate) fn experiment_overlays_for_activity(&self) {}\n}\n",
            "tag_instructions.rs":
                "pub(in crate::application) trait TagInstructionExperimentPort {\n"
                "    fn tag_instruction_experiment_overlays(&self);\n}\n"
                "impl OrbitRuntime {\n    fn resolve(&self) {\n"
                "        self.tag_instruction_experiment_overlays();\n    }\n}\n",
            "experiment/tag_instruction_port.rs":
                "impl TagInstructionExperimentPort for OrbitRuntime {\n"
                "    fn tag_instruction_experiment_overlays(&self) {\n"
                "        self.experiment_overlays_for_activity();\n    }\n}\n",
            "experiment/skills.rs":
                "use crate::application::tag_instructions::LazyRevision;\n",
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
        # One batch per detector across both modules' chains, as in the
        # forecast shim rows above.
        self.check_rows([
            (f"{module}/claimed.rs", "use crate::application::experiment::Probe;\n")
            for module in ("task", "job")
        ], rejected=True)
        self.check_rows([
            (f"{module}/claimed.rs", "use crate::application::{experiment::Probe};\n")
            for module in ("task", "job")
        ], rejected=True)
        self.check_rows([
            row for module in ("task", "job") for row in (
                (f"{module}/claimed.rs", "use super::super::experiment::Probe;\n"),
                (f"{module}/crew_pools/claims.rs",
                 "use super::super::super::{experiment::Probe};\n"),
            )
        ], rejected=True)
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

    def test_run_cannot_reach_pipeline(self):
        # [ORB-24781] `job::run` sat on top of `job::pipeline` for its audit
        # writer, worker-exit evidence and definition reads, closing a direct
        # two-module cycle; run is the lower module. One batch per detector:
        # the guard's chains stop at the first hit.
        method = {
            "job/pipeline/state.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_pipeline_method(&self) {}\n}\n",
        }
        self.check_rows([
            ("job/run/audit.rs", "use crate::application::job::pipeline::Probe;\n"),
            ("job/run/delivery/observe.rs", "use super::super::pipeline::Probe;\n"),
        ], rejected=True)
        self.check_rows([
            ("job/run/audit.rs", "use crate::application::job::{pipeline::Probe, run::Other};\n"),
            ("job/run/delivery/observe.rs", "use super::super::{pipeline::Probe};\n"),
        ], rejected=True)
        # A nested group that closes before `pipeline` must not hide it.
        self.check_rows([
            ("job/run/audit.rs",
             "use crate::application::job::{catalog::{A, B}, pipeline::Probe};\n"),
            ("job/run/delivery/observe.rs",
             "use super::super::{catalog::{A, B}, pipeline::Probe};\n"),
        ], rejected=True)
        self.check_rows([
            ("job/run/audit.rs", "use crate::application::{job::pipeline::Probe};\n"),
            ("job/run/reconcile.rs", "use crate::application::{job::{pipeline::Probe}};\n"),
        ], rejected=True)
        self.check_rows([
            ("job/run/audit.rs",
             "use crate::application::{job::{catalog::{A, B}, pipeline::Probe}};\n"),
        ], rejected=True)
        self.check_rows([
            ("job/run/reconcile.rs",
             "impl OrbitRuntime {\n    fn settle(&self) {\n"
             "        self.fresh_pipeline_method();\n    }\n}\n"),
        ], rejected=True, shared=method)
        self.check_files({
            "job/run/audit_record.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_run_method(&self) {}\n}\n",
            "job/run/reconcile.rs":
                "impl OrbitRuntime {\n    fn settle(&self) {\n"
                "        self.fresh_run_method();\n    }\n}\n",
            "job/pipeline/worker.rs":
                "use crate::application::job::run::audit_record;\n"
                "impl OrbitRuntime {\n    fn drive(&self) {\n"
                "        self.fresh_run_method();\n    }\n}\n",
            "job/run/tests/reconcile.rs":
                "use crate::application::job::pipeline::Probe;\n",
            "job/run/definition.rs":
                "/// Written by the pipeline's submission.\nfn pipeline_note() {}\n",
        }, rejected=False)

    def test_lower_modules_reach_landing_only_through_owned_port(self):
        # [ORB-22708] Task and job resolved claims through landing, and review
        # dispatched it directly; the missing guard admitted these cycles.
        method = {
            "landing/handoff.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_landing_method(&self) {}\n}\n",
        }
        # One batch per detector across the four modules' landing chains,
        # as in the forecast shim rows above.
        modules = ("task", "job", "review", "distributed")
        self.check_rows([
            (f"{module}/probe.rs",
             "impl OrbitRuntime {\n    fn probe(&self) {\n"
             "        self.fresh_landing_method();\n    }\n}\n")
            for module in modules
        ], rejected=True, shared=method)
        self.check_rows([
            row for module in modules for row in (
                (f"{module}/probe.rs", "use crate::application::landing::Probe;\n"),
                (f"{module}/deep/probe.rs",
                 "fn probe() { crate::application::landing::step(); }\n"),
            )
        ], rejected=True)
        self.check_rows([
            (f"{module}/probe.rs",
             "use crate::application::{\n    landing::Probe,\n};\n")
            for module in modules
        ], rejected=True)
        self.check_rows([
            row for module in modules for row in (
                (f"{module}/mod.rs", "use super::landing::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::landing::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::{landing::Probe};\n"),
                (f"{module}/deep/probe.rs", "use super::super::super::{landing::Probe};\n"),
            )
        ], rejected=True)
        self.check_files({
            **method,
            "task/handoff_claim.rs":
                "impl OrbitRuntime {\n    pub(crate) fn landing_claim(&self) {}\n}\n",
            "job/pr_repair/admission.rs":
                "impl OrbitRuntime {\n    fn admit(&self) { self.landing_claim(); }\n}\n",
            "review/landing_dispatch_port.rs":
                "pub(in crate::application) trait ReviewLandingDispatchPort {\n"
                "    fn dispatch_recorded_landing_authority(&self);\n}\n",
            "review/handoff/accept.rs":
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
            "review/handoff/console.rs":
                "fn probe() {\n    self.stores().tasks().landing_attempts()?\n}\n",
        }, rejected=False)
        self.check_rejected_exact_rows(self.app, [
            (relative, "impl OrbitRuntime {\n    fn probe(&self) {\n"
                       "        self.landing_attempts();\n    }\n}\n", 3)
            for relative in ("task/external_merge/candidate.rs",
                             "task/external_merge/rescued.rs",
                             "review/handoff/console.rs")
        ], shared=method)

    def test_automation_and_auto_tasks_cannot_reach_routines(self):
        # [ORB-22623] `routines` sits above both; automation's clock-settings
        # read and auto_tasks' display state were back-edges into it.
        method = {
            "routines/status/report.rs":
                "impl OrbitRuntime {\n    pub(crate) fn fresh_routine_method(&self) {}\n}\n",
        }
        # One batch per detector across both modules' routines chains, as
        # in the forecast shim rows above.
        modules = ("automation", "auto_tasks")
        self.check_rows([
            (f"{module}/probe.rs",
             "impl OrbitRuntime {\n    fn probe(&self) {\n"
             "        self.fresh_routine_method();\n    }\n}\n")
            for module in modules
        ], rejected=True, shared=method)
        self.check_rows([
            row for module in modules for row in (
                (f"{module}/probe.rs", "use crate::application::routines::Probe;\n"),
                (f"{module}/deep/probe.rs",
                 "fn probe(root: &Path) { crate::application::routines::clock::load(root); }\n"),
            )
        ], rejected=True)
        self.check_rows([
            (f"{module}/probe.rs", "use crate::application::{\n    routines::Probe,\n};\n")
            for module in modules
        ], rejected=True)
        self.check_rows([
            row for module in modules for row in (
                (f"{module}/mod.rs", "use super::routines::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::routines::Probe;\n"),
                (f"{module}/probe.rs", "use super::super::{routines::Probe};\n"),
                (f"{module}/deep/probe.rs", "use super::super::super::{routines::Probe};\n"),
            )
        ], rejected=True)
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
        targets = ("routines::clock::set_clock_cadence", "auto_tasks::AutoTaskListingRow")
        # Separate runs exercise direct, grouped and relative-path detectors.
        self.check_rows([
            ("contracts/clock_settings.rs", f"use crate::application::{target};\n")
            for target in targets
        ], rejected=True)
        self.check_rows([
            ("contracts/clock_settings.rs", f"use crate::application::{{{target}}};\n")
            for target in targets
        ], rejected=True)
        self.check_rows([
            row for target in targets for row in (
                ("contracts/clock_settings.rs", f"use super::super::{{{target}}};\n"),
                ("contracts/clock_settings/nested.rs",
                 f"use super::{{super::{{super::{{\n    {target},\n}}}}}};\n"),
            )
        ], rejected=True)
        self.check_rejected_exact_rows(self.app, [
            ("contracts/mod.rs", source, 1)
            for target in targets
            for source in (
                f"use super::{target};\n",
                f"use super::{{{target}}};\n",
                f"use super::{{\n    {target},\n}};\n",
            )
        ])
        self.check_files({
            "contracts/mod.rs": "pub use self::clock_settings::ClockSettings;\n",
            "contracts/clock_settings.rs": "use super::{ScheduleDisplayState};\n",
            "contracts/clock_settings/nested.rs":
                "use super::super::{ScheduleDisplayState};\n",
        }, rejected=False)
        # [ORB-22674] A chain that climbs one step past application, to the crate
        # root, and then descends `application::<higher>` names the same module
        # as a chain that lands on application, so it is refused at each depth.
        # The mod.rs rows keep their exact path for the module-depth check.
        self.check_rows([
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
        ], rejected=True)
        self.check_rejected_exact_rows(self.app, [
            ("contracts/mod.rs", source, 1) for source in (
                "use super::super::application::routines::Probe;\n",
                "use super::super::{application::routines::Probe};\n",
                "use super::super::{\n    application::routines::Probe,\n};\n",
            )
        ])

    def test_moved_clock_and_display_contracts_stay_in_contracts(self):
        # [ORB-22623] A second definition would bring the back-edge's owner back.
        # Every row hits the same definitions scan over orbit-core/src, so
        # they share one guard run (check_rows asserts each row's own path).
        self.check_rows([
            ("routines/clock/settings.rs", source) for source in (
                "pub struct ClockSettings;\n",
                "pub fn load_clock_settings() {}\n",
                "pub enum ScheduleDisplayState {}\n",
            )
        ], rejected=True)
        self.check_files({
            "routines/clock/settings.rs":
                "pub use crate::application::contracts::{ClockSettings, load_clock_settings};\n",
            "routines/status/mod.rs":
                "pub use crate::application::contracts::ScheduleDisplayState;\n",
        }, rejected=False)

    def test_agents_cannot_reach_experiment(self):
        # One batch per detector, as in the forecast shim rows above.
        self.check_rows([
            ("agents/facts/scan.rs", "use crate::application::experiment::Probe;\n"),
        ], rejected=True)
        self.check_rows([
            ("agents/facts/scan.rs", "use crate::application::{experiment::Probe};\n"),
        ], rejected=True)
        self.check_rows([
            ("agents/facts/scan.rs", "use super::super::super::experiment::Probe;\n"),
            ("agents/facts/scan.rs", "use super::super::super::{experiment::Probe};\n"),
            ("agents/mod.rs", "use super::experiment::Probe;\n"),
        ], rejected=True)
        self.check_files({
            "experiment/readout.rs": "impl OrbitRuntime {\n    pub(crate) fn experiment_probe(&self) {}\n}\n",
            "agents/facts/scan.rs": "fn scan(runtime: &OrbitRuntime) { runtime.experiment_probe(); }\n",
        }, rejected=True)
        self.check_files({
            "experiment/readout.rs": "use crate::application::agents::Probe;\n",
            "codebase/diff_stats.rs": "impl OrbitRuntime {\n    pub(crate) fn landed_task_lines_changed(&self) {}\n}\n",
            "agents/facts/scan.rs": "fn scan(runtime: &OrbitRuntime) { runtime.landed_task_lines_changed(); }\n",
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
        self.check_rows([
            (caller, f"fn drive(runtime: &OrbitRuntime) {{\n    {call};\n}}\n")
            for caller in ("task/worker.rs", "task_quality/runtime.rs")
            for call in (
                "fresh_owner_method(runtime)",
                "<Other>::fresh_owner_method(runtime)",
                "Vec::<OrbitRuntime>::fresh_owner_method(runtime)",
            )
        ], rejected=False, shared={
            owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_owner_method(&self) {}\n}\n"
            for owner in ("job/state.rs", "../adapter/state.rs")
        })

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
        # One guard run per owner, with both callers present: check_rows
        # requires the guard to report each caller's own path.
        for owner in ("../composition/runtime.rs", "../bootstrap/init.rs"):
            with self.subTest(owner=owner):
                self.check_rows([
                    (caller, "impl Probe {\n    fn drive(&self, runtime: &OrbitRuntime) {\n"
                             "        runtime.fresh_joiner();\n    }\n}\n")
                    for caller in ("task_quality/runtime.rs",
                                   "../adapter/engine_host/dispatch.rs")
                ], rejected=True, shared={
                    owner: "impl OrbitRuntime {\n    pub(crate) fn fresh_joiner(&self) {}\n}\n"})

    def test_joiner_ports_and_their_mentions_accepted(self):
        # Both callers present in one run: acceptance must hold for each
        # beside the other, which is at least as strong as one at a time.
        caller_source = ("// crate::bootstrap::init is only named here.\n"
                         'const NOTE: &str = "crate::composition::x";\n'
                         "fn drive(runtime: &OrbitRuntime) {\n"
                         "    runtime.ported_service();\n    runtime.private_joiner();\n"
                         "    runtime.narrow_joiner();\n}\n")
        self.check_files({
            "../composition/runtime.rs":
                "impl OrbitRuntime {\n    fn private_joiner(&self) {}\n"
                "    pub(in crate::bootstrap) fn narrow_joiner(&self) {}\n}\n"
                "impl AssistanceServicePort for OrbitRuntime {\n"
                "    fn ported_service(&self) {}\n}\n",
            "task_quality/runtime.rs": caller_source,
            "../adapter/engine_host/dispatch.rs": caller_source,
            "../adapter/tests/fixture.rs": "use crate::bootstrap::init::Probe;\n",
            "task/tests/fixture.rs": "use crate::composition::Probe;\n",
        }, rejected=False)

    def test_task_port_and_task_owned_calls_accepted(self):
        self.check_files({
            "task/mod.rs": "pub(crate) trait TaskAutomationPort {\n    fn schedule_probe(&self);\n}\n"
                           "pub(in crate::application) trait TaskEvolutionPort {\n"
                           "    fn project_task_checkpoint_costs(&self);\n}\n",
            "task/worker.rs":
                "impl OrbitRuntime {\n    pub(crate) fn drive_task(&self) {\n"
                "        self.schedule_probe();\n        self.task_local_probe();\n"
                "        self.project_task_checkpoint_costs();\n    }\n}\n",
            "task/state.rs": "impl OrbitRuntime {\n    pub(crate) fn task_local_probe(&self) {}\n}\n",
            "automation/port.rs":
                "impl TaskAutomationPort for OrbitRuntime {\n    fn schedule_probe(&self) {}\n}\n",
            "evolution/operations.rs":
                "impl OrbitRuntime {\n    pub(crate) fn ingest_checkpoint_costs(&self) {}\n}\n"
                "impl TaskEvolutionPort for OrbitRuntime {\n"
                "    fn project_task_checkpoint_costs(&self) {\n"
                "        self.ingest_checkpoint_costs();\n    }\n}\n",
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
        # One batch per detector, as in the forecast shim rows above.
        self.check_rows([
            ("job/worker.rs", f"use crate::application::{module}::Probe;\n")
            for module in ("distributed", "pull_drain")
        ], rejected=True)
        self.check_rows([
            ("job/worker.rs", f"use super::super::{module}::Probe;\n")
            for module in ("distributed", "pull_drain")
        ], rejected=True)

    def test_commented_grouped_imports_rejected(self):
        for owner, modules in (
            ("job", ("distributed", "pull_drain", "review")),
            ("task", ("job", "review", "distributed", "sweep_filing", "workspace_auto",
                      "evolution")),
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
        # Accepted rows share one run: any false hit on any row fails it.
        self.check_rows([
            (relative, f"use {tree};\n") for relative, tree in (
                ("job/pr_repair/adoption.rs", "super::super::{RunOwnerLiveness, run_owner_liveness}"),
                ("job/worker.rs", "super::{distributed::Probe, pull_drain::Other}"),
                ("job/pr_repair/adoption.rs", "super::super::{distributed::Probe}"),
            )
        ], rejected=False)

    def test_test_fixtures_accepted(self):
        self.check_files({
            relative: "use super::super::{distributed::Probe, pull_drain::Other};\n"
                      "fn probe() { let _: Option<Probe> = None; }\n"
            for relative in ("job/tests/worker.rs", "job/tests.rs")
        }, rejected=False)

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
        self.check_files(
            {snapshot: "use crate::crew_defaults::builtin_default_crews;\n"},
            rejected=False, base=self.root / "crates/orbit-config/src")

    def test_config_crew_defaults_leaf_reaches_nothing_higher(self):
        base = self.root / "crates/orbit-config/src"
        leaf = "crew_defaults.rs"
        # The path scan and the use-allowlist scan both run on this file.
        self.check_rejected_exact_rows(base, [
            (leaf, source + "\n", 1) for source in (
                "use crate::resolved::Probe;",
                "use super::registry::Probe;",
                "use crate::raw::Probe;",
                "use toml::Value;",
                # [ORB-22452] Inline paths to crates outside std, orbit-common
                # and orbit-types count as the same edge as a use.
                "fn probe() -> toml::Value { toml::Value::Boolean(true) }",
                "fn probe() { let _ = serde_json::Value::Null; }",
                'fn probe() { let _ = serde::de::value::Error::custom(""); }',
            )
        ], rg_single_file=True)
        self.check_files({leaf: (
            "use std::collections::BTreeMap;\nuse orbit_common::model_defaults::{\n    A, B,\n};\n"
            "use orbit_types::identity::{Crew, CrewAssignment};\n")}, rejected=False, base=base)

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
        self.check_rejected_exact_rows(src, [
            ("aliases.rs", "use crate::registry::read_optional;\n", 1),
            ("seeding.rs", "use crate::{describe_config_key, raw::Probe};\n", 1),
        ])
        self.check_rows([(relative, source + "\n") for relative, source in (
            ("crew/ladder.rs", "use crate::config_document;"),
            ("operation/policy.rs", "use super::crew::ReviewCrewPool;"),
            ("provider_limit/policy.rs", "use crate::CrewPerformancePolicy;"),
        )], rejected=False, base=src)
        self.check_files({"aliases.rs": "use crate::config_document;\n"},
                         rejected=False, base=src)

    def test_config_document_leaf_admits_only_external_crates(self):
        base = self.root / "crates/orbit-config/src"
        leaf = "config_document.rs"
        self.check_rejected_exact_rows(base, [
            (leaf, source + "\n", 1) for source in (
                "use crate::registry::Probe;",
                "use super::raw::Probe;",
                "fn probe() { crate::config_io::read(); }",
                "use serde_json::Value;",
            )
        ], rg_single_file=True)
        self.check_files({leaf: (
            "use std::path::Path;\nuse orbit_common::OrbitError;\n"
            "use orbit_common::security::redaction::redact_home_dir;\n"
            "use serde::de::DeserializeOwned;\n")}, rejected=False, base=base)

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
        result = self.run_guard()
        self.assertEqual(result.stderr, "", result.stderr)
        self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)

    def check_surface_dependency(self, crate, dependency, kind, target, rejected):
        """Run the guard over a synthetic Cargo metadata edge for a surface crate.

        Builds `cargo metadata --no-deps`-shaped packages for `crate`, the
        `dependency` crate and the `orbit-core` fixture the guard also scans,
        then runs the real public script against them.
        """
        names = {crate, dependency, "orbit-core"}
        packages = []
        for name in sorted(names):
            manifest = self.root / f"crates/{name}/Cargo.toml"
            manifest.parent.mkdir(parents=True, exist_ok=True)
            if not manifest.exists():
                manifest.write_text(f'[package]\nname = "{name}"\n')
            dependencies = (
                [{"name": dependency, "kind": kind, "target": target}]
                if name == crate else []
            )
            packages.append({"id": name, "name": name,
                             "manifest_path": str(manifest),
                             "dependencies": dependencies})
        metadata = self.root / "metadata.json"
        metadata.write_text(json.dumps({
            "workspace_members": sorted(names), "packages": packages}))
        result = self.run_guard()
        self.assertEqual(result.stderr, "", result.stderr)
        self.assertEqual(result.returncode, 1 if rejected else 0, result.stdout)
        if rejected:
            self.assertIn(
                f"forbidden dependency '{dependency}'", result.stdout)
        else:
            self.assertIn("dependency direction guard passed", result.stdout)

    def test_cli_auth_edge_is_refused(self):
        # Cargo and ARCHITECTURE.md give orbit-cli no orbit-auth edge, in
        # normal or dev form; credential code reaches the assembled binary
        # only through orbit-web, so the guard refuses a direct edge.
        for kind, target in ((None, None), ("dev", None), (None, "cfg(unix)")):
            with self.subTest(kind=kind, target=target):
                self.check_surface_dependency(
                    "orbit-cli", "orbit-auth", kind, target, rejected=True)

    def test_web_auth_edge_remains_legal(self):
        # orbit-web's lease listener wraps orbit-auth's token verification,
        # client-address rule and limiter, so its normal edge stays allowed.
        self.check_surface_dependency(
            "orbit-web", "orbit-auth", None, None, rejected=False)

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

    def test_orbit_web_lower_modules_do_not_name_api_or_serve(self):
        # [ORB-25492] state, projections, the leaves and their siblings sit
        # below api and serve. lease may still name api::leases. A comment or
        # a string is not an edge. A missing leaf fails closed.
        web = self.root / "crates/orbit-web/src"
        self.check_rows([
            ("state/up.rs", source + "\n") for source in (
                "use crate::api::host_error_code;",
                "use crate::{state::DashboardState, api::host_error_code};",
                "fn probe() { crate::serve::build_state(); }",
            )
        ] + [
            ("lease/up.rs", "use crate::serve::shutdown_signal;\n"),
            ("lease/up.rs", "fn probe() { crate::serve::build_app(); }\n"),
        ], rejected=True, base=web)
        self.check_rows([
            ("state/down.rs", "use crate::runtime_memo::RoutineMemoKey;\n"),
            ("state/down.rs", "// crate::api::host_error_code stays a comment\n"),
            ("state/down.rs", 'const NOTE: &str = "crate::serve::build_state";\n'),
            ("lease/down.rs", "use crate::api::leases::{LeaseBackend, lease_router};\n"),
            ("lease/down.rs", "use crate::shutdown::shutdown_signal;\n"),
        ], rejected=False, base=web)
        self.check_rejected_exact_rows(web, [
            ("why_stuck.rs", "use crate::api::failure_root_line;\n", 1),
            ("projections.rs", "use crate::api::tasks::reads;\n", 1),
            ("connect.rs", "use crate::serve::open_browser;\n", 1),
            ("host_tunnels.rs", "use crate::{serve::DEFAULT_DASHBOARD_PORT};\n", 1),
            ("health.rs", "fn probe() { crate::api::router(); }\n", 1),
            ("assets.rs", "use crate::serve::build_app;\n", 1),
            ("run_failure.rs", "use crate::api::runs::detail;\n", 1),
            ("shutdown.rs", "use crate::serve::run_server;\n", 1),
            ("runtime_memo.rs", "use crate::api::routines::RoutineMemoKey;\n", 1),
            ("runtime_memo.rs", "use crate::serve::shutdown_signal;\n", 1),
        ])
        self.check_files({
            "why_stuck.rs": "use crate::run_failure::failure_root_line;\n"
                            "// use crate::api::failure_root_line;\n"
                            'const NOTE: &str = "crate::serve::shutdown_signal";\n',
            "connect.rs": "pub(crate) const DEFAULT_DASHBOARD_PORT: u16 = 7878;\n",
            "shutdown.rs": "use orbit_core::OrbitError;\n",
            "run_failure.rs": "use orbit_core::{JobRun, OrbitRuntime};\n",
            "assets.rs": "use crate::state::DashboardState;\n",
            "host_tunnels.rs": "use crate::connect::DEFAULT_DASHBOARD_PORT;\n"
                               "// api::forward is the HTTP side.\n",
            "projections.rs": "use crate::why_stuck::attach_why_stuck;\n",
            "health.rs": "fn probe() {}\n",
            "runtime_memo.rs": "use orbit_core::OrbitError;\n",
        }, rejected=False, base=web)
        for required_leaf in ("runtime_memo.rs", "shutdown.rs"):
            missing = web / required_leaf
            missing.unlink()
            try:
                result = self.run_guard()
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("dependency scan path missing", result.stderr)
                self.assertIn(required_leaf, result.stderr)
                self.assertNotIn("dependency direction guard passed", result.stdout)
            finally:
                missing.write_text("")


# Each case builds its own fixture tree and spends most of its time in the
# guard's Python and ripgrep subprocesses, so independent cases run concurrently.
# Four workers match the hosted 4-vCPU guardrails runner.
CONCURRENT_CASES = 4


def available_cpus():
    if hasattr(os, "sched_getaffinity"):
        return len(os.sched_getaffinity(0))
    return os.cpu_count() or 1


def run_case(case):
    result = unittest.TestResult()
    case(result)
    return result


def run_cases_concurrently():
    """Run every case once, each with its own result, and report like unittest."""
    global DEFERRED
    cases = list(unittest.defaultTestLoader.loadTestsFromTestCase(RelativeImportTests))
    concurrent = [case for case in cases if case._testMethodName != DEFERRED_CASE]
    workers = max(1, min(CONCURRENT_CASES, available_cpus(), len(concurrent)))
    started = time.perf_counter()
    DEFERRED = {"accepted": [], "rejected": []}
    try:
        with ThreadPoolExecutor(max_workers=workers) as pool:
            results = list(pool.map(run_case, concurrent))
        # Every other case has registered its accepted batches by now.
        results += [run_case(case) for case in cases if case._testMethodName == DEFERRED_CASE]
    finally:
        DEFERRED = None
    elapsed = time.perf_counter() - started
    problems = [("ERROR", case, trace) for result in results for case, trace in result.errors]
    problems += [("FAIL", case, trace) for result in results for case, trace in result.failures]
    for kind, case, trace in problems:
        print("=" * 70, file=sys.stderr)
        print(f"{kind}: {case}", file=sys.stderr)
        print("-" * 70, file=sys.stderr)
        print(trace, file=sys.stderr)
    ran = sum(result.testsRun for result in results)
    unexpected = sum(len(result.unexpectedSuccesses) for result in results)
    print("-" * 70, file=sys.stderr)
    print(f"Ran {ran} tests in {elapsed:.3f}s ({workers} concurrent, "
          f"{GUARD_RUNS[0]} guard runs)", file=sys.stderr)
    # A case that ran nothing, or a lost result, is a failure, never a pass.
    if not cases or ran != len(cases) or problems or unexpected:
        print(f"FAILED (cases={len(cases)}, ran={ran}, problems={len(problems)}, "
              f"unexpected successes={unexpected})", file=sys.stderr)
        return 1
    print("OK", file=sys.stderr)
    return 0


if __name__ == "__main__":
    if len(sys.argv) > 1:
        # Named cases or unittest options keep the standard serial runner.
        unittest.main()
    else:
        sys.exit(run_cases_concurrently())
