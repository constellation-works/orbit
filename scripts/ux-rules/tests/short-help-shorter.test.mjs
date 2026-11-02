import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../cli/short-help-shorter.mjs";

// Shapes taken from `orbit audit list` on orbit 0.43.0, where -h was 1555
// bytes and --help 1514 although both say the same thing (ORB-21634).
const SHORT_HELP = `List audit events

Usage: orbit audit list [OPTIONS]

Options:
      --since <SINCE>                      Filter events since duration or timestamp (e.g. "1h", "90d", RFC3339)
      --status <STATUS>                    Filter by status [possible values: success, failure, denied]
      --process-machine <PROCESS_MACHINE>  Filter by trusted executing-process machine ID
      --limit <LIMIT>                      Maximum number of events to return [default: 100]
  -h, --help                               Print help (see more with '--help')
`;

const LONG_HELP = `List audit events

Usage: orbit audit list [OPTIONS]

Options:
      --since <SINCE>
          Filter events since duration or timestamp (e.g. "1h", "90d", RFC3339)

      --status <STATUS>
          Filter by status

          [possible values: success, failure, denied]

      --process-machine <PROCESS_MACHINE>
          Filter by trusted executing-process machine ID

      --limit <LIMIT>
          Maximum number of events to return

          [default: 100]

  -h, --help
          Print help (see a summary with '-h')
`;

function contextFor(short, long) {
  return {
    async discoverCommands() {
      return [["audit", "list"]];
    },
    async capture({ argv }) {
      return { exitCode: 0, stdout: argv.at(-1) === "-h" ? short : long, stderr: "" };
    },
  };
}

test("layout-only difference is skipped as no long help even when -h has more bytes", async () => {
  assert.ok(Buffer.byteLength(SHORT_HELP) > Buffer.byteLength(LONG_HELP));
  const [result] = await check(contextFor(SHORT_HELP, LONG_HELP));
  assert.equal(result.status, "skipped");
  assert.equal(result.measurements.reason, "no long help");
  assert.equal(result.measurements.missing_from_long, 0);
});

test("long help that adds content passes", async () => {
  const long = LONG_HELP.replace("Filter by status\n", "Filter by status, the outcome of the audited call\n");
  const [result] = await check(contextFor(SHORT_HELP, long));
  assert.equal(result.status, "pass");
  assert.ok(result.measurements.added_by_long > 0);
});

test("short help with content the long help lacks fails", async () => {
  const short = SHORT_HELP.replace("Filter by status ", "Filter by status, newest first ");
  const [result] = await check(contextFor(short, LONG_HELP));
  assert.equal(result.status, "fail");
  assert.equal(result.measurements.missing_from_long, 2);
  assert.equal(result.measurements.missing_sample, "newest first");
});
