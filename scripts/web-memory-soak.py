#!/usr/bin/env python3
"""Dashboard memory soak harness (Linux only).

Builds a disposable Orbit store sized like a busy workspace, then measures the
resident memory of `orbit web serve` while it answers the dashboard's polling
mix:

1. Fresh-server growth: a new server answers one `/api/scoreboard` request,
   and another new server answers one `/api/routines` request. Each delta is
   VmRSS after the request minus VmRSS after a warm-up request.
2. Soak: one server answers every endpoint in the dashboard GET mix
   concurrently, once per round, for `--rounds` rounds. Rounds are spaced
   past the 15 s dashboard memo TTL so every round recomputes. VmRSS is
   sampled after each round, and VmHWM gives the peak.
3. CLI cost: median wall time of `orbit task list` on the same fixture.

The harness measures whichever binary `--bin` names, so a before/after
comparison runs it twice against identical fixtures. Exit status is 1 when a
budget is exceeded, unless `--report-only` is given.

Usage:
    make web-memory-soak
    python3 scripts/web-memory-soak.py --bin target/release/orbit \
        [--work-dir DIR] [--report report.json] [--rounds 20] [--report-only]
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import random
import re
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
import urllib.request
from datetime import datetime, timedelta, timezone
from pathlib import Path

# Store shape. ws_orbit held about 4,200 tasks, 25,600 job runs and 390,000
# audit rows when the dashboard ratchet was measured.
TASKS = 4_000
JOB_RUNS = 25_000
AUDIT_ROWS = 300_000
V2_EVENTS_PER_RUN = 12
INVOCATIONS = 5_000
TOOL_CALLS_PER_INVOCATION = 20
FRICTIONS = 750
ROUTINE_FIRES = 11_000
PROCESS_LOG_LINES = 40_000

# Budgets from the task acceptance criteria.
SOAK_DRIFT_BUDGET = 0.15
PEAK_RSS_BUDGET_MB = 512
SINGLE_REQUEST_BUDGET_MB = 20

# Longer than the dashboard's 15 s memo TTLs, so each round recomputes.
DEFAULT_INTERVAL_S = 16.0
WORKSPACE_NAME = "soak"
WORKSPACE_ID = "ws_soak"
TASK_PREFIX = "SK"

# The panels an open dashboard polls (see crates/orbit-web/assets/dashboard).
GET_MIX = [
    "/api/tasks?workspace={ws}",
    "/api/tasks/all",
    "/api/routines",
    "/api/auto-tasks?workspace={ws}",
    "/api/scoreboard?window=24h&workspace={ws}",
    "/api/diagnostics/errors?since=24h&limit=50&workspace={ws}",
    "/api/diagnostics/friction?limit=50&workspace={ws}",
    "/api/frictions?workspace={ws}",
    "/api/audit/summary?since=24h&workspace={ws}",
]

PIPELINES = [
    "task_gate_pipeline",
    "task_pr_pipeline",
    "task_auto_pipeline",
    "auto_task_scheduler_pipeline",
    "task_pilot_pipeline",
    "ci_failure_sweep_pipeline",
    "worktree_gc_pipeline",
    "workspace_auto_pipeline",
]
MODELS = ["claude-opus-5-5", "claude-sonnet-5-5", "gpt-5.5-codex", "claude-haiku-5-5"]
TOOLS = [
    "orbit.task.show",
    "orbit.task.update",
    "orbit.task.list",
    "orbit.search",
    "orbit.friction.add",
    "proc.spawn",
    "orbit.task.artifact.put",
]
LOREM = (
    "The dashboard polls every visible panel on a fixed interval, and each "
    "poll reads task bundles, job runs and audit history from the store. "
)


def now_utc() -> datetime:
    return datetime.now(timezone.utc)


def iso(ts: datetime) -> str:
    return ts.isoformat().replace("+00:00", "Z")


# ── isolated orbit invocations ───────────────────────────────────────────────


def isolated_env(home: Path) -> dict[str, str]:
    """Environment for fixture children: no inherited Orbit or Git authority.

    Mirrors `orbit_common::test_env::INHERITED_AUTHORITY_ENV` by dropping every
    `ORBIT_*` variable, so a harness launched from a managed run cannot route
    its writes into the live store.
    """
    env = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith("ORBIT_") and not key.startswith("GIT_")
    }
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    return env


class Fixture:
    def __init__(self, work: Path, binary: Path):
        self.work = work
        self.binary = binary
        self.home = work / "home"
        self.repo = work / "repo"
        self.root = work / "root"
        self.env = isolated_env(self.home)

    def orbit(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        result = subprocess.run(
            [str(self.binary), "--root", str(self.root), *args],
            cwd=self.repo,
            env=self.env,
            capture_output=True,
            text=True,
        )
        if check and result.returncode != 0:
            raise RuntimeError(
                f"orbit {' '.join(args)} failed ({result.returncode}):\n"
                f"{result.stdout}\n{result.stderr}"
            )
        return result

    def exists(self) -> bool:
        return (self.work / "fixture.json").is_file()

    # ── generation ──────────────────────────────────────────────────────────

    def build(self) -> dict:
        started = time.monotonic()
        rng = random.Random(14723)
        self.home.mkdir(parents=True, exist_ok=True)
        self.repo.mkdir(parents=True, exist_ok=True)
        for args in (
            ["init", "-q", "--initial-branch=main"],
            ["config", "user.email", "soak@example.com"],
            ["config", "user.name", "soak"],
            ["commit", "-q", "--allow-empty", "-m", "fixture"],
        ):
            subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True)
        self.orbit(
            "init",
            "--non-interactive",
            "--machine-name",
            "soak-host",
            "--task-prefix",
            TASK_PREFIX,
        )
        self.orbit("workspace", "init", "--name", WORKSPACE_NAME)
        workspace_id = self.workspace_id()
        self.enable_routines()
        now = now_utc()
        counts = {
            "tasks": self.seed_tasks(workspace_id, rng, now),
            **self.seed_store(workspace_id, rng, now),
            "process_log_lines": self.seed_process_log(rng, now),
        }
        manifest = {
            "workspace_id": workspace_id,
            "generated_at": iso(now),
            "generation_seconds": round(time.monotonic() - started, 1),
            "counts": counts,
        }
        (self.work / "fixture.json").write_text(json.dumps(manifest, indent=2))
        return manifest

    def workspace_id(self) -> str:
        config = (self.root / "config.yaml").read_text()
        match = re.search(r"^workspace_id:\s*(\S+)", config, re.MULTILINE)
        if not match:
            raise RuntimeError(f"no workspace_id in {self.root / 'config.yaml'}")
        return match.group(1)

    def enable_routines(self) -> None:
        # Busy workspaces run their seeded routines, including the
        # state-triggered task pilot that fingerprints open tasks.
        for path in (self.root / "routines").glob("*.yaml"):
            text = path.read_text()
            path.write_text(re.sub(r"^enabled: false", "enabled: true", text, flags=re.M))

    def seed_tasks(self, workspace_id: str, rng: random.Random, now: datetime) -> int:
        description = (LOREM * 16).strip()
        self.orbit(
            "task",
            "add",
            "--title",
            "Soak template task",
            "--description",
            description,
            "--acceptance-criteria",
            "The dashboard stays near its working set under polling",
            "--acceptance-criteria",
            "Every endpoint answers within its budget",
            "--plan",
            LOREM * 4,
            "--complexity",
            "medium",
            "--tag",
            "soak,perf",
        )
        bundles = self.root / "tasks" / "workspaces" / workspace_id
        template = next(path for path in bundles.iterdir() if path.is_dir())
        template_yaml = (template / "task.yaml").read_text()
        statuses = (
            ["done"] * 3500
            + ["rejected"] * 200
            + ["backlog"] * 120
            + ["proposed"] * 80
            + ["blocked"] * 50
            + ["in_progress"] * 30
            + ["review"] * 20
        )
        rng.shuffle(statuses)
        for index in range(1, TASKS):
            task_id = f"{TASK_PREFIX}-{index:05d}"
            created = now - timedelta(minutes=(TASKS - index) * 36)
            target = bundles / task_id
            shutil.copytree(template, target)
            text = re.sub(r"^id: .*$", f"id: {task_id}", template_yaml, flags=re.M)
            text = re.sub(
                r"^title: .*$",
                f"title: Soak task {index} keeps the dashboard bounded",
                text,
                flags=re.M,
            )
            status = statuses[index % len(statuses)]
            text = re.sub(r"^status: .*$", f"status: {status}", text, flags=re.M)
            text = re.sub(r"^created_at: .*$", f"created_at: {iso(created)}", text, flags=re.M)
            text = re.sub(r"^updated_at: .*$", f"updated_at: {iso(created)}", text, flags=re.M)
            (target / "task.yaml").write_text(text)
            if status != "proposed":
                # The event log must agree with the envelope status.
                event = {
                    "schema_version": 1,
                    "event_id": "EV-0100",
                    "at": iso(created),
                    "by": "human:soak",
                    "type": "status_changed",
                    "from_status": "proposed",
                    "to_status": status,
                }
                with (target / "events.jsonl").open("a") as events:
                    events.write(json.dumps(event) + "\n")
        self.orbit("task", "reindex")
        return TASKS

    def seed_store(self, workspace_id: str, rng: random.Random, now: datetime) -> dict:
        db = sqlite3.connect(self.root / "orbit.db")
        db.execute("PRAGMA foreign_keys = OFF")
        span = timedelta(days=100)
        workspace_path = str(self.repo)

        runs = []
        steps = []
        v2 = []
        for index in range(JOB_RUNS):
            run_id = f"jrun-soak-{index:05d}"
            created = now - span * (JOB_RUNS - index) / JOB_RUNS
            roll = rng.random()
            state = "failed" if roll < 0.11 else ("cancelled" if roll < 0.12 else "success")
            duration = rng.randint(2_000, 900_000)
            finished = created + timedelta(milliseconds=duration)
            job_id = PIPELINES[index % len(PIPELINES)]
            model = MODELS[index % len(MODELS)]
            input_json = json.dumps(
                {
                    "task_ids": [f"{TASK_PREFIX}-{rng.randrange(TASKS):05d}"],
                    "base_branch": "main",
                    "source_revision": f"{rng.getrandbits(160):040x}",
                    "workspace_path": workspace_path,
                    "automation_origin": "routine",
                    "notes": LOREM * 2,
                }
            )
            runs.append(
                (
                    run_id, workspace_id, job_id, 1, state, iso(created), iso(created),
                    iso(finished), duration, iso(created), input_json, "sol", model,
                    model, model, model,
                )
            )
            for step_index, step in enumerate(("prepare", "implement_one")):
                failed = state == "failed" and step_index == 1
                steps.append(
                    (
                        workspace_id, run_id, step_index, "activity", step,
                        "failed" if failed else "success", iso(created), iso(finished),
                        duration // 2, 1 if failed else 0,
                        "step_failed" if failed else None,
                        "provider exited with status 1: validation gate failed" if failed else None,
                    )
                )
            v2.extend(self.run_events(workspace_id, run_id, job_id, created, finished, state, index))

        db.executemany(
            "INSERT INTO job_runs (run_id, workspace_id, job_id, attempt, state, scheduled_at,"
            " started_at, finished_at, duration_ms, created_at, input_json, resolved_crew,"
            " planner_model, implementer_model, reviewer_model, crew_model)"
            " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            runs,
        )
        db.executemany(
            "INSERT INTO job_run_steps (workspace_id, run_id, step_index, target_type,"
            " target_id, state, started_at, finished_at, duration_ms, exit_code, error_code,"
            " error_message) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
            steps,
        )
        db.executemany(
            "INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version,"
            " event_type, ts, run_id, agent_identity, parent_event_id, workspace_path,"
            " payload_json) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            v2,
        )
        db.commit()

        audit = []
        audit_span = timedelta(days=20)
        for index in range(AUDIT_ROWS):
            ts = now - audit_span * (AUDIT_ROWS - index) / AUDIT_ROWS
            roll = rng.random()
            status = "failure" if roll < 0.029 else ("denied" if roll < 0.055 else "success")
            model = MODELS[index % len(MODELS)]
            family = model.split("-")[0]
            tool = TOOLS[index % len(TOOLS)]
            run_id = f"jrun-soak-{rng.randrange(JOB_RUNS):05d}"
            audit.append(
                (
                    f"exec-soak-{index}", iso(ts), "tool", "run-mcp", tool, "tool", tool,
                    model, status,
                    0 if status == "success" else 1, rng.randint(1, 4_000), workspace_path,
                    "tool call refused by policy" if status == "denied"
                    else ("tool returned an error" if status == "failure" else None),
                    4242, run_id, workspace_id, "local", "agent", family,
                    "openai" if family == "gpt" else "anthropic", family, model,
                )
            )
        db.executemany(
            "INSERT INTO audit_events (execution_id, timestamp, command, subcommand, tool_name,"
            " target_type, target_id, role, status, exit_code, duration_ms, working_directory,"
            " error_message, pid, job_run_id, workspace_id, transport, actor_kind, actor_id,"
            " actor_vendor, actor_family, actor_model)"
            " VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            audit,
        )
        db.commit()

        invocations = []
        for index in range(INVOCATIONS):
            ts = now - span * (INVOCATIONS - index) / INVOCATIONS
            model = MODELS[index % len(MODELS)]
            invocations.append(
                (
                    index + 1, iso(ts), f"jrun-soak-{index * (JOB_RUNS // INVOCATIONS):05d}",
                    "implement", model.split("-")[0], model, rng.randint(10_000, 900_000),
                    rng.randint(10, 5_000), rng.randint(10_000, 400_000), rng.randint(0, 50_000),
                    rng.randint(1_000, 40_000), TOOL_CALLS_PER_INVOCATION, workspace_id,
                )
            )
        db.executemany(
            "INSERT INTO invocations (id, ts, job_run_id, activity_id, agent, model, duration_ms,"
            " input_tokens, cache_read_tokens, cache_create_tokens, output_tokens,"
            " tool_call_count, workspace_id) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            invocations,
        )
        db.executemany(
            "INSERT INTO tool_calls (invocation_id, seq, tool_name, result_bytes) VALUES (?,?,?,?)",
            (
                (invocation + 1, seq, TOOLS[seq % len(TOOLS)], rng.randint(50, 20_000))
                for invocation in range(INVOCATIONS)
                for seq in range(TOOL_CALLS_PER_INVOCATION)
            ),
        )

        frictions = []
        for index in range(FRICTIONS):
            ts = now - timedelta(days=100) * (FRICTIONS - index) / FRICTIONS
            month = ts.strftime("%Y-%m")
            frictions.append(
                (
                    workspace_id, f"F{month}-{index + 1:03d}", month, index + 1,
                    f"Soak friction {index}", MODELS[index % len(MODELS)].split("-")[0],
                    ("open", "triaged", "resolved")[index % 3], iso(ts),
                    '["tooling","automation"]', LOREM * 15,
                )
            )
        db.executemany(
            "INSERT INTO friction_records (workspace_id, friction_id, month, seq, title, model,"
            " status, created_at, tags_json, body) VALUES (?,?,?,?,?,?,?,?,?,?)",
            frictions,
        )

        routine_names = [
            re.search(r"^name:\s*(\S+)", path.read_text(), re.M).group(1)
            for path in sorted((self.root / "routines").glob("*.yaml"))
        ]
        fires = []
        for index in range(ROUTINE_FIRES):
            ts = now - span * (ROUTINE_FIRES - index) / ROUTINE_FIRES
            fires.append(
                (
                    routine_names[index % len(routine_names)],
                    iso(ts.replace(microsecond=0)),
                    "succeeded", f"jrun-soak-{index % JOB_RUNS:05d}", WORKSPACE_NAME,
                    iso(ts), iso(ts),
                )
            )
        db.executemany(
            "INSERT INTO routine_fires (routine_name, slot, state, run_id, source_workspace,"
            " created_at, updated_at) VALUES (?,?,?,?,?,?,?)",
            fires,
        )
        db.commit()
        counts = {
            table: db.execute(f"SELECT count(*) FROM {table}").fetchone()[0]
            for table in (
                "job_runs",
                "job_run_steps",
                "audit_events",
                "v2_audit_events",
                "invocations",
                "tool_calls",
                "friction_records",
                "routine_fires",
            )
        }
        db.close()
        return counts

    @staticmethod
    def run_events(workspace_id, run_id, job_id, created, finished, state, index):
        """Twelve v2 envelope events per run, shaped like the engine's."""
        workspace_path = "/srv/soak/repo"
        kinds = [
            ("run.started", "run_started", {"job_name": job_id}),
            ("step.started", "step_started", {"step_id": "prepare"}),
            ("activity.started", "activity_started", {"activity_name": "prepare", "activity_type": "deterministic"}),
            ("activity.finished", "activity_finished", {"activity_name": "prepare", "outcome": "success"}),
            ("step.finished", "step_finished", {"step_id": "prepare", "outcome": "success"}),
            ("step.started", "step_started", {"step_id": "implement_one"}),
            ("activity.started", "activity_started", {"activity_name": "implement_one", "activity_type": "agent_loop"}),
            ("cli.invocation.started", "cli_invocation_started", {"provider": "claude", "argv_digest": "0" * 64}),
            (
                "cli.invocation.finished",
                "cli_invocation_finished",
                {
                    "provider": "claude",
                    "exit_code": 1 if state == "failed" else 0,
                    "duration_ms": 33466,
                    "stdout_blob_ref": f"{index:064x}",
                    "stderr_blob_ref": f"{index + 1:064x}",
                    "harness_version": None,
                    "timed_out": False,
                },
            ),
            ("activity.finished", "activity_finished", {"activity_name": "implement_one", "outcome": state}),
            ("step.finished", "step_finished", {"step_id": "implement_one", "outcome": state}),
            ("run.finished", "run_finished", {"outcome": state}),
        ]
        events = []
        span = (finished - created) / len(kinds)
        parents = []
        for seq, (event_type, body_kind, body) in enumerate(kinds, start=1):
            event_id = f"v2evt-{run_id}-{seq:08x}"
            ts = iso(created + span * seq)
            parent = parents[-1] if parents else None
            payload = {
                "schemaVersion": 1,
                "event_type": event_type,
                "event_id": event_id,
                "ts": ts,
                "run_id": run_id,
                "agent_identity": "system",
                **({"parent_event_id": parent} if parent else {}),
                "workspace_path": workspace_path,
                "body_kind": body_kind,
                **body,
            }
            events.append(
                (
                    workspace_id, event_id, "v2_envelope", 1, event_type, ts, run_id, "system",
                    parent, workspace_path, json.dumps(payload),
                )
            )
            if body_kind in ("step_started", "activity_started"):
                parents.append(event_id)
            elif body_kind in ("step_finished", "activity_finished") and parents:
                parents.pop()
        return events

    def seed_process_log(self, rng: random.Random, now: datetime) -> int:
        logs = self.root / "state" / "logs"
        logs.mkdir(parents=True, exist_ok=True)
        span = timedelta(hours=48)
        with (logs / "orbit.jsonl").open("w") as out:
            for index in range(PROCESS_LOG_LINES):
                ts = now - span * (PROCESS_LOG_LINES - index) / PROCESS_LOG_LINES
                if index % 50 == 0:
                    event = {
                        "timestamp": iso(ts),
                        "level": "ERROR",
                        "fields": {
                            "message": "step finished with error",
                            "job_run_id": f"jrun-soak-{JOB_RUNS - 1 - rng.randrange(900):05d}",
                            "step_id": "implement_one",
                            "error_message": "provider exited with status 1",
                            "event_id": f"evt-{index}",
                        },
                        "target": "orbit.job.step_finished",
                    }
                else:
                    event = {
                        "timestamp": iso(ts),
                        "level": "WARN",
                        "fields": {
                            "message": "still waiting for advisory file lock; a holder may be hung",
                            "lock_path": "/srv/soak/root/tasks/.task-commit.lock",
                            "waited_ms": rng.randint(3_000, 9_000),
                        },
                        "target": "orbit.common.fs.file_lock",
                    }
                out.write(json.dumps(event) + "\n")
        return PROCESS_LOG_LINES


# ── measurement ──────────────────────────────────────────────────────────────


def proc_status(pid: int) -> dict:
    fields = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        key, _, value = line.partition(":")
        if key in ("VmRSS", "VmHWM", "Threads"):
            fields[key] = int(value.split()[0])
    return {"rss_kb": fields["VmRSS"], "hwm_kb": fields["VmHWM"], "threads": fields["Threads"]}


def arena_like_mappings(pid: int) -> int:
    """Writable anonymous mappings starting on a 64 MiB boundary.

    glibc allocates each secondary malloc arena as a heap aligned to its
    64 MiB maximum size, so this approximates the number of arenas in use.
    """
    count = 0
    for line in Path(f"/proc/{pid}/maps").read_text().splitlines():
        parts = line.split()
        if len(parts) > 5 or not parts[1].startswith("rw"):
            continue
        start = int(parts[0].split("-")[0], 16)
        if start % (64 * 1024 * 1024) == 0:
            count += 1
    return count


class Server:
    def __init__(self, fixture: Fixture, extra_env: dict[str, str] | None = None):
        env = dict(fixture.env)
        env.update(extra_env or {})
        self.process = subprocess.Popen(
            [
                str(fixture.binary), "--root", str(fixture.root), "web", "serve",
                "--port", "0", "--no-open", "--workspace", WORKSPACE_NAME,
            ],
            cwd=fixture.repo,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        line = self.process.stdout.readline()
        match = re.search(r"http://(\S+)", line)
        if not match:
            self.stop()
            raise RuntimeError(f"dashboard did not announce its address: {line!r}")
        self.base = f"http://{match.group(1)}"
        self.pid = self.process.pid

    def get(self, path: str) -> dict:
        started = time.monotonic()
        request = urllib.request.Request(self.base + path)
        with urllib.request.urlopen(request, timeout=300) as response:
            body = response.read()
            status = response.status
        if status != 200:
            raise RuntimeError(f"GET {path} returned {status}")
        return {"path": path, "ms": round((time.monotonic() - started) * 1000), "bytes": len(body)}

    def stop(self) -> None:
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def settle() -> None:
    # Let the response finish and any post-request housekeeping run.
    time.sleep(2.0)


def fresh_request_growth(fixture: Fixture, path: str, extra_env) -> dict:
    server = Server(fixture, extra_env)
    try:
        server.get("/api/workspaces")
        server.get(f"/api/tasks/locks?workspace={WORKSPACE_ID}")
        settle()
        before = proc_status(server.pid)
        timing = server.get(path)
        settle()
        after = proc_status(server.pid)
    finally:
        server.stop()
    return {
        "path": path,
        "ms": timing["ms"],
        "bytes": timing["bytes"],
        "rss_before_mb": round(before["rss_kb"] / 1024, 1),
        "rss_after_mb": round(after["rss_kb"] / 1024, 1),
        "rss_growth_mb": round((after["rss_kb"] - before["rss_kb"]) / 1024, 1),
        "hwm_growth_mb": round((after["hwm_kb"] - before["hwm_kb"]) / 1024, 1),
    }


def soak(fixture: Fixture, rounds: int, interval: float, extra_env) -> dict:
    server = Server(fixture, extra_env)
    paths = [path.format(ws=WORKSPACE_ID) for path in GET_MIX]
    samples = []
    try:
        start = proc_status(server.pid)
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(paths)) as pool:
            for number in range(1, rounds + 1):
                round_started = time.monotonic()
                timings = list(pool.map(server.get, paths))
                settle()
                status = proc_status(server.pid)
                sample = {
                    "round": number,
                    "rss_mb": round(status["rss_kb"] / 1024, 1),
                    "hwm_mb": round(status["hwm_kb"] / 1024, 1),
                    "threads": status["threads"],
                    "arena_like_mappings": arena_like_mappings(server.pid),
                    "slowest_ms": max(timing["ms"] for timing in timings),
                }
                if number == 1:
                    sample["requests"] = timings
                samples.append(sample)
                print(
                    f"  round {number:2d}: rss {sample['rss_mb']:7.1f} MB  "
                    f"hwm {sample['hwm_mb']:7.1f} MB  threads {sample['threads']:3d}  "
                    f"64MiB-maps {sample['arena_like_mappings']:2d}  "
                    f"slowest {sample['slowest_ms']} ms",
                    flush=True,
                )
                remaining = interval - (time.monotonic() - round_started)
                if number < rounds and remaining > 0:
                    time.sleep(remaining)
        final = proc_status(server.pid)
    finally:
        server.stop()
    baseline_round = min(3, rounds)
    rss_baseline = samples[baseline_round - 1]["rss_mb"]
    rss_final = samples[-1]["rss_mb"]
    # The two-sample drift check cannot tell a ratchet from round-to-round
    # variation, so the report also carries the range the later rounds span.
    settled = [sample["rss_mb"] for sample in samples[baseline_round - 1 :]]
    return {
        "rounds": rounds,
        "interval_s": interval,
        "startup_rss_mb": round(start["rss_kb"] / 1024, 1),
        "rss_after_round_3_mb": rss_baseline,
        "rss_after_last_round_mb": rss_final,
        "drift": round((rss_final - rss_baseline) / rss_baseline, 3),
        "rss_range_from_round_3_mb": [min(settled), max(settled)],
        "peak_rss_mb": round(final["hwm_kb"] / 1024, 1),
        "samples": samples,
    }


def task_list_timing(fixture: Fixture, runs: int) -> dict:
    fixture.orbit("task", "list")  # warm the page cache
    walls = []
    for _ in range(runs):
        started = time.monotonic()
        fixture.orbit("task", "list")
        walls.append((time.monotonic() - started) * 1000)
    return {
        "runs": runs,
        "median_ms": round(statistics.median(walls), 1),
        "min_ms": round(min(walls), 1),
        "max_ms": round(max(walls), 1),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bin", required=True, type=Path, help="orbit binary to measure")
    parser.add_argument("--work-dir", type=Path, help="fixture directory (reused when it already holds one)")
    parser.add_argument("--report", type=Path, help="write the JSON report here")
    parser.add_argument("--rounds", type=int, default=20)
    parser.add_argument("--interval", type=float, default=DEFAULT_INTERVAL_S)
    parser.add_argument("--task-list-runs", type=int, default=9)
    parser.add_argument(
        "--server-env",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="extra environment for the dashboard process (e.g. MALLOC_ARENA_MAX=2)",
    )
    parser.add_argument("--report-only", action="store_true", help="never fail on a budget")
    args = parser.parse_args()

    if not sys.platform.startswith("linux"):
        print("web-memory-soak: Linux only (reads /proc and glibc arena layout)", file=sys.stderr)
        return 2
    binary = args.bin.resolve()
    if not binary.is_file():
        print(f"web-memory-soak: no binary at {binary}", file=sys.stderr)
        return 2
    extra_env = dict(item.split("=", 1) for item in args.server_env)

    temp = None
    if args.work_dir:
        work = args.work_dir.resolve()
        work.mkdir(parents=True, exist_ok=True)
    else:
        temp = tempfile.TemporaryDirectory(prefix="orbit-web-soak-")
        work = Path(temp.name)
    fixture = Fixture(work, binary)
    try:
        if fixture.exists():
            manifest = json.loads((work / "fixture.json").read_text())
            print(f"Reusing fixture in {work}")
        else:
            print(f"Generating fixture in {work} ...", flush=True)
            manifest = fixture.build()
        print(json.dumps(manifest["counts"]))

        print("Fresh-server single requests:", flush=True)
        fresh = [
            fresh_request_growth(fixture, f"/api/scoreboard?window=24h&workspace={WORKSPACE_ID}", extra_env),
            fresh_request_growth(fixture, "/api/routines", extra_env),
        ]
        for item in fresh:
            print(
                f"  {item['path']}: rss +{item['rss_growth_mb']} MB, "
                f"hwm +{item['hwm_growth_mb']} MB, {item['ms']} ms"
            )
        print(f"Soak: {args.rounds} rounds every {args.interval:g}s", flush=True)
        soak_report = soak(fixture, args.rounds, args.interval, extra_env)
        timing = task_list_timing(fixture, args.task_list_runs)
        print(f"orbit task list: median {timing['median_ms']} ms over {timing['runs']} runs")
    finally:
        if temp is not None:
            temp.cleanup()

    checks = {
        "soak_drift_within_15pct": abs(soak_report["drift"]) <= SOAK_DRIFT_BUDGET,
        "peak_rss_under_512mb": soak_report["peak_rss_mb"] < PEAK_RSS_BUDGET_MB,
        **{
            f"single_request_under_20mb:{item['path'].split('?')[0]}": item["rss_growth_mb"]
            < SINGLE_REQUEST_BUDGET_MB
            for item in fresh
        },
    }
    report = {
        "binary": str(binary),
        "server_env": extra_env,
        "fixture": manifest,
        "fresh_requests": fresh,
        "soak": soak_report,
        "task_list": timing,
        "checks": checks,
    }
    if args.report:
        args.report.write_text(json.dumps(report, indent=2))
    print(
        f"Soak: round 3 {soak_report['rss_after_round_3_mb']} MB -> round {args.rounds} "
        f"{soak_report['rss_after_last_round_mb']} MB (drift {soak_report['drift']:+.1%}), "
        f"peak {soak_report['peak_rss_mb']} MB; rounds 3-{args.rounds} span "
        f"{soak_report['rss_range_from_round_3_mb'][0]}-{soak_report['rss_range_from_round_3_mb'][1]} MB"
    )
    for name, passed in checks.items():
        print(f"  {'PASS' if passed else 'FAIL'} {name}")
    if all(checks.values()) or args.report_only:
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
