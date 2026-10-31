// Piped flag help stays on one line, and long help stays inside a byte budget.

import { flagEntries, resultOf, utf8Bytes } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.help-length",
  title: "Flag help is one line and long help stays within a byte budget",
  severity: "low",
  surface: "cli",
};

// A command whose piped --help exceeds this is a verbosity finding.
// The current surface sits near 3 KB; 8 KB leaves room for a real command
// and still catches a help page that has absorbed a runbook.
const LONG_HELP_BUDGET = 8192;

export async function check(ctx) {
  const commands = await ctx.discoverCommands();
  const results = [];
  for (const argv of commands) {
    const target = { id: argv.length === 0 ? "orbit" : `orbit ${argv.join(" ")}`, argv };
    const captured = await ctx.capture({ argv: [...argv, "--help"] });
    if (captured.exitCode !== 0) {
      results.push(
        resultOf(rule, target, "error", {
          selector: "throw",
          measurements: { exit_code: captured.exitCode, stderr: captured.stderr.slice(0, 400) },
        }),
      );
      continue;
    }
    const bytes = utf8Bytes(captured.stdout);
    const wrapped = flagEntries(captured.stdout).filter((entry) => entry.lines.length > 1);
    const overBudget = bytes > LONG_HELP_BUDGET;
    const failed = wrapped.length > 0 || overBudget;
    const selector = failed
      ? [
          ...wrapped.map((entry) => entry.flag),
          ...(overBudget ? ["long-help"] : []),
        ]
          .sort()
          .join(",")
      : "";
    results.push(
      resultOf(rule, target, failed ? "fail" : "pass", {
        selector,
        measurements: {
          long_help_bytes: bytes,
          long_help_budget: LONG_HELP_BUDGET,
          wrapped_flags: wrapped.map((entry) => entry.flag),
        },
      }),
    );
  }
  return results;
}
