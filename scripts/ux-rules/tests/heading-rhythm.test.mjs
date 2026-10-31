import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../ui/heading-rhythm.mjs";

const target = {
  id: "dashboard|dark|1440|default",
  view: "dashboard",
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

test("heading-rhythm fails when the gap below exceeds the gap above", async () => {
  const result = await check(
    context({
      matched: 2,
      failures: [
        {
          selector: "h4.section",
          above_px: 4,
          below_px: 16,
          text: "Shown, not scored",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
});

test("heading-rhythm passes when headings open their section", async () => {
  const result = await check(context({ matched: 2, failures: [] }));
  assert.equal(result.status, "pass");
});

test("heading-rhythm skips when no heading introduces content", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});
