// Serves the dashboard asset tree the way `orbit web serve` does, for the
// browser harnesses' disposable fixture servers: `/static/<path>` and
// `/<path>` both map to assets/dashboard/<path>, and `/static/dashboard.css`
// joins css/ in the order `DASHBOARD_CSS` in lib.rs lists the files.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const assets = fileURLToPath(new URL('../../assets/dashboard/', import.meta.url));
const libRs = fileURLToPath(new URL('../lib.rs', import.meta.url));

function joinedStylesheet() {
  const source = fs.readFileSync(libRs, 'utf8');
  const start = source.indexOf('DASHBOARD_CSS: &str = concat!(');
  const block = source.slice(start, source.indexOf(');', start));
  const files = [...block.matchAll(/include_str!\("\.\.\/assets\/dashboard\/(css\/[^"]+)"\)/g)].map(match => match[1]);
  if (start < 0 || files.length === 0) throw new Error('DASHBOARD_CSS file list not found in lib.rs');
  return Buffer.concat(files.map(file => fs.readFileSync(path.join(assets, file))));
}

/** The bytes and content type served at `pathname`, or null for a 404. */
export function dashboardFile(pathname) {
  if (pathname === '/static/dashboard.css') return { data: joinedStylesheet(), type: 'text/css' };
  const relative = pathname === '/' ? 'index.html' : pathname.replace(/^\/(static\/)?/, '');
  const file = path.resolve(assets, relative);
  if (!file.startsWith(assets) || !fs.existsSync(file) || !fs.statSync(file).isFile()) return null;
  const type = file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : 'text/javascript';
  return { data: fs.readFileSync(file), type };
}
