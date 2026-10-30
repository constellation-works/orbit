import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../ui/forecast-unavailable-reason.mjs";

function context(unavailable, domReasons) {
  return {
    target: {
      id: "tasks-list|dark|1440|default",
      view: "tasks-list",
      theme: "dark",
      width: 1440,
      state: "default",
    },
    page: {
      async evaluate(script) {
        if (typeof script === "function") {
          return { status: 200, json: { forecast: { unavailable } } };
        }
        return domReasons;
      },
    },
  };
}

test("fails when visible DOM text names a different known reason than the API", async () => {
  const result = await check(context(
    { regression_history_missing: true },
    [{ reason: "empty", text: "No backlog items" }],
  ));

  assert.equal(result.status, "fail");
});

test("passes when visible DOM text names an API-reported reason", async () => {
  const result = await check(context(
    { regression_history_missing: true },
    [{ reason: "regression_history_missing", text: "Regression history is missing" }],
  ));

  assert.equal(result.status, "pass");
});

test("fails when the DOM reason has no visible text", async () => {
  const result = await check(context(
    { regression_history_missing: true },
    [{ reason: "regression_history_missing", text: "" }],
  ));

  assert.equal(result.status, "fail");
});

test("passes when the API reports no unavailable reasons", async () => {
  const result = await check(context(
    {},
    [{ reason: "empty", text: "No backlog items" }],
  ));

  assert.equal(result.status, "pass");
});

test("passes a matching reason alongside the live regression-history counts payload", async () => {
  const result = await check(context(
    { regression_history_missing: true, regression_history_gaps: { count: 1 } },
    [{ reason: "regression_history_missing", text: "Regression history is missing" }],
  ));

  assert.equal(result.status, "pass");
});
