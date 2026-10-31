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

// The browser kit is optional for the ordinary Node suite. Set PLAYWRIGHT_MODULE
// and PLAYWRIGHT_BROWSERS_PATH from its env.sh to run the measurement regression.
test("text-overflow compares the overflow with real clip boxes", { skip: !process.env.PLAYWRIGHT_MODULE }, async () => {
  const { chromium } = await import(process.env.PLAYWRIGHT_MODULE);
  const browser = await chromium.launch({ headless: true });
  try {
    const page = await browser.newPage();
    const measure = async (html) => {
      await page.setContent(html);
      return check({ page, target });
    };
    const label = '<div id="label" style="width:50px;white-space:nowrap">Readable long label</div>';
    const wide = await measure(`<div style="width:600px;overflow:hidden">${label}</div>`);
    assert.equal(wide.status, "pass");

    const narrow = await measure(`<div style="width:100px;overflow:hidden">${label}</div>`);
    assert.equal(narrow.status, "fail");
    assert.equal(narrow.measurements.failures.find((failure) => failure.selector === "#label").clipped, true);

    const sibling = await measure(`<div style="display:flex">${label}<div id="sibling" style="width:80px">Other</div></div>`);
    assert.equal(sibling.status, "fail");
    assert.equal(sibling.measurements.failures.find((failure) => failure.selector === "#label").sibling, "#sibling");

    const own = await measure('<div id="label" style="width:50px;white-space:nowrap;overflow:hidden">Readable long label</div>');
    assert.equal(own.status, "fail");
    assert.equal(own.measurements.failures[0].clipped, true);
  } finally {
    await browser.close();
  }
});
