// Wrapped grouped-help descriptions keep one hanging indent and no hand breaks.

import { inspectGroupedHelp, resultOf } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.help-hanging-indent",
  title: "Wrapped help keeps one hanging indent and no hand-inserted breaks",
  severity: "medium",
  surface: "cli",
  targets: [
    { id: "orbit", argv: [] },
    { id: "orbit run", argv: ["run"] },
    { id: "orbit task", argv: ["task"] },
  ],
};

const WIDTHS = [60, 80, 120];
const FLAGS = ["-h", "--help"];

export async function check(ctx) {
  const target = ctx.target;
  const captures = [];
  const failed = [];
  for (const flag of FLAGS) {
    for (const width of [...WIDTHS, null]) {
      const captured = await ctx.capture({
        argv: [...target.argv, flag],
        width,
      });
      if (captured.exitCode !== 0) {
        return resultOf(rule, target, "error", {
          selector: "throw",
          measurements: { flag, exit_code: captured.exitCode, stderr: captured.stderr.slice(0, 500) },
        });
      }
      const rendererWidth = width === null ? 0 : captured.rendererWidth;
      const sections = inspectGroupedHelp(captured.stdout, rendererWidth);
      if (sections.length === 0) {
        return resultOf(rule, target, "error", {
          selector: "matched no target",
          measurements: { flag, renderer_width: rendererWidth },
        });
      }
      for (const section of sections) {
        if (!section.hangingOk || section.handBreaks.length > 0) {
          failed.push(
            `${flag}:${rendererWidth}:${section.name}:${section.handBreaks.join("+") || "indent"}`,
          );
        }
      }
      captures.push({
        flag,
        requested_width: width,
        renderer_width: rendererWidth,
        mode: captured.mode,
        sections: sections.map((section) => ({
          name: section.name,
          hanging_ok: section.hangingOk,
          hand_breaks: section.handBreaks,
        })),
      });
    }
  }
  const selector = failed.length > 0 ? [...new Set(failed)].sort().join(",") : "";
  return resultOf(rule, target, failed.length > 0 ? "fail" : "pass", {
    selector: selector || undefined,
    measurements: { captures },
  });
}
