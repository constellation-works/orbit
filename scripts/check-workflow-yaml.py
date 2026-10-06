#!/usr/bin/env python3
"""Parse GitHub Actions workflows and guard the Homebrew shell boundary.

GitHub creates no jobs for a workflow it cannot parse. A path-filtered leg
such as the macOS sandbox job then disappears from the PR checks instead of
failing, so a syntax error there stays silent until someone notices red on
the branch tip (a trailing `generation_root::` once disabled it for every
merge).

PyYAML is required under GitHub Actions; a local run without it warns and
skips, mirroring the soft-presence of the other optional guardrail tools.
"""

from pathlib import Path
import os
import sys

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ROOT / ".github" / "workflows"


def main() -> int:
    try:
        import yaml
    except ImportError:
        if os.environ.get("GITHUB_ACTIONS") == "true":
            print("check-workflow-yaml: PyYAML is required in CI (python3 -m pip install pyyaml)",
                  file=sys.stderr)
            return 1
        print("check-workflow-yaml: PyYAML not installed; skipping workflow parse "
              "(install: python3 -m pip install pyyaml)", file=sys.stderr)
        return 0

    workflows = sorted(
        path for pattern in ("*.yml", "*.yaml") for path in WORKFLOWS.glob(pattern)
    )
    errors = []
    for path in workflows:
        relative = path.relative_to(ROOT)
        try:
            document = yaml.safe_load(path.read_text(encoding="utf-8"))
        except yaml.YAMLError as error:
            mark = getattr(error, "problem_mark", None)
            location = f"{relative}:{mark.line + 1}" if mark is not None else str(relative)
            problem = getattr(error, "problem", None) or str(error)
            errors.append(f"{location}: invalid YAML: {problem}")
            continue
        if not isinstance(document, dict) or not isinstance(document.get("jobs"), dict):
            errors.append(f"{relative}: a workflow must be a mapping with a `jobs` mapping")
            continue
        # Tag-derived metadata must enter Bash as environment data. GitHub
        # expressions in script source are substituted before Bash parses it.
        tap = document["jobs"].get("bump-homebrew-tap", {})
        for index, step in enumerate(tap.get("steps", []), start=1):
            run = step.get("run", "")
            if isinstance(run, str) and "${{" in run:
                errors.append(
                    f"{relative}: bump-homebrew-tap step {index}: GitHub expressions "
                    "in run scripts allow shell injection; pass values through env instead"
                )

    for error in errors:
        print(f"check-workflow-yaml: {error}", file=sys.stderr)
    if errors:
        return 1
    print(f"check-workflow-yaml: {len(workflows)} workflow files parsed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
