// MCP tool and field descriptions stay within a byte budget.

import { resultOf, utf8Bytes } from "../lib/cli-context.mjs";

export const rule = {
  id: "cli.mcp-description-length",
  title: "MCP tool and field descriptions stay within a byte budget",
  severity: "low",
  surface: "cli",
};

// Budgets sit above the p90 of the shipped catalog and below a second
// paragraph of policy. Tool p90 is about 700 bytes; one field (task update
// relations) exceeds 1000.
const TOOL_BUDGET = 1000;
const FIELD_BUDGET = 800;

export async function check(ctx) {
  const tools = await ctx.listMcpTools();
  const results = [];
  for (const tool of [...tools].sort((left, right) => left.name.localeCompare(right.name))) {
    const target = { id: tool.name, argv: [tool.name] };
    const toolBytes = utf8Bytes(tool.description ?? "");
    const properties = tool.inputSchema?.properties ?? {};
    const longFields = [];
    for (const [name, spec] of Object.entries(properties)) {
      if (!spec || typeof spec !== "object") {
        continue;
      }
      const bytes = utf8Bytes(spec.description ?? "");
      if (bytes > FIELD_BUDGET) {
        longFields.push({ name, bytes });
      }
    }
    longFields.sort((left, right) => left.name.localeCompare(right.name));
    const toolOver = toolBytes > TOOL_BUDGET;
    const failed = toolOver || longFields.length > 0;
    const selector = failed
      ? [
          ...(toolOver ? [`${tool.name}`] : []),
          ...longFields.map((field) => `${tool.name}.${field.name}`),
        ].join(",")
      : "";
    results.push(
      resultOf(rule, target, failed ? "fail" : "pass", {
        selector,
        measurements: {
          tool_bytes: toolBytes,
          tool_budget: TOOL_BUDGET,
          field_budget: FIELD_BUDGET,
          long_fields: longFields,
        },
      }),
    );
  }
  return results;
}
