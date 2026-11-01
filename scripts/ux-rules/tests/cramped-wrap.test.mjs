import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../ui/cramped-wrap.mjs";

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

test("cramped-wrap fails when a short label stacks one word per line", async () => {
  const result = await check(
    context({
      matched: 3,
      failures: [
        {
          selector: "dt.cost",
          words: 2,
          lines: 2,
          words_per_line: 1,
          width_px: 40,
          text: "Cost coverage",
        },
      ],
    }),
  );
  assert.equal(result.status, "fail");
});

test("cramped-wrap passes when short labels share one line", async () => {
  const result = await check(context({ matched: 3, failures: [] }));
  assert.equal(result.status, "pass");
});

test("cramped-wrap skips when no short label matched", async () => {
  const result = await check(context({ matched: 0, failures: [] }));
  assert.equal(result.status, "skipped");
});

test("cramped-wrap fingerprint is stable across fail, first-selector change and pass", async () => {
  const fail = await check(
    context({ matched: 3, failures: [{ selector: "dt.cost", words: 2, lines: 2, words_per_line: 1, width_px: 40, text: "Cost coverage" }] }),
  );
  const failOther = await check(
    context({ matched: 3, failures: [{ selector: "span.other", words: 2, lines: 2, words_per_line: 1, width_px: 40, text: "Other label" }] }),
  );
  const pass = await check(context({ matched: 3, failures: [] }));
  const expected = "ui.cramped-wrap|analytics-implementers|dark|1440|default";
  assert.equal(fail.fingerprint, expected);
  assert.equal(failOther.fingerprint, expected);
  assert.equal(pass.fingerprint, expected);
  assert.equal(fail.measurements.failures[0].selector, "dt.cost");
  assert.equal(failOther.measurements.failures[0].selector, "span.other");
});
