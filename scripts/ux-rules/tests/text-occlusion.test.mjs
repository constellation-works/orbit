import assert from "node:assert/strict";
import test from "node:test";

import { check, rule } from "../ui/text-occlusion.mjs";

const target = {
  id: "analytics-implementers|dark|1440|default",
  view: "analytics-implementers",
  width: 1440,
  theme: "dark",
  state: "default",
};

function context(measured) {
  return {
    page: { evaluate: async () => measured },
    target,
  };
}

test("text-occlusion fails when a value paints over its label", async () => {
  const result = await check(
    context({
      matched: 2,
      failures: [
        {
          selector: "dt.cost",
          covered_by: "dd.cost-value",
          point_x_px: 120,
          point_y_px: 300,
          text: "Cost coverage",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
  assert.equal(result.rule, rule.id);
  assert.match(result.fingerprint, /ui\.text-occlusion\|analytics-implementers/);
});

test("text-occlusion passes when nothing is covered", async () => {
  const result = await check(context({ matched: 4, failures: [] }));
  assert.equal(result.status, "pass");
});

test("text-occlusion skips when no text matched", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});
