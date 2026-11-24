# Development records

Evidence and post-mortems produced while developing Orbit. They describe the tree at a recorded commit or date and do not certify the current one.

- [`examples/`](./examples/release-readiness/README.md) holds current, disabled examples. Repository tests exercise the release-readiness script manifest, but do not validate the routine or auto-task definitions.
- [`qa/`](./qa/README.md) holds QA evidence: command, MCP tool and workflow coverage records, plus the MCP Apps probe and its evidence template.
- [`rca/`](./rca/) holds incident root cause analyses, and the [run failure log](./rca/failure-log.md) of open task-run failure causes (an entry is removed once its fix lands).
- [`reviews/`](./reviews/) holds test-audit reviews: unit-test audits, per-crate admission records and test-budget reports.
