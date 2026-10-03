// Usage: node task-panel-layout-browser.mjs /path/to/playwright/index.mjs /evidence/directory
// Renders the actual task-panel presentation module and stylesheet in Chromium.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [playwrightPath, evidencePath] = process.argv.slice(2);
if (!playwrightPath || !evidencePath) {
  throw new Error('Usage: node task-panel-layout-browser.mjs /path/to/playwright/index.mjs /evidence/directory');
}

const { chromium } = await import(pathToFileURL(path.resolve(playwrightPath)).href);
const assets = new URL('../../../assets/task-panel/', import.meta.url);
const dashboard = new URL('../../../../orbit-web/assets/dashboard/', import.meta.url);
const read = (base, file) => fs.readFileSync(new URL(file, base), 'utf8');
const css = read(assets, 'css/task-panel.css');
const longNote = 'History note that must wrap at narrow widths: ' + 'unbroken-value-'.repeat(12);
const longError = 'Step detail that must stay readable: ' + 'unbroken-detail-'.repeat(12);
const history = [
  {
    at: '2026-09-10T01:26:01.052932Z',
    by: 'system',
    event: 'created',
    to_status: 'backlog',
    note: longNote,
  },
  {
    at: '2026-09-10T01:31:01.052932Z',
    by: 'human:daniel',
    event: 'status_changed',
    from_status: 'backlog',
    to_status: 'in-progress',
  },
];
const steps = [
  {
    step_index: 0,
    target: 'delivery',
    state: 'success',
    started_at: '2026-10-03T13:15:00Z',
    finished_at: '2026-10-03T13:15:02Z',
    duration_ms: 125000,
    error_message: longError,
  },
  {
    step_index: 1,
    target: 'wait_for_window',
    state: 'running',
    started_at: '2026-10-03T13:16:00Z',
    duration_ms: 0,
  },
];

fs.mkdirSync(path.resolve(evidencePath), { recursive: true });
const browser = await chromium.launch({
  headless: true,
  executablePath: process.env.ORBIT_CHROMIUM_PATH || undefined,
});
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  await page.setContent(`<!doctype html><html><head><meta charset="utf-8"><style>
    ${css}
    body { margin: 0; }
    #panel { width: min(440px, calc(100vw - 24px)); margin: 24px auto; padding: 16px; }
  </style></head><body><main id="panel"><div id="details"></div></main></body></html>`);
  await page.addScriptTag({ content: read(dashboard, 'vendor/marked.umd.js') });
  await page.addScriptTag({ content: read(dashboard, 'vendor/purify.min.js') });
  await page.addScriptTag({ content: read(assets, 'js/presentation.js') });
  await page.evaluate(({ historyData, stepData }) => {
    const view = window.OrbitPanelView;
    const container = document.getElementById('details');
    view.field(container, 'History', historyData, { technical: true });
    view.field(container, 'Steps', stepData);
  }, { historyData: history, stepData: steps });

  const summary = page.locator('details.field > summary');
  await summary.focus();
  await page.keyboard.press('Enter');
  assert.equal(await page.locator('details.field').evaluate(node => node.open), true,
    'History disclosure opens from the keyboard');
  await page.keyboard.press('Enter');
  assert.equal(await page.locator('details.field').evaluate(node => node.open), false,
    'History disclosure closes from the keyboard');
  await page.keyboard.press('Enter');
  await summary.evaluate(node => node.blur());

  const inspect = () => {
    const fields = [...document.querySelectorAll('#details > .field')];
    const historyField = fields.find(field => field.querySelector('summary')?.textContent === 'History');
    const stepsField = fields.find(field => field.querySelector('h3')?.textContent === 'Steps');
    const inspectField = field => {
      const list = field.querySelector('.value-list');
      const items = [...list.children];
      const entries = items.map(item => {
        const definitionList = item.querySelector(':scope > .property-list');
        const properties = [...definitionList.children];
        const pairs = [];
        for (let index = 0; index < properties.length; index += 2) {
          const label = properties[index];
          const value = properties[index + 1];
          const labelBox = label.getBoundingClientRect();
          const valueBox = value.getBoundingClientRect();
          pairs.push({
            label: label.textContent,
            aligned: Math.abs(labelBox.top - valueBox.top) < 2,
            separated: labelBox.right <= valueBox.left + 1,
          });
        }
        const style = getComputedStyle(definitionList);
        return {
          text: item.textContent,
          pairs,
          rowGap: parseFloat(style.rowGap),
          markdownMargins: [...definitionList.querySelectorAll('dd.markdown > :last-child')]
            .map(node => parseFloat(getComputedStyle(node).marginBottom)),
          overflowingValues: [...definitionList.querySelectorAll('dd')]
            .filter(node => node.scrollWidth > node.clientWidth + 1).length,
        };
      });
      const boxes = items.map(item => item.getBoundingClientRect());
      return {
        entries,
        entryGaps: boxes.slice(1).map((box, index) => box.top - boxes[index].bottom),
        width: field.getBoundingClientRect().width,
        scrollWidth: field.scrollWidth,
        clientWidth: field.clientWidth,
      };
    };
    const text = document.getElementById('panel').innerText;
    return {
      history: inspectField(historyField),
      steps: inspectField(stepsField),
      panelWidth: document.getElementById('panel').clientWidth,
      panelScrollWidth: document.getElementById('panel').scrollWidth,
      pageScrollWidth: document.documentElement.scrollWidth,
      hasSuccessBadge: Boolean(document.querySelector('.field h3 + .field-body .badge[data-status="success"]')),
      hasDuration: text.includes('2m 5s'),
      renderedText: text,
      historyTimestampVisible: text.includes('2026-09-10T01:26:01.052932Z'),
      stepTimestampTitle: [...stepsField.querySelectorAll('dd')].some(dd => dd.title === '2026-10-03T13:15:00Z'),
    };
  };

  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 1000 });
    const layout = await page.evaluate(inspect);
    for (const fieldName of ['history', 'steps']) {
      const field = layout[fieldName];
      assert.equal(field.entries.length, 2, `${fieldName} renders both structured entries at ${width}px`);
      assert.ok(field.entries.every(entry => entry.pairs.every(pair => pair.aligned && pair.separated)),
        `${fieldName} labels and values share compact rows at ${width}px`);
      assert.ok(field.entries.every(entry => entry.rowGap <= 3),
        `${fieldName} key/value rows have compact vertical spacing at ${width}px`);
      assert.ok(field.entryGaps.every(gap => gap >= 4 && gap <= 8),
        `${fieldName} entries stay distinct with compact separation at ${width}px: ${field.entryGaps}`);
      assert.ok(field.entries.every(entry => entry.markdownMargins.every(margin => margin === 0)),
        `${fieldName} Markdown values do not add blank space after their final paragraph at ${width}px`);
      assert.ok(field.entries[0].markdownMargins.length > 0,
        `${fieldName} long primitive value is rendered through Markdown at ${width}px`);
      assert.ok(field.entries.every(entry => entry.overflowingValues === 0),
        `${fieldName} values fit their cells at ${width}px`);
      assert.ok(field.scrollWidth <= field.clientWidth + 1,
        `${fieldName} has no horizontal overflow at ${width}px`);
    }
    assert.ok(layout.panelScrollWidth <= layout.panelWidth + 1 && layout.pageScrollWidth <= width + 1,
      `Task panel does not clip or widen the page at ${width}px`);
    assert.ok(layout.hasSuccessBadge, `Steps retains its success status badge at ${width}px`);
    assert.ok(layout.hasDuration, `Steps retains formatted duration at ${width}px`);
    assert.ok(layout.renderedText.includes(longNote) && layout.renderedText.includes(longError),
      `Long History and Steps values remain complete at ${width}px`);
    assert.ok(layout.historyTimestampVisible && layout.stepTimestampTitle, `History and Steps timestamps remain readable at ${width}px`);
    await page.screenshot({ path: path.join(path.resolve(evidencePath), `task-panel-layout-${width}.png`), fullPage: true });
    console.log(JSON.stringify({ width, layout }));
  }
} finally {
  await browser.close();
}
