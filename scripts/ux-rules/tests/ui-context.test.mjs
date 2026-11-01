import assert from "node:assert/strict";
import test from "node:test";

import { uiResultOf } from "../lib/ui-context.mjs";

const rule = { id: "ui.no-horizontal-overflow", severity: "high" };
const target = {
  id: "analytics-implementer-detail|dark|1440|default",
  view: "analytics-implementer-detail",
  width: 1440,
  theme: "dark",
  state: "default",
};

test("uiResultOf keeps selector in measurements, not the aggregate fingerprint", () => {
  const fail = uiResultOf(rule, target, "fail", { selector: "div.wide", measurements: { excess_px: 12 } });
  const pass = uiResultOf(rule, target, "pass", { measurements: { excess_px: 0 } });
  const expected = "ui.no-horizontal-overflow|analytics-implementer-detail|dark|1440|default";
  assert.equal(fail.fingerprint, expected);
  assert.equal(pass.fingerprint, expected);
  assert.equal(fail.measurements.selector, "div.wide");
  assert.equal(fail.measurements.excess_px, 12);
});
