// Invoked by host_forward::gzip_forward_returns_json_to_the_browser.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const [modulePath, evidencePath, origin] = process.argv.slice(2);
const { chromium } = await import(pathToFileURL(modulePath).href);
const browser = await chromium.launch({ headless: true });
try {
  const page = await browser.newPage();
  // Avoid unrelated dashboard polling; use a same-origin JSON page.
  await page.goto(`${origin}/api/workspaces`);
  const results = await page.evaluate(async () => {
    const results = [];
    for (const resource of ['routines', 'job-runs?limit=25']) {
      const separator = resource.includes('?') ? '&' : '?';
      const response = await fetch(`/api/on/hostb/${resource}${separator}workspace=ws_http_fixture`);
      results.push({ resource, status: response.status, encoding: response.headers.get('content-encoding'), json: await response.json() });
    }
    return results;
  });
  for (const result of results) {
    assert.equal(result.status, 200);
    assert.equal(result.encoding, 'gzip');
    assert.ok(result.json !== null && typeof result.json === 'object');
  }
  assert.equal(results[1].json.items[0].run_id, 'jrun-browser-compression');
  fs.mkdirSync(evidencePath, { recursive: true });
  fs.writeFileSync(path.join(evidencePath, 'result.json'), JSON.stringify(results, null, 2));
  console.log('Browser decoded forwarded gzip JSON from both real dashboards');
} finally {
  await browser.close();
}
