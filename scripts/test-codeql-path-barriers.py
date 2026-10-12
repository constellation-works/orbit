#!/usr/bin/env python3
"""Exercise source-available Rust barriers with real CodeQL (Linux, opt-in).

The generated validators deliberately return unchecked input. Only the model
can stop flow; an explicit remote source avoids relying on inactive CLI sources.
The fixture is extracted in its own Cargo workspace, never the production tree.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
SUITE = "codeql/rust-queries:codeql-suites/rust-security-extended.qls"
OK_FIELD = "ReturnValue.Field[core::result::Result::Ok(0)]"
# name, return type, unchecked return expression, extraction, model, alert
CASES = [
    ("unmodelled", "io::Result<PathBuf>", "Ok(path.to_path_buf())",
     "let path = admitted(Path::new(&input))?; std::fs::read(path)?;", None, True),
    ("result_field", "io::Result<PathBuf>", "Ok(path.to_path_buf())",
     "let path = admitted(Path::new(&input))?; std::fs::read(path)?;", OK_FIELD, True),
    ("result_field_match", "io::Result<PathBuf>", "Ok(path.to_path_buf())",
     "if let Ok(path) = admitted(Path::new(&input)) { std::fs::read(path)?; }",
     OK_FIELD, True),
    ("result_return", "io::Result<PathBuf>", "Ok(path.to_path_buf())",
     "let path = admitted(Path::new(&input))?; std::fs::read(path)?;", "ReturnValue", False),
    ("option_return", "Option<PathBuf>", "Some(path.to_path_buf())",
     "if let Some(path) = admitted(Path::new(&input)) { std::fs::read(path)?; }",
     "ReturnValue", False),
    ("nested_field", "io::Result<Option<PathBuf>>", "Ok(Some(path.to_path_buf()))",
     "if let Some(path) = admitted(Path::new(&input))? { std::fs::read(path)?; }",
     OK_FIELD + ".Field[core::option::Option::Some(0)]", True),
    ("nested_return", "io::Result<Option<PathBuf>>", "Ok(Some(path.to_path_buf()))",
     "if let Some(path) = admitted(Path::new(&input))? { std::fs::read(path)?; }",
     "ReturnValue", False),
]


def main() -> None:
    scratch = ROOT / ".orbit/tmp"
    scratch.mkdir(parents=True, exist_ok=True)
    fixture = Path(tempfile.mkdtemp(prefix="codeql-path-barriers-", dir=scratch))
    (fixture / "src").mkdir()
    (fixture / "scripts").mkdir()
    models = fixture / ".github/codeql/extensions/barriers"
    models.mkdir(parents=True)
    shutil.copy2(ROOT / "scripts/codeql-rust-local.sh", fixture / "scripts")
    (fixture / "Cargo.toml").write_text(
        '[package]\nname = "orbit_barrier_probe"\nversion = "0.0.0"\n'
        'edition = "2021"\n\n[workspace]\n', encoding="utf-8")
    (fixture / ".github/codeql/codeql-config.yml").write_text(
        'name: barrier-probe\npaths-ignore:\n  - ".orbit/**"\n  - "target/**"\n',
        encoding="utf-8")
    (models / "codeql-pack.yml").write_text(
        'name: constellation-works/orbit-barrier-probe\nversion: 0.0.1\n'
        'library: true\nextensionTargets:\n  codeql/rust-all: "*"\n'
        'dataExtensions:\n  - barriers.yaml\n', encoding="utf-8")
    rows = []
    for name, return_type, value, extraction, model, _ in CASES:
        (fixture / f"src/{name}.rs").write_text(
            'use std::path::{Path, PathBuf};\nuse std::io;\n'
            f'fn admitted(path: &Path) -> {return_type} {{ {value} }}\n'
            'pub(crate) fn exercise() -> io::Result<()> {\n'
            '    let input = crate::tainted_input();\n'
            f'    {extraction}\n    Ok(())\n}}\n', encoding="utf-8")
        if model:
            rows.append(f'      - ["orbit_barrier_probe::{name}::admitted", '
                        f'"{model}", "path-injection", "manual"]')
    (fixture / "src/main.rs").write_text(
        ''.join(f'mod {case[0]};\n' for case in CASES)
        + 'fn tainted_input() -> String {\n'
        + '    std::env::var("ORBIT_CODEQL_PROBE").unwrap_or_default()\n}\n'
        + 'fn main() -> std::io::Result<()> {\n'
        + ''.join(f'    {case[0]}::exercise()?;\n' for case in CASES)
        + '    Ok(())\n}\n', encoding="utf-8")
    (models / "barriers.yaml").write_text(
        'extensions:\n  - addsTo:\n      pack: codeql/rust-all\n'
        '      extensible: barrierModel\n    data:\n' + '\n'.join(rows)
        + '\n  - addsTo:\n      pack: codeql/rust-all\n'
        + '      extensible: sourceModel\n    data:\n'
        + '      - ["orbit_barrier_probe::tainted_input", "ReturnValue", "remote", "manual"]\n',
        encoding="utf-8")
    environment = dict(os.environ, ORBIT_SCRATCH_DIR=str(fixture / ".orbit/tmp"))
    command = [str(fixture / "scripts/codeql-rust-local.sh"), "--ram", "16384", SUITE]
    log = fixture / "runner.log"
    print(f"CodeQL barrier fixture: {fixture}", flush=True)
    with log.open("w", encoding="utf-8") as output:
        completed = subprocess.run(command, cwd=fixture, env=environment,
                                   stdout=output, stderr=subprocess.STDOUT, timeout=10800)
    print(log.read_text(encoding="utf-8"), end="", flush=True)
    completed.check_returncode()
    runs = list((fixture / ".orbit/tmp").glob("codeql-rust-local.*"))
    if len(runs) != 1:
        raise RuntimeError(f"expected exactly one CodeQL run, got {runs}")
    sarif = json.loads((runs[0] / "results.sarif").read_text(encoding="utf-8"))
    results = [result for run in sarif["runs"] for result in run.get("results", [])]
    findings = [result for result in results if result["ruleId"] == "rust/path-injection"]
    actual = sorted(location["physicalLocation"]["artifactLocation"]["uri"]
                    for result in findings for location in result["locations"])
    expected = sorted(f"src/{name}.rs" for name, *_, alert in CASES if alert)
    report = {
        "schema_version": 1,
        "codeql_version": subprocess.check_output(
            ["codeql", "version", "--format=terse"], text=True).strip(),
        "query": SUITE,
        "run_directory": str(runs[0].relative_to(ROOT)),
        "expected_alert_locations": expected,
        "actual_alert_locations": actual,
        "other_rule_ids": sorted({r["ruleId"] for r in results if r not in findings}),
        "passed": actual == expected and len(results) == len(findings),
    }
    (fixture / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if not report["passed"]:
        raise RuntimeError(f"barrier behavior changed: {report}")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
