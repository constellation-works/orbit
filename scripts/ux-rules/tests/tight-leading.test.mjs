import assert from "node:assert/strict";
import test from "node:test";

import { check, MIN_LEADING } from "../ui/tight-leading.mjs";

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

test("tight-leading fails when multi-line text runs under 1.3x", async () => {
  const result = await check(
    context({
      matched: 2,
      normal_multiline: 0,
      failures: [
        {
          selector: "p.note",
          lines: 3,
          line_height_px: 14,
          font_size_px: 12,
          ratio: 1.17,
          text: "Three lines of cramped copy",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
  assert.equal(MIN_LEADING, 1.3);
});

test("tight-leading passes when multi-line text breathes", async () => {
  const result = await check(
    context({ matched: 2, normal_multiline: 1, failures: [] }),
  );
  assert.equal(result.status, "pass");
});

test("tight-leading skips when no multi-line text matched", async () => {
  const result = await check(
    context({ matched: 0, normal_multiline: 0, failures: [] }),
  );
  assert.equal(result.status, "skipped");
});
