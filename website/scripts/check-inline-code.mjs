// Check every built HTML page with loaded fonts and the site's saved theme.
// node website/scripts/check-inline-code.mjs <playwright-module> <site-url> <evidence-dir>
import assert from 'node:assert/strict';
import { mkdir, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { markdownToHtml } from 'satteri';
import { inlineCodeWrap } from '../plugins/inline-code.mjs';

const [playwrightModule, siteURL, evidenceDir] = process.argv.slice(2);
if (!playwrightModule || !siteURL || !evidenceDir) {
  throw new Error('Usage: check-inline-code.mjs <playwright-module> <site-url> <evidence-dir>');
}
const { chromium } = await import(path.resolve(playwrightModule));
await mkdir(evidenceDir, { recursive: true });

async function htmlRoutes(directory, prefix = '') {
  const routes = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const relative = `${prefix}${entry.name}`;
    if (entry.isDirectory()) {
      routes.push(...await htmlRoutes(path.join(directory, entry.name), `${relative}/`));
    } else if (entry.name.endsWith('.html')) {
      routes.push(`/${relative.replace(/index\.html$/, '')}`);
    }
  }
  return routes.sort();
}

// Ranges measure actual glyph positions, including code nested in links or
// raw HTML. Element rectangles alone cannot detect a flag split across lines.
function inspectLayout() {
  const failures = [];
  let codes = 0;
  let wrappedCodes = 0;
  const tolerance = 1;
  const configTable = document.querySelector('table.orbit-config-keys');
  const isConfigPage = location.pathname === '/reference/config/';
  for (const code of document.querySelectorAll('code')) {
    if (code.closest('pre') || !code.getClientRects().length) continue;
    codes++;
    const text = code.textContent;
    let container = code.parentElement;
    while (container && ['inline', 'contents'].includes(getComputedStyle(container).display)) {
      container = container.parentElement;
    }
    const bound = container.getBoundingClientRect();
    const rects = [...code.getClientRects()];
    if (rects.some((rect) => rect.right > bound.right + tolerance || rect.left < bound.left - tolerance)) {
      failures.push({ kind: 'containment', text, container: container.tagName, class: container.className,
        codeRight: Math.max(...rects.map((rect) => rect.right)), containerRight: bound.right });
    }
    if (code.closest('.orbit-pipeline-note') && code.scrollWidth > code.clientWidth) {
      failures.push({ kind: 'pipeline-code-overflow', text,
        clientWidth: code.clientWidth, scrollWidth: code.scrollWidth });
    }
    const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
    let previous;
    let wrapped = false;
    const lineTops = [];
    const isConfigCode = Boolean(code.closest('table.orbit-config-keys'));
    let whitespaceSincePrevious = false;
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      for (let offset = 0; offset < node.length; offset++) {
        const character = node.data[offset];
        if (!isConfigCode && /\s/.test(character)) {
          previous = undefined;
          continue;
        }
        const range = document.createRange();
        range.setStart(node, offset);
        range.setEnd(node, offset + 1);
        if (isConfigCode && /\s/.test(character)) whitespaceSincePrevious = true;
        const rects = [...range.getClientRects()].filter((rect) => rect.width && rect.height);
        for (const rect of rects) {
          if (isConfigCode && !lineTops.some((top) => Math.abs(top - rect.top) <= tolerance)) lineTops.push(rect.top);
          if (previous && Math.abs(previous.top - rect.top) > tolerance) {
            wrapped = true;
            const allowedBreak = isConfigCode
              ? whitespaceSincePrevious || /\s/.test(character) || /[._/=]/.test(previous.character)
              : /[/.=]/.test(previous.character);
            if (!allowedBreak) {
              failures.push({ kind: 'token-break', text, before: previous.character, after: character });
            }
          }
          previous = { character, top: rect.top };
          if (isConfigCode && !/\s/.test(character)) whitespaceSincePrevious = false;
        }
      }
    }
    if (wrapped) wrappedCodes++;
    if (isConfigCode && code.scrollWidth > code.clientWidth) {
      failures.push({ kind: 'config-code-overflow', text,
        clientWidth: code.clientWidth, scrollWidth: code.scrollWidth });
    }
    if (isConfigCode && innerWidth >= 1024 && code.closest('td')?.cellIndex === 0 && lineTops.length > 1) {
      failures.push({ kind: 'config-key-wrapped-at-desktop', text, lines: lineTops.length, width: innerWidth });
    }
  }
  if ((isConfigPage || innerWidth === 375) && document.documentElement.scrollWidth !== innerWidth) {
    failures.push({ kind: 'page-overflow', scrollWidth: document.documentElement.scrollWidth, width: innerWidth });
  }
  let configMetrics;
  if (isConfigPage) {
    if (!configTable) {
      failures.push({ kind: 'config-table-missing' });
    } else {
      const container = configTable.parentElement;
      configMetrics = {
        tableClientWidth: configTable.clientWidth,
        tableScrollWidth: configTable.scrollWidth,
        containerClientWidth: container.clientWidth,
        pageScrollWidth: document.documentElement.scrollWidth,
        viewportWidth: innerWidth,
      };
      if (configMetrics.tableScrollWidth !== configMetrics.containerClientWidth) {
        failures.push({ kind: 'config-table-container-overflow', ...configMetrics });
      }
    }
  }
  let providerTable;
  if (location.pathname === '/concepts/agents/' && innerWidth === 1280) {
    const table = [...document.querySelectorAll('table')].find((el) =>
      [...el.querySelectorAll('tbody code')].some((code) => code.textContent === 'model_reasoning_effort' ||
        code.textContent === '--config model_reasoning_effort'));
    if (!table) {
      failures.push({ kind: 'provider-table-missing' });
    } else {
      const bounds = table.getBoundingClientRect();
      const cells = [...table.querySelectorAll('th, td')].map((cell) => cell.getBoundingClientRect());
      providerTable = { clientWidth: table.clientWidth, scrollWidth: table.scrollWidth,
        tableRight: bounds.right, cellRight: Math.max(...cells.map((rect) => rect.right)),
        borderRight: getComputedStyle(table).borderRightWidth };
      if (table.scrollWidth > table.clientWidth || providerTable.cellRight > bounds.right + tolerance ||
          bounds.right > table.parentElement.getBoundingClientRect().right + tolerance ||
          parseFloat(providerTable.borderRight) <= 0) {
        failures.push({ kind: 'provider-table-clipping', ...providerTable });
      }
    }
  }
  return { codes, wrappedCodes, configMetrics, providerTable, failures };
}

const routes = await htmlRoutes(fileURLToPath(new URL('../dist/', import.meta.url)));
assert.ok(routes.length, 'Build the website before running the browser check');
const fixtures = [
  { source: '`orbit clock tick`', text: 'orbit clock tick', width: 90 },
  { source: '`orbit run auto --local-candidate --confirm --force`',
    text: 'orbit run auto --local-candidate --confirm --force', width: 220 },
  { source: '`path/to/config.toml=value`', text: 'path/to/config.toml=value', width: 120 },
  { source: '`an_unbroken_identifier_that_is_wider_than_its_container`',
    text: 'an_unbroken_identifier_that_is_wider_than_its_container', width: 120, scrolls: true },
  { source: '<p><code data-example="keep" title="a > b / c.d=e">orbit --strict-worker-containment &lt;value&gt;</code></p>',
    text: 'orbit --strict-worker-containment <value>', width: 180 },
  { source: '<p><code>alpha/<strong>beta-gamma</strong>.delta=value &amp; more</code></p>',
    text: 'alpha/beta-gamma.delta=value & more', width: 120 },
  { source: '<p><code>orbit --strict-<strong>worker</strong>-containment=value</code></p>',
    text: 'orbit --strict-worker-containment=value', width: 180 },
];
const fixtureHTML = fixtures.map(({ source, width }) =>
  `<div style="width:${width}px;max-width:100%">${markdownToHtml(source, { hastPlugins: [inlineCodeWrap] }).html}</div>`).join('');
const preText = 'orbit --local-candidate\npath/to/config.toml=value';
const preHTML = markdownToHtml(`\n\n<pre><code>${preText}</code></pre>`, { hastPlugins: [inlineCodeWrap] }).html;
const evidence = { routes, viewports: [], fixtures: [], failures: [] };
const browser = await chromium.launch();
try {
  const standardWidths = [375, 768, 1280, 1440];
  for (const width of [320, ...standardWidths, 1024, 1152, 1920]) {
    for (const colorScheme of ['dark', 'light']) {
      const page = await browser.newPage({ viewport: { width, height: 900 }, colorScheme });
      await page.addInitScript((theme) => localStorage.setItem('orbit-theme-choice', theme), colorScheme);
      for (const route of routes) {
        if (route !== '/reference/config/' && !standardWidths.includes(width)) continue;
        const response = await page.goto(new URL(route, siteURL).href);
        assert.equal(response.status(), 200, `Built route ${route} must load`);
        await page.evaluate(async () => {
          await document.fonts.load('16px "Geist Variable"');
          await document.fonts.load('16px "Geist Mono Variable"');
          await document.fonts.ready;
          // Include older release notes and any other collapsed examples.
          for (const details of document.querySelectorAll('main details')) details.open = true;
        });
        assert.equal(await page.locator('html').getAttribute('data-theme'), colorScheme);
        const result = { ...await page.evaluate(inspectLayout), providerPanels: [] };
        evidence.viewports.push({ route, width, colorScheme, ...result });
        evidence.failures.push(...result.failures.map((failure) => ({ route, width, colorScheme, ...failure })));
        // The provider picker hides authored inline examples using CSS. Select
        // each provider so the same assertions cover every available panel.
        const providers = page.locator('input[name="ose-provider"]');
        for (let index = 0; index < await providers.count(); index++) {
          const id = await providers.nth(index).getAttribute('id');
          await page.locator(`label[for="${id}"]`).click();
          const selected = await providers.nth(index).inputValue();
          const panelResult = await page.evaluate(inspectLayout);
          result.providerPanels.push({ selected, codes: panelResult.codes, wrappedCodes: panelResult.wrappedCodes });
          evidence.failures.push(...panelResult.failures.map((failure) =>
            ({ route, width, colorScheme, selected, ...failure })));
        }
        if (route === '/reference/cli/') {
          const rendered = await page.evaluate(({ html, pre }) => {
            const section = document.createElement('section');
            section.id = 'inline-code-fixtures';
            section.innerHTML = html + pre;
            document.querySelector('.sl-markdown-content').append(section);
            return {
              codes: [...section.querySelectorAll('code:not(pre code)')].map((code) => {
                const tops = new Set();
                const walker = document.createTreeWalker(code, NodeFilter.SHOW_TEXT);
                for (let node = walker.nextNode(); node; node = walker.nextNode()) {
                  const range = document.createRange();
                  range.selectNodeContents(node);
                  for (const rect of range.getClientRects()) if (rect.width) tops.add(rect.top);
                }
                code.scrollLeft = 1;
                const scrolls = code.scrollLeft > 0;
                code.scrollLeft = 0;
                return { text: code.textContent, innerText: code.innerText, scrolls, lines: tops.size };
              }),
              attribute: section.querySelector('[data-example]')?.getAttribute('data-example'),
              title: section.querySelector('[data-example]')?.title,
              pre: section.querySelector('pre code').textContent,
              preElements: section.querySelector('pre code').children.length,
            };
          }, { html: fixtureHTML, pre: preHTML });
          assert.deepEqual(rendered.codes.map(({ text }) => text), fixtures.map(({ text }) => text),
            'Markdown and raw HTML wrapping must preserve selectable code text and entities');
          assert.deepEqual(rendered.codes.map(({ innerText }) => innerText), fixtures.map(({ text }) => text),
            'Wrap opportunities must not add characters to copied examples');
          assert.equal(rendered.attribute, 'keep', 'Raw code attributes must survive wrapping');
          assert.equal(rendered.title, 'a > b / c.d=e', 'Quoted attribute delimiters must remain literal');
          assert.equal(rendered.pre, preText, 'Preformatted code must remain literal');
          assert.equal(rendered.preElements, 0, 'Preformatted examples must not receive inline tokens');
          for (const [index, fixture] of fixtures.entries()) {
            if (fixture.scrolls) assert.ok(rendered.codes[index].scrolls, 'An oversized identifier must scroll locally');
            else assert.ok(rendered.codes[index].lines > 1, 'Narrow commands and paths must actually wrap');
          }
          const fixtureResult = await page.evaluate(inspectLayout);
          evidence.fixtures.push({ width, colorScheme, ...rendered });
          evidence.failures.push(...fixtureResult.failures.map((failure) =>
            ({ route, width, colorScheme, fixture: true, ...failure })));
          await page.locator('#inline-code-fixtures').evaluate((el) => el.remove());
        }
        if (result.failures.length) {
          await page.screenshot({ path: path.join(evidenceDir,
            `${route.replace(/[^a-z0-9]/gi, '_')}-${width}-${colorScheme}.png`) });
        }
      }
      await page.close();
    }
  }
  await writeFile(path.join(evidenceDir, 'inline-code-browser.json'), JSON.stringify(evidence, null, 2) + '\n');
  console.log(JSON.stringify({ pages: routes.length, layouts: evidence.viewports.length,
    codes: evidence.viewports.reduce((sum, result) => sum + result.codes, 0),
    failures: evidence.failures.length, evidence: path.join(evidenceDir, 'inline-code-browser.json') }));
  assert.equal(evidence.failures.length, 0,
    `Inline code must wrap at token boundaries and fit its container: ${JSON.stringify(evidence.failures.slice(0, 10))}`);
} finally {
  await browser.close();
}
