// Serves the dashboard asset tree the way `orbit web serve` does, for the
// browser harnesses' disposable fixture servers and the workspace visual
// fixture: `/static/<path>` and `/` map to assets/dashboard, and
// `/static/dashboard.css` joins css/ in the order `DASHBOARD_CSS` in lib.rs
// lists the files. `dashboardCsp()` is the page policy `DASHBOARD_CSP` in
// assets.rs sends. `dashboardDeclaredRoutes()` is the embedded route list.
// `dashboardDeclaredAsset()` returns one of those routes or throws when the
// checkout file is missing or unreadable, so a fixture cannot fall through
// to another server's bytes.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const assets = fileURLToPath(new URL('../../assets/dashboard/', import.meta.url));
const libRs = fileURLToPath(new URL('../lib.rs', import.meta.url));
const assetsRs = fileURLToPath(new URL('../assets.rs', import.meta.url));

function joinedStylesheet() {
  const source = fs.readFileSync(libRs, 'utf8');
  const start = source.indexOf('DASHBOARD_CSS: &str = concat!(');
  const block = source.slice(start, source.indexOf(');', start));
  const files = [...block.matchAll(/include_str!\("\.\.\/assets\/dashboard\/(css\/[^"]+)"\)/g)].map(match => match[1]);
  if (start < 0 || files.length === 0) throw new Error('DASHBOARD_CSS file list not found in lib.rs');
  return Buffer.concat(files.map(file => fs.readFileSync(path.join(assets, file))));
}

/** The Content-Security-Policy the dashboard serves, read from `DASHBOARD_CSP`. */
export function dashboardCsp() {
  const source = fs.readFileSync(assetsRs, 'utf8');
  const start = source.indexOf('DASHBOARD_CSP: &str = concat!(');
  const block = source.slice(start, source.indexOf(');', start));
  const parts = [...block.matchAll(/^\s*"([^"]*)",?\s*$/gm)].map(match => match[1]);
  if (start < 0 || parts.length === 0) throw new Error('DASHBOARD_CSP not found in assets.rs');
  return parts.join('');
}

/** Route paths of `DASHBOARD_FILES`, in source order. */
export function dashboardDeclaredRoutes() {
  const source = fs.readFileSync(assetsRs, 'utf8');
  const start = source.indexOf('DASHBOARD_FILES:');
  const end = source.indexOf('\n];', start);
  const block = start < 0 || end < 0 ? '' : source.slice(start, end);
  const routes = [...block.matchAll(/\(\s*"(\/[^"]*)"\s*,/g)].map(match => match[1]);
  if (routes.length === 0) throw new Error('DASHBOARD_FILES routes not found in assets.rs');
  return routes;
}

/** Production Content-Type for a declared dashboard route. */
export function dashboardMediaType(pathname) {
  if (pathname === '/static/dashboard.css' || pathname.endsWith('.css')) return 'text/css; charset=utf-8';
  if (pathname === '/' || pathname.endsWith('.html')) return 'text/html; charset=utf-8';
  if (pathname.endsWith('.woff2')) return 'font/woff2';
  if (pathname.endsWith('.js')) return 'application/javascript; charset=utf-8';
  return 'application/octet-stream';
}

function assetFile(relative) {
  if (!relative || relative.includes('\0')) return null;
  const file = path.resolve(assets, relative);
  const rel = path.relative(assets, file);
  if (!rel || rel.startsWith('..') || path.isAbsolute(rel)) return null;
  return file;
}

/** The bytes and content type served at `pathname`, or null for a 404. */
export function dashboardFile(pathname) {
  if (pathname === '/static/dashboard.css') return { data: joinedStylesheet(), type: 'text/css' };
  const relative = pathname === '/' ? 'index.html' : pathname.replace(/^\/(static\/)?/, '');
  const file = assetFile(relative);
  if (!file || !fs.existsSync(file) || !fs.statSync(file).isFile()) return null;
  const type = file.endsWith('.html') ? 'text/html' : file.endsWith('.css') ? 'text/css' : 'text/javascript';
  return { data: fs.readFileSync(file), type };
}

/**
 * Bytes for one declared route. Throws when the route is declared and the
 * checkout asset is missing or unreadable. Returns null for any other path.
 */
export function dashboardDeclaredAsset(pathname) {
  if (!dashboardDeclaredRoutes().includes(pathname)) return null;
  let served;
  try {
    served = dashboardFile(pathname);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    throw new Error(`candidate asset unreadable: ${pathname}: ${message}`);
  }
  if (!served || served.data == null) throw new Error(`candidate asset missing: ${pathname}`);
  return served;
}
