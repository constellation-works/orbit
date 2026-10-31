import assert from "node:assert/strict";
import test from "node:test";

import { check, MIN_FONT_PX } from "../ui/undersized-text.mjs";

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

test("undersized-text fails when content text drops below 12px", async () => {
  const result = await check(
    context({
      matched: 3,
      failures: [
        {
          selector: "p.meta",
          font_size_px: 10,
          overline: false,
          text: "Meta copy readers must read",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
  assert.equal(MIN_FONT_PX, 11);
});

test("undersized-text passes when content meets the floor", async () => {
  const result = await check(context({ matched: 3, failures: [] }));
  assert.equal(result.status, "pass");
});

test("undersized-text skips when no content text matched", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});
