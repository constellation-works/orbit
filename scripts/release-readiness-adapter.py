#!/usr/bin/env python3
"""Judge release-readiness evidence for Orbit's `release-readiness/v1` contract.

Reads one JSON document on stdin and prints one verdict on stdout. The input's
`trusted` block and `tasks` come from the owner's records; `evidence` is what the
run left, which this script checks but never believes. It owns this workspace's
QA, security and preparation policy. It writes nothing, reaches nothing, and any
gap answers `unavailable`, so only complete evidence can pass.

Forms:
  qa-full-sweep    the full QA report and supplemental forecast scenarios
  security-review  the structured scope, evidence and finding references
  release-prep     the actual canonical preparation handoff; a board-busy or
                   nothing-to-release probe is a valid outcome that never passes

Exit status is 0 for every verdict, so nonzero means the adapter itself failed.
"""

import hashlib
import json
import os
import re
import runpy
import sys
from pathlib import Path

SCHEMA_VERSION = 1
CONTRACT = "release-readiness/v1"
SHA_RE = re.compile(r"[0-9a-f]{40}")

QA_REPORT = "qa-full-sweep-report.json"
QA_FORECAST = "forecast-history-qa.json"
QA_CAPABILITY_EVIDENCE = "capability-gap-evidence.json"
QA_INVENTORY = "qa-full-sweep-inventory.json"
# The native definition's required supplemental forecast scenarios.
QA_FORECAST_SCENARIOS = ("legacy-history", "forecast-contract", "rendered-behavior", "historical-evidence")

SECURITY_EVIDENCE = "security-review-evidence.json"
SECURITY_AREAS = ("application-code", "dependencies", "secret-handling", "configuration")
BLOCKING_PRIORITIES = {"high", "critical"}
RESOLVED_STATUSES = {"done", "rejected"}

PREP_EVIDENCE = "release-prep-evidence.json"
PREP_TITLE_RE = re.compile(r"Prepare v(\d+)\.(\d+)\.(\d+) release")
PREP_TAGS = {"release", "no-diff-expected"}
PREP_CONTEXT = {"file:CHANGELOG.md", "file:Cargo.toml", "file:Cargo.lock", "file:npm/package.json"}
OPEN_STATUSES = {"backlog", "proposed", "in-progress", "review", "blocked"}


class Verdict(Exception):
    """Stop judging with one normalized result."""

    def __init__(self, status, code, detail):
        super().__init__(detail)
        self.status = status
        self.code = code
        self.detail = detail


def unavailable(code, detail):
    raise Verdict("unavailable", code, detail)


def blocked(code, detail):
    raise Verdict("blocked", code, detail)


def artifact_json(request, path):
    """A listed artifact's parsed JSON; a missing or unparsable one is a gap."""
    entry = request["evidence"]["artifacts"].get(path)
    if entry is None:
        unavailable("evidence-missing", f"the evidence does not list `{path}`")
    body = entry.get("json")
    if not isinstance(body, dict):
        unavailable("evidence-malformed", f"`{path}` is not a JSON object")
    return body


def expect_revision(actual, trusted, where):
    if actual != trusted:
        unavailable("evidence-stale", f"{where} covers revision {actual!r}, not {trusted}")


def expect_refs_listed(request, refs, where):
    """Every evidence reference must name an artifact the manifest digests."""
    listed = request["evidence"]["artifacts"]
    if not isinstance(refs, list) or not refs:
        unavailable("evidence-missing", f"{where} cites no evidence")
    for ref in refs:
        if not isinstance(ref, str) or ref not in listed:
            unavailable("evidence-missing", f"{where} cites `{ref}`, which the manifest does not list")


def load_inventory():
    root = os.environ.get("ORBIT_SCRIPT_IN")
    if not root:
        unavailable("inventory-unavailable", "no inventory file was mounted for the adapter")
    path = Path(root) / QA_INVENTORY
    try:
        raw = path.read_bytes()
        return json.loads(raw), hashlib.sha256(raw).hexdigest()
    except (OSError, ValueError) as error:
        unavailable("inventory-unavailable", f"the QA inventory is unreadable: {error}")


def judge_qa_results(inventory, report, candidate_id):
    """The inventory is the read-only coverage contract: every required scenario
    needs a passing, same-candidate result with exactly its required assertions."""
    results = report.get("results")
    if not isinstance(results, list):
        unavailable("report-malformed", "the report has no results")
    for scenario in inventory["scenarios"]:
        if not scenario.get("required"):
            continue
        name = scenario["id"]
        if scenario.get("kind") == "coverage-gap" or scenario.get("coverage_gaps"):
            unavailable("required-coverage-gap", f"{name} has a required coverage gap")
        rows = [row for row in results if isinstance(row, dict) and row.get("scenario") == name]
        if not rows:
            unavailable("scenario-missing", f"required scenario {name} has no result")
        if any(row.get("outcome") != "PASS" for row in rows):
            unavailable("scenario-not-passed", f"required scenario {name} did not pass")
        if any(row.get("candidate_id") != candidate_id for row in rows):
            unavailable("evidence-stale", f"required scenario {name} ran on another candidate")
        observed = {item for row in rows for item in row.get("assertions", [])}
        if observed != set(scenario.get("assertions", [])):
            unavailable("assertions-mismatch", f"required scenario {name} has different assertions")
        # Parity with producer scenario_decision in test-qa-full-sweep.py:
        # a required browser PASS row needs retained evidence, and every
        # retired case with a command needs a row that executed that command.
        if scenario.get("capability") == "browser" and any(
                not row.get("retained_evidence") for row in rows):
            unavailable("retained-evidence-missing",
                        f"required scenario {name} has a browser PASS row without retained_evidence")
        for case in scenario.get("retired_cases") or []:
            command = case.get("command") if isinstance(case, dict) else None
            if command and not any(row.get("command") == command for row in rows):
                unavailable("retired-case-not-executed",
                            f"required scenario {name} retired case {case.get('retired')} has no executed command")
        capability = scenario.get("capability")
        if capability == "macos":
            # Use the harness's complete certificate and native-execution
            # contract, against declared candidate files rather than report labels.
            inputs = Path(os.environ["ORBIT_SCRIPT_IN"])
            source_files = {
                path: inputs / Path(path).name
                for path in (
                    "scripts/check-ci-macos.sh", "scripts/qa-full-sweep-inventory.json",
                    "scripts/test-qa-full-sweep.py", ".github/workflows/ci-macos.yml",
                )
            }
            try:
                validate_native = runpy.run_path(str(inputs / "test-qa-full-sweep.py"))[
                    "validate_hosted_macos_evidence"]
                candidate = {"head": report["candidate"]["source_revision"],
                             "status": report["source_state"]["status"]}
                for row in rows:
                    validate_native(row.get("native_evidence", {}), scenario, candidate,
                                    inputs, source_files=source_files)
            except (OSError, ImportError, KeyError, TypeError, AttributeError, ValueError) as error:
                unavailable("platform-evidence-missing", f"{name} has no valid macOS evidence: {error}")
        if capability == "linux" and report.get("environment", {}).get("os") != "Linux" and not all(
            row.get("imported_from") and row.get("evidence_sha256") for row in rows
        ):
            unavailable("platform-evidence-missing", f"{name} has no matching Linux evidence")


def judge_capability_gaps(request, inventory, report, candidate_id, revision):
    """A required capability gap stays unavailable until evidence for this exact
    candidate discharges it. The report's own decision may then be INCOMPLETE
    only for gaps discharged that way."""
    gaps = inventory.get("required_capability_gaps", {})
    decision = report.get("decision")
    findings = report.get("findings", [])
    if decision == "PASS" and not findings and not gaps:
        return
    if decision not in ("PASS", "INCOMPLETE"):
        unavailable("report-malformed", f"the report decision is {decision!r}")
    if gaps and QA_CAPABILITY_EVIDENCE not in request["evidence"]["artifacts"]:
        unavailable("capability-gap", f"required capability gaps are not discharged: {sorted(gaps)}")
    discharged = {}
    if gaps:
        body = artifact_json(request, QA_CAPABILITY_EVIDENCE)
        if body.get("candidate_id") != candidate_id:
            unavailable("evidence-stale", "the capability evidence is for another candidate")
        expect_revision(body.get("source_revision"), revision, "the capability evidence")
        discharged = body.get("surfaces") if isinstance(body.get("surfaces"), dict) else {}
    for surface in gaps:
        entry = discharged.get(surface)
        if not isinstance(entry, dict) or entry.get("outcome") != "PASS":
            unavailable("capability-gap", f"required capability gap {surface} is not discharged")
        expect_refs_listed(request, entry.get("evidence"), f"capability gap {surface}")
    for finding in findings:
        surface = str(finding).split(": required capability gap:", 1)[0]
        if surface == str(finding) or surface not in gaps:
            unavailable("report-incomplete", f"the report records: {finding}")


def judge_qa_forecast(request, candidate_id, revision):
    forecast = artifact_json(request, QA_FORECAST)
    if forecast.get("candidate_id") != candidate_id:
        unavailable("evidence-stale", "the forecast scenarios ran on another candidate")
    expect_revision(forecast.get("source_revision"), revision, "the forecast scenarios")
    scenarios = forecast.get("scenarios")
    if not isinstance(scenarios, list):
        unavailable("evidence-malformed", "the forecast evidence has no scenarios")
    by_id = {row.get("id"): row for row in scenarios if isinstance(row, dict)}
    for name in QA_FORECAST_SCENARIOS:
        row = by_id.get(name)
        if row is None:
            unavailable("scenario-missing", f"forecast scenario {name} has no result")
        if row.get("outcome") != "PASS":
            unavailable("scenario-not-passed", f"forecast scenario {name} did not pass")
        expect_refs_listed(request, row.get("evidence"), f"forecast scenario {name}")


def judge_qa(request):
    revision = request["trusted"]["source_revision"]
    report = artifact_json(request, QA_REPORT)
    if report.get("schema_version") != 2:
        unavailable("report-malformed", "the report has an unknown schema_version")
    candidate = report.get("candidate")
    if not isinstance(candidate, dict) or not candidate.get("id"):
        unavailable("report-malformed", "the report has no candidate")
    expect_revision(candidate.get("source_revision"), revision, "the QA report")
    state = report.get("source_state")
    if not isinstance(state, dict) or state.get("status"):
        unavailable("candidate-dirty", "the QA report's checkout was not a clean candidate")
    inventory, inventory_sha = load_inventory()
    if report.get("inventory_sha256") != inventory_sha:
        unavailable("inventory-mismatch", "the report ran a different coverage inventory")
    candidate_id = candidate["id"]
    judge_qa_results(inventory, report, candidate_id)
    judge_capability_gaps(request, inventory, report, candidate_id, revision)
    judge_qa_forecast(request, candidate_id, revision)


def judge_security(request):
    trusted = request["trusted"]
    body = artifact_json(request, SECURITY_EVIDENCE)
    expect_revision(body.get("source_revision"), trusted["source_revision"], "the security review")
    scope = body.get("scope")
    if not isinstance(scope, list):
        unavailable("evidence-malformed", "the security review has no scope")
    reviewed = {area.get("area"): area for area in scope if isinstance(area, dict)}
    for name in SECURITY_AREAS:
        area = reviewed.get(name)
        if area is None:
            unavailable("scope-incomplete", f"review area {name} is not covered")
        if not area.get("summary"):
            unavailable("scope-incomplete", f"review area {name} has no summary")
        expect_refs_listed(request, area.get("evidence"), f"review area {name}")
    findings = body.get("findings")
    if not isinstance(findings, list):
        unavailable("evidence-malformed", "the security review has no findings list")
    bound = trusted["task_id"]
    spawned = {task["id"]: task for task in request["tasks"]["spawned"]}
    referenced = {task["id"]: task for task in request["tasks"]["referenced"]}
    cited = set()
    unresolved = []
    for finding in findings:
        if not isinstance(finding, dict) or not finding.get("task_id"):
            unavailable("finding-malformed", "a finding names no task")
        disposition = finding.get("disposition", "new")
        if disposition not in ("new", "duplicate"):
            unavailable("finding-malformed", "a finding has an unknown disposition")
        task_id = finding["task_id"]
        cited.add(task_id)
        task = referenced.get(task_id) if disposition == "duplicate" else (
            spawned.get(task_id) or referenced.get(task_id)
        )
        if task is None or task.get("status") == "missing":
            unavailable("finding-unresolved", f"finding task {task_id} does not exist")
        edge = any(
            relation.get("type") == "spawned_from" and relation.get("target") == bound
            for relation in task.get("relations", [])
        )
        if disposition == "duplicate":
            # Duplicates are read back through task_refs, retaining their
            # original provenance. A current-review spawn cannot opt out of
            # the new-finding contract by calling itself a duplicate.
            if task_id in spawned or edge:
                unavailable("finding-not-duplicate", f"finding task {task_id} belongs to this review")
        elif not edge:
            unavailable("finding-unlinked", f"finding task {task_id} is not spawned from this review")
        severity = str(finding.get("severity", "")).lower()
        if (severity in BLOCKING_PRIORITIES or task.get("priority") in BLOCKING_PRIORITIES) and (
            task.get("status") not in RESOLVED_STATUSES
        ):
            unresolved.append(task_id)
    # Trusted spawned work the review omitted is a gap in the evidence itself.
    omitted = sorted(set(spawned) - cited)
    if omitted:
        unavailable("finding-omitted", f"spawned tasks are not reported as findings: {omitted}")
    if unresolved:
        blocked("unresolved-findings", f"release-blocking findings are open: {sorted(unresolved)}")


def expected_version(latest_tag, breaking):
    """This workspace's pre-1.0 policy: a patch bump, or a minor bump for a break."""
    match = re.fullmatch(r"v(\d+)\.(\d+)\.(\d+)", latest_tag or "")
    if match is None:
        unavailable("survey-malformed", f"the survey's latest tag is {latest_tag!r}")
    major, minor, patch = (int(part) for part in match.groups())
    if major != 0:
        unavailable("survey-malformed", "the pre-1.0 version policy does not cover this tag")
    return f"{major}.{minor + 1}.0" if breaking else f"{major}.{minor}.{patch + 1}"


def trusted_prep_survey(trusted):
    """Pinned survey facts the owner obtained independently of the candidate.

    Core derives them from Git at the pinned revision and from owner task
    records (see `run_adapter` in completion.rs) and supplies them in the
    trusted block; a handoff without them has nothing to be checked against.
    """
    survey = trusted.get("release_survey")
    if not isinstance(survey, dict):
        unavailable("trusted-survey-missing",
                    "the owner supplied no independently verified release survey")
    if survey.get("head") != trusted.get("source_revision"):
        unavailable("trusted-survey-malformed", "the trusted survey does not end at the pinned revision")
    commits = survey.get("commits")
    if not isinstance(commits, list) or not all(
            isinstance(commit, str) and SHA_RE.fullmatch(commit) for commit in commits):
        unavailable("trusted-survey-malformed", "the trusted survey has no valid commit range")
    if not isinstance(survey.get("breaking"), bool):
        unavailable("trusted-survey-malformed", "the trusted survey has no breaking classification")
    if not isinstance(survey.get("latest_tag"), str):
        unavailable("trusted-survey-malformed", "the trusted survey has no latest tag")
    return survey


def judge_prep(request):
    trusted = request["trusted"]
    body = artifact_json(request, PREP_EVIDENCE)
    expect_revision(body.get("source_revision"), trusted["source_revision"], "the preparation report")
    scenario = body.get("scenario")
    if scenario == "blocked":
        blocked("prep-board-busy", "the probe reported a busy board; no preparation was accepted")
    if scenario == "no_release":
        blocked("prep-nothing-to-release", "the probe found nothing to release; no preparation was accepted")
    if scenario != "handoff":
        unavailable("evidence-malformed", f"the preparation scenario is {scenario!r}")
    if body.get("blockers"):
        unavailable("prep-inconsistent", "a handoff reports blockers")
    survey = body.get("survey")
    if not isinstance(survey, dict) or not survey.get("commits"):
        unavailable("survey-malformed", "the handoff has no commit survey")
    if survey.get("head") != trusted["source_revision"]:
        unavailable("evidence-stale", "the survey does not end at the pinned revision")
    # Agreement between the candidate evidence and the canonical task (both
    # agent-authored) is not proof: the tag, range and breaking classification
    # must equal the owner's independently verified pinned facts, and the
    # version is derived from those facts, not from the candidate's fields.
    trusted_survey = trusted_prep_survey(trusted)
    if not trusted_survey["commits"]:
        unavailable("survey-mismatch", "the trusted survey is empty; there is nothing to release")
    if survey.get("latest_tag") != trusted_survey["latest_tag"]:
        unavailable("survey-mismatch",
                    f"the handoff tag is {survey.get('latest_tag')!r}; "
                    f"the pinned source gives {trusted_survey['latest_tag']}")
    if sorted(survey.get("commits")) != sorted(trusted_survey["commits"]):
        unavailable("survey-mismatch", "the handoff commit range does not match the pinned source")
    if (survey.get("breaking") is True) != trusted_survey["breaking"]:
        unavailable("survey-mismatch", "the handoff breaking classification does not match the pinned source")
    version = body.get("version")
    expected = expected_version(trusted_survey["latest_tag"], trusted_survey["breaking"])
    if version != expected:
        unavailable("version-mismatch", f"the handoff names {version!r}; the pinned survey gives {expected}")

    canonical = sorted(
        (task for task in request["tasks"]["tagged"]
         if PREP_TITLE_RE.fullmatch(task.get("title", "")) and task.get("status") in OPEN_STATUSES),
        key=lambda task: task["id"],
    )
    if not canonical:
        unavailable("handoff-missing", "no open canonical preparation task exists")
    chosen = canonical[0]
    if body.get("canonical_task_id") != chosen["id"]:
        unavailable("handoff-mismatch",
                    f"the handoff names {body.get('canonical_task_id')!r}, not the canonical {chosen['id']}")
    if chosen["title"] != f"Prepare v{version} release":
        unavailable("handoff-mismatch", f"the canonical task is `{chosen['title']}`")
    if not PREP_TAGS.issubset(chosen.get("tags", [])):
        unavailable("handoff-mismatch", "the canonical task lacks its required tags")
    if not PREP_CONTEXT.issubset(chosen.get("context_files", [])):
        unavailable("handoff-mismatch", "the canonical task lacks its release selectors")
    description = chosen.get("description", "")
    if trusted["source_revision"] not in description or version not in description:
        unavailable("handoff-mismatch", "the canonical task does not record its revision and version")


FORMS = {"qa-full-sweep": judge_qa, "security-review": judge_security, "release-prep": judge_prep}


def judge(request):
    form = request.get("form")
    judge_form = FORMS.get(form)
    if judge_form is None:
        unavailable("form-unknown", f"this adapter has no form {form!r}")
    if request.get("contract") != CONTRACT:
        unavailable("contract-unknown", f"this adapter judges {CONTRACT}")
    trusted = request.get("trusted")
    if not isinstance(trusted, dict) or not SHA_RE.fullmatch(str(trusted.get("source_revision"))):
        unavailable("input-malformed", "the input has no trusted source revision")
    if not isinstance(request.get("evidence"), dict) or not isinstance(request["evidence"].get("artifacts"), dict):
        unavailable("input-malformed", "the input has no evidence")
    if not isinstance(request.get("tasks"), dict):
        unavailable("input-malformed", "the input has no task records")
    judge_form(request)


def verdict_for(request):
    status, reasons = "passed", []
    try:
        judge(request)
    except Verdict as verdict:
        status, reasons = verdict.status, [{"code": verdict.code, "detail": verdict.detail}]
    except (KeyError, TypeError, AttributeError, ValueError) as error:
        status = "unavailable"
        reasons = [{"code": "evidence-malformed", "detail": f"the evidence has an unexpected shape: {error!r}"}]
    trusted = request.get("trusted") if isinstance(request.get("trusted"), dict) else {}
    return {
        "schema_version": SCHEMA_VERSION,
        "contract": str(request.get("contract", "")),
        "form": str(request.get("form", "")),
        "task_id": str(trusted.get("task_id", "")),
        "source_revision": str(trusted.get("source_revision", "")),
        "status": status,
        "reasons": reasons,
    }


def main():
    try:
        request = json.load(sys.stdin)
    except ValueError as error:
        request, malformed = {}, error
    else:
        malformed = None if isinstance(request, dict) else "the input is not an object"
        request = request if isinstance(request, dict) else {}
    result = verdict_for(request)
    if malformed is not None:
        result["status"] = "unavailable"
        result["reasons"] = [{"code": "input-malformed", "detail": str(malformed)}]
    json.dump(result, sys.stdout, sort_keys=True)
    print()


if __name__ == "__main__":
    main()
