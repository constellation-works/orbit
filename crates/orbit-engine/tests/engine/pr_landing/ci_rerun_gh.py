"""Offline provider for the public CI producer/adapter/completion regressions."""
import json
from pathlib import Path
import subprocess
import sys

root = Path(sys.argv[1])
args = sys.argv[2:]
config = json.loads((root / "retry-config.json").read_text())
applied = (root / "retry-applied").exists()
phase = 2 if applied else 1
merged = (root / "retry-merged").exists()
repo = {"id": 500, "full_name": "orbit/test", "url": "https://api.github.com/repos/orbit/test"}
app = {"id": 901 if config.get("wrong_app") else 900, "slug": "github-actions"}
head, base = config["head"], config["base"]
branch, base_branch = "orbit/landing-candidate", "agent-main"
names = ["test", "lint", "independent"]

def outcome(name, is_base=False):
    return "success" if is_base or (name == "independent" and not config.get("candidate")) or (applied and not config.get("repeat")) else "failure"

def emit(value):
    print(json.dumps(value))
    raise SystemExit

def source(path, revision):
    blob = subprocess.check_output(["git", "rev-parse", f"{revision}:{path}"], cwd=config["workspace"], text=True).strip()
    size = int(subprocess.check_output(["git", "cat-file", "-s", blob], cwd=config["workspace"], text=True))
    return {"type": "file", "path": path, "sha": blob, "size": size}

def run(is_base=False):
    sha = base if is_base else head
    result = {"id": 701 if is_base else 700, "name": "CI", "workflow_id": 600,
              "run_attempt": 1 if is_base else phase, "check_suite_id": 800,
              "path": ".github/workflows/retry.yml", "head_sha": sha,
              "head_branch": base_branch if is_base else branch,
              "event": "push" if is_base else "pull_request",
              "repository": repo, "head_repository": repo, "actor": {"id": 123},
              "status": "completed", "conclusion": "success" if (applied and not config.get("repeat")) or is_base else "failure",
              "run_started_at": "2026-10-29T10:00:00Z",
              "pull_requests": [] if is_base else [{"number": 42, "head": {"sha": head}, "base": {"sha": base, "ref": base_branch, "repo": repo}}]}
    if is_base and config.get("base_unproven"):
        result["conclusion"] = "failure"
    if not is_base:
        result.update(config.get("run_override", {}))
    return result

def jobs(is_base=False, producing=False):
    sha = base if is_base else head
    rows = []
    for index, name in enumerate(names):
        step = "Wait" if name == "lint" else "Tests"
        conclusion = outcome(name, is_base)
        rows.append({"id": (2000 if is_base else 1000) + index + (10 if applied else 0), "run_id": 701 if is_base else 700,
                     "run_attempt": 1 if is_base else phase, "head_sha": sha, "name": name,
                     "status": "completed", "conclusion": conclusion,
                     "started_at": "2026-10-29T10:02:00Z" if applied else "2026-10-29T10:00:00Z", "completed_at": "2026-10-29T10:03:00Z" if applied else "2026-10-29T10:01:00Z",
                     "check_run_url": f"https://api.github.com/repos/orbit/test/check-runs/{1100 + index}",
                     "steps": [{"name": step, "number": 1, "status": "completed", "conclusion": conclusion}]})
    rows[1]["steps"][0]["number"] = 2
    rows[1]["steps"].insert(0, {"name": "Check out producer", "number": 1, "status": "completed", "conclusion": "success"})
    rows[1]["steps"].append({"name": "Own check", "number": 3, "status": "completed", "conclusion": "failure" if config.get("own_step") and not is_base else "success"})
    if producing:
        rows[1]["status"] = "in_progress"
        rows[1]["conclusion"] = None
        rows[1]["steps"][1].update(status="in_progress", conclusion=None)
    if not is_base and not producing:
        for row in rows:
            row.update(config.get("job_overrides", {}).get(str(row["id"]), {}))
    for row in rows:
        for index, step in enumerate(row["steps"]):
            starts = ["00", "10", "50"] if len(row["steps"]) == 3 else ["00"]
            ends = ["10", "50", "60"] if len(row["steps"]) == 3 else ["60"]
            step.setdefault("started_at", f"2026-10-29T10:00:{starts[index]}Z")
            end = f"2026-10-29T10:00:{ends[index]}Z" if ends[index] != "60" else "2026-10-29T10:01:00Z"
            step.setdefault("completed_at", end)
    result = {"total_count": len(rows), "jobs": rows}
    if config.get("incomplete") and not is_base and not producing:
        result["total_count"] += 1
    return result

if args[:2] == ["run", "rerun"]:
    if not (root / "retry-reserved").exists():
        raise SystemExit("rerun without durable reservation")
    with (root / "retry-sends").open("a") as file:
        file.write(" ".join(args) + "\n")
    if config.get("reply") != "unknown":
        (root / "retry-applied").touch()
    if config.get("reply") in ("lost", "unknown"):
        print("lost provider reply", file=sys.stderr)
        raise SystemExit(1)
    raise SystemExit
if args[:2] == ["run", "view"]:
    if "--json" in args:
        rows = [dict(row, databaseId=row["id"]) for row in jobs()["jobs"]]
        if config.get("fallback_name"):
            rows[0]["name"] = "foreign"
        emit({"databaseId": 700, "url": "https://github.com/orbit/test/actions/runs/700", "jobs": rows})
    if config.get("raw_fallback"):
        raise SystemExit
    job = args[args.index("--job") + 1]
    print((root / f"retry-log-{job}").read_text(), end="")
    raise SystemExit
if args[:2] == ["pr", "view"]:
    emit({"number": 42, "url": "https://github.com/orbit/test/pull/42", "state": "MERGED" if merged else "OPEN",
          "headRefName": branch, "baseRefName": base_branch, "headRefOid": head,
          "mergeStateStatus": "CLEAN" if applied and not config.get("repeat") else "BLOCKED", "reviewDecision": "APPROVED",
          "isDraft": False, "mergedAt": "2026-10-29T10:02:00Z" if merged else None, "mergeCommit": {"oid": head} if merged else None,
          "statusCheckRollup": [{"__typename": "CheckRun", "name": name, "workflowName": "CI",
                                 "status": "COMPLETED", "conclusion": outcome(name).upper()} for name in names]})
if args[0] != "api":
    raise SystemExit(77)
if args[1] == "graphql":
    if any("requiredStatusChecks" in arg for arg in args):
        emit({"data": {"repository": {"pullRequest": {"headRefOid": head, "baseRefName": base_branch, "reviewDecision": "APPROVED", "baseRef": {"branchProtectionRule": {"requiredStatusChecks": [{"context": name, "app": {"databaseId": 900}} for name in names]}}}}}})
    if any("statusCheckRollup" in arg for arg in args):
        contexts = {"pageInfo": {"hasNextPage": False}, "nodes": [{"__typename": "CheckRun", "name": name, "workflowName": "CI", "status": "COMPLETED", "conclusion": outcome(name).upper(), "isRequired": True} for name in names]}
        emit({"data": {"repository": {"pullRequest": {"headRefOid": head, "commits": {"nodes": [{"commit": {"oid": head, "statusCheckRollup": {"contexts": contexts}}}]}}}}})
    raise SystemExit(77)
endpoint = next((arg for arg in args[1:] if arg.startswith("repos/")), "")
if "/actions/jobs/" in endpoint and endpoint.endswith("/logs"):
    job = endpoint.split("/actions/jobs/", 1)[1].split("/", 1)[0]
    print((root / f"retry-log-{job}").read_text(), end="")
    raise SystemExit
if "--include" in args and "actions/runs/700/jobs" in endpoint:
    print("HTTP/2 200 OK\nContent-Type: application/json\n")
    print(json.dumps(jobs(producing=True)))
    raise SystemExit
if endpoint == "repos/{owner}/{repo}":
    emit(repo)
if "/pulls/42" in endpoint:
    emit({"number": 42, "state": "open", "merged": False, "merge_commit_sha": config["merge"], "head": {"sha": head, "ref": branch, "repo": repo}, "base": {"sha": base, "ref": base_branch, "repo": repo}})
if "/rules/branches/" in endpoint:
    emit([])
if "/git/commits/" in endpoint:
    revision = endpoint.rsplit("/", 1)[1]
    parents = subprocess.check_output(["git", "show", "-s", "--format=%P", revision], cwd=config["workspace"], text=True).split()
    emit({"sha": revision, "parents": [{"sha": parent} for parent in parents]})
if "/contents/" in endpoint:
    path, revision = endpoint.split("/contents/", 1)[1].split("?ref=", 1)
    value = source(path, revision)
    if config.get("changed_helper") and path.endswith(".py") and revision == head:
        value["sha"] = "f" * 40
    if config.get("changed_workflow") and path.endswith(".yml") and revision == head:
        value["sha"] = "f" * 40
    emit(value)
if "/actions/workflows/" in endpoint:
    emit({"id": 600, "path": ".github/workflows/retry.yml"})
if "/check-suites/" in endpoint:
    emit({"id": 800, "head_sha": head, "app": app, "repository": repo})
if "/check-runs?" in endpoint:
    emit({"total_count": len(names), "check_runs": [{"id": 1100 + index, "head_sha": head, "name": name, "app": app, "check_suite": {"id": 800}, "status": "completed", "conclusion": outcome(name)} for index, name in enumerate(names)]})
if "/status?" in endpoint:
    emit({"sha": head, "repository": repo, "total_count": 0, "statuses": []})
if "/actions/runs" in endpoint:
    is_base = any(arg == f"head_sha={base}" or arg == "event=push" for arg in args) or f"head_sha={base}" in endpoint
    if endpoint.endswith("/jobs") or "/jobs?" in endpoint:
        emit(jobs(is_base="/701/" in endpoint))
    if endpoint.endswith("/700"):
        emit(run())
    emit({"total_count": 1, "workflow_runs": [run(is_base)]})
raise SystemExit(77)
