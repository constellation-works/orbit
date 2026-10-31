// --json parses, and a second call keeps the same top-level shape.

import { resultOf, topLevelShape } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.json-valid",
  title: "JSON output parses and keeps its top-level shape",
  severity: "high",
  surface: "cli",
  targets: [
    { id: "task list", argv: ["task", "list"] },
    { id: "workspace list", argv: ["workspace", "list"] },
    { id: "config show", argv: ["config", "show"] },
  ],
};

export async function check(ctx) {
  const target = ctx.target;
  const calls = [];
  for (let attempt = 0; attempt < 2; attempt += 1) {
    const captured = await ctx.capture({
      argv: target.argv,
      json: true,
      fresh: true,
    });
    calls.push(captured);
  }
  const measurements = {
    exit_codes: calls.map((call) => call.exitCode),
  };
  if (calls.some((call) => call.exitCode !== 0)) {
    return resultOf(rule, target, "fail", {
      selector: target.argv.join(" "),
      measurements: {
        ...measurements,
        stderr: calls.map((call) => call.stderr.slice(0, 300)),
      },
    });
  }
  let shapes;
  try {
    shapes = calls.map((call) => topLevelShape(call.stdout));
  } catch (error) {
    return resultOf(rule, target, "fail", {
      selector: target.argv.join(" "),
      measurements: {
        ...measurements,
        parse_error: error instanceof Error ? error.message : String(error),
      },
    });
  }
  const stable = JSON.stringify(shapes[0]) === JSON.stringify(shapes[1]);
  return resultOf(rule, target, stable ? "pass" : "fail", {
    selector: stable ? "" : target.argv.join(" "),
    measurements: { ...measurements, shapes },
  });
}
