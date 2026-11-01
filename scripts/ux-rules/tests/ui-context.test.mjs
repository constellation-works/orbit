import assert from "node:assert/strict";
import test from "node:test";

import { failureEvidenceCandidates, uiResultOf } from "../lib/ui-context.mjs";

const rule = { id: "ui.no-horizontal-overflow", severity: "high" };
const target = {
  id: "analytics-implementer-detail|dark|1440|default",
  view: "analytics-implementer-detail",
  width: 1440,
  theme: "dark",
  state: "default",
};

test("failureEvidenceCandidates prefers a unique handle over a shared selector", () => {
  const result = uiResultOf(rule, target, "fail", {
    measurements: {
      failures: [
        { selector: "span.mx-muted", text: "Recorded effort: x", width_px: 140, evidence_selector: '[data-ux-evidence-target="ui.cramped-wrap-0"]' },
        { selector: "span.mx-muted", text: "Other text", width_px: 90 },
        { text: "no selector" },
        null,
      ],
    },
  });
  assert.deepEqual(failureEvidenceCandidates(result), [
    { evidence_selector: '[data-ux-evidence-target="ui.cramped-wrap-0"]' },
    { selector: "span.mx-muted", text: "Other text", width_px: 90 },
  ]);
  assert.deepEqual(failureEvidenceCandidates(uiResultOf(rule, target, "pass")), []);
  assert.equal(result.fingerprint, "ui.no-horizontal-overflow|analytics-implementer-detail|dark|1440|default");
});

test("uiResultOf keeps selector in measurements, not the aggregate fingerprint", () => {
  const fail = uiResultOf(rule, target, "fail", { selector: "div.wide", measurements: { excess_px: 12 } });
  const pass = uiResultOf(rule, target, "pass", { measurements: { excess_px: 0 } });
  const expected = "ui.no-horizontal-overflow|analytics-implementer-detail|dark|1440|default";
  assert.equal(fail.fingerprint, expected);
  assert.equal(pass.fingerprint, expected);
  assert.equal(fail.measurements.selector, "div.wide");
  assert.equal(fail.measurements.excess_px, 12);
});
