import assert from "node:assert/strict";
import test from "node:test";

import { check, MIN_INSET_PX } from "../ui/cramped-padding.mjs";

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

test("cramped-padding fails when text touches its container edge", async () => {
  const result = await check(
    context({
      matched: 5,
      failures: [
        {
          selector: "p.note",
          container: "div.panel",
          inset_px: 0,
          text: "Counts sit on the border",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
  assert.equal(MIN_INSET_PX, 4);
});

test("cramped-padding passes when every run keeps its inset", async () => {
  const result = await check(context({ matched: 5, failures: [] }));
  assert.equal(result.status, "pass");
});

test("cramped-padding skips when no text matched", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});
