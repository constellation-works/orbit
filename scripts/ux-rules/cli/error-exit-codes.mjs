// Common mistakes exit non-zero and tell the operator what to do next.

import { resultOf } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.error-exit-codes",
  title: "Common mistakes exit non-zero and name a next step",
  severity: "medium",
  surface: "cli",
  targets: [
    { id: "unknown-task-id", argv: ["task", "show", "ZZUX-99999"] },
    { id: "bad-flag", argv: ["task", "list", "--not-a-real-flag"] },
    { id: "conflicting-flags", argv: ["run", "history", "--failures", "--incidents"] },
  ],
};

const NEXT_STEP = /try ['"]--help['"]|For more information|Usage: orbit |pass --|rerun |see [`']orbit/i;

export async function check(ctx) {
  const target = ctx.target;
  const captured = await ctx.capture({ argv: target.argv });
  const text = `${captured.stderr}\n${captured.stdout}`;
  const nonzero = captured.exitCode !== 0;
  const nextStep = NEXT_STEP.test(text);
  const failed = !nonzero || !nextStep;
  return resultOf(rule, target, failed ? "fail" : "pass", {
    selector: failed ? target.id : "",
    measurements: {
      exit_code: captured.exitCode,
      next_step: nextStep,
      excerpt: text.trim().slice(0, 500),
    },
  });
}
