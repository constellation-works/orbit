import assert from "node:assert/strict";
import test from "node:test";

import { check } from "../ui/nested-card.mjs";

const target = {
  id: "dashboard|dark|1440|default",
  view: "dashboard",
  width: 1440,
  theme: "dark",
  state: "default",
};

async function checkChildren(children) {
  return check({
    page: { evaluate: async () => children },
    target,
  });
}

test("nested-card reports bordered and filled direct children while retaining exemptions", async (t) => {
  await t.test("filled-only child fails", async () => {
    const result = await checkChildren([
      {
        selector: "div.filled-child",
        tag: "DIV",
        bordered: false,
        filled: true,
        role: "",
        labelled: false,
      },
    ]);

    assert.equal(result.status, "fail");
    assert.deepEqual(result.measurements.failures.map((item) => item.selector), ["div.filled-child"]);
  });

  await t.test("bordered child still fails", async () => {
    const result = await checkChildren([
      {
        selector: "div.bordered-child",
        tag: "DIV",
        bordered: true,
        filled: false,
        role: "",
        labelled: false,
      },
    ]);

    assert.equal(result.status, "fail");
  });

  await t.test("allowed tag still passes", async () => {
    const result = await checkChildren([
      {
        selector: "table.nested",
        tag: "TABLE",
        bordered: true,
        filled: true,
        role: "",
        labelled: false,
      },
    ]);

    assert.equal(result.status, "pass");
  });

  await t.test("grouping role still passes", async () => {
    const result = await checkChildren([
      {
        selector: "div.group",
        tag: "DIV",
        bordered: true,
        filled: true,
        role: "group",
        labelled: false,
      },
    ]);

    assert.equal(result.status, "pass");
  });

  await t.test("accessible label still passes", async () => {
    const result = await checkChildren([
      {
        selector: "div.labelled",
        tag: "DIV",
        bordered: true,
        filled: true,
        role: "",
        labelled: true,
      },
    ]);

    assert.equal(result.status, "pass");
  });
});
