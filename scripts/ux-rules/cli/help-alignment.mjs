// Sibling descriptions in grouped help share one column, with a two-space gap.

import { inspectGroupedHelp, resultOf } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.help-alignment",
  title: "Grouped help descriptions start in one column",
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
  const failedSections = [];
  const excerpts = [];
  for (const flag of FLAGS) {
    for (const width of [...WIDTHS, null]) {
      const captured = await ctx.capture({
        argv: [...target.argv, flag],
        width,
      });
      if (captured.exitCode !== 0) {
        return resultOf(rule, target, "error", {
          selector: "throw",
          measurements: {
            flag,
            width,
            exit_code: captured.exitCode,
            stderr: captured.stderr.slice(0, 500),
          },
        });
      }
      const rendererWidth = width === null ? 0 : captured.rendererWidth;
      const sections = inspectGroupedHelp(captured.stdout, rendererWidth);
      if (sections.length === 0) {
        return resultOf(rule, target, "error", {
          selector: "matched no target",
          measurements: { flag, requested_width: width, renderer_width: rendererWidth },
        });
      }
      const bad = sections.filter((section) => !section.aligned);
      for (const section of bad) {
        failedSections.push(`${flag}:${rendererWidth}:${section.name}`);
      }
      captures.push({
        flag,
        requested_width: width,
        renderer_width: rendererWidth,
        mode: captured.mode,
        columns_cleared: true,
        sections: sections.map((section) => ({
          name: section.name,
          column: section.column,
          min_gap: section.minGap,
          aligned: section.aligned,
        })),
      });
      excerpts.push(
        [
          `flag=${flag} requested=${width ?? "piped"} renderer=${rendererWidth} mode=${captured.mode}`,
          ...sections.map(
            (section) =>
              `${section.name} column=${section.column} min_gap=${section.minGap} aligned=${section.aligned}`,
          ),
        ].join("\n"),
      );
    }
  }
  const evidence = [
    await ctx.writeEvidence(rule.id, target.id, "widths", `${excerpts.join("\n\n")}\n`),
  ];
  const selector = failedSections.length > 0 ? [...new Set(failedSections)].sort().join(",") : "";
  return resultOf(rule, target, failedSections.length > 0 ? "fail" : "pass", {
    selector: selector || undefined,
    measurements: { captures },
    evidence,
  });
}
