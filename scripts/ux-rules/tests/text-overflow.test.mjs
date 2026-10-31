import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../ui/text-overflow.mjs";

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

test("text-overflow fails when a clipped label runs past its box", async () => {
  const result = await check(
    context({
      matched: 1,
      failures: [
        {
          selector: "dt.cost",
          scroll_width_px: 120,
          client_width_px: 40,
          excess_px: 80,
          clipped: false,
          sibling: "dd.cost-value",
          text: "Cost coverage",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
});

test("text-overflow passes when overflow stays inside scroll wrappers", async () => {
  const result = await check(context({ matched: 2, failures: [] }));
  assert.equal(result.status, "pass");
});

test("text-overflow skips when nothing overflows", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});
