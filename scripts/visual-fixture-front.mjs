// Workspace front for scripts/visual-fixture-server.sh.
//
// Serves declared dashboard assets from the admitted checkout and proxies
// every other request to the disposable API backend. Paths in the dashboard
// static namespace are never proxied. A proxied request stays on the
// validated loopback upstream origin: a network-path or absolute-form
// target is refused before any outbound dispatch. Readiness is refused when a
// declared asset is missing or unreadable, or when OPTIONS probes do not
// establish that the backend has each /api path the checkout dashboard can request.
// Template segments are filled from the checkout route table. Payload
// schemas are not compared. Provenance records the
// renderer, source revision and asset hashes separately from the backend
// and does not claim those identities match.
import crypto from 'node:crypto';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const FRONT_PATH = fileURLToPath(import.meta.url);
const PROBE_SEGMENT = 'orbit-fixture-probe';

function fail(message) {
  const line = message.startsWith('visual-fixture-server:') ? message : `visual-fixture-server: ${message}`;
  console.error(line);
  process.exit(1);
}

function requireEnv(name) {
  const value = process.env[name];
  if (!value) fail(`${name} is not set`);
  return value;
}

export function classifyProbeStatus(status) {
  if (status === 404) return 'absent';
  if (
    status === 405 || status === 400 || status === 401 || status === 409
    || status === 415 || status === 422 || status === 429
    || (status >= 200 && status < 400)
  ) {
    return 'present';
  }
  return 'unestablished';
}

function extractLiterals(source) {
  const literals = [];
  let index = 0;
  while (index < source.length) {
    const char = source[index];
    if (char === '/' && source[index + 1] === '/') {
      const next = source.indexOf('\n', index);
      index = next < 0 ? source.length : next + 1;
      continue;
    }
    if (char === '/' && source[index + 1] === '*') {
      const next = source.indexOf('*/', index + 2);
      index = next < 0 ? source.length : next + 2;
      continue;
    }
    if (char === '"' || char === "'") {
      let cursor = index + 1;
      let value = '';
      while (cursor < source.length) {
        if (source[cursor] === '\\') {
          value += source[cursor + 1] ?? '';
          cursor += 2;
          continue;
        }
        if (source[cursor] === char) break;
        value += source[cursor];
        cursor += 1;
      }
      literals.push(value);
      index = cursor + 1;
      continue;
    }
    if (char === '`') {
      let cursor = index + 1;
      let value = '';
      while (cursor < source.length) {
        if (source[cursor] === '\\') {
          value += source[cursor + 1] ?? '';
          cursor += 2;
          continue;
        }
        if (source[cursor] === '`') break;
        if (source[cursor] === '$' && source[cursor + 1] === '{') {
          // A substitution that is its own path segment is a parameter.
          // One glued on, such as `?refresh=true`, is not part of the path.
          if (value.endsWith('/')) value += PROBE_SEGMENT;
          let depth = 1;
          cursor += 2;
          while (cursor < source.length && depth > 0) {
            if (source[cursor] === '{') depth += 1;
            else if (source[cursor] === '}') depth -= 1;
            cursor += 1;
          }
          continue;
        }
        value += source[cursor];
        cursor += 1;
      }
      literals.push(value);
      index = cursor + 1;
      continue;
    }
    index += 1;
  }
  return literals;
}

function normalizeApiPath(raw) {
  const pathOnly = raw.split('?')[0].split('#')[0].replace(/\/{2,}/g, '/');
  const trimmed = pathOnly.length > 1 && pathOnly.endsWith('/') ? pathOnly.slice(0, -1) : pathOnly;
  if (!/^\/api\/[A-Za-z0-9._~-]+(?:\/[A-Za-z0-9._~-]+)*$/.test(trimmed)) return null;
  return trimmed;
}

function firstPartyDashboardFiles(dashboardDir) {
  const files = [];
  const add = (file) => {
    if (fs.existsSync(file) && fs.statSync(file).isFile()) files.push(file);
  };
  add(path.join(dashboardDir, 'index.html'));
  add(path.join(dashboardDir, 'app.js'));
  const jsDir = path.join(dashboardDir, 'js');
  if (fs.existsSync(jsDir)) {
    for (const name of fs.readdirSync(jsDir)) {
      if (name.endsWith('.js')) add(path.join(jsDir, name));
    }
  }
  return files;
}

function repoRootFromDashboard(dashboardDir) {
  return path.resolve(dashboardDir, '../../../..');
}

function checkoutApiRoutes(dashboardDir) {
  const root = path.join(repoRootFromDashboard(dashboardDir), 'crates/orbit-web/src');
  if (!fs.existsSync(root)) return [];
  const routes = [];
  const walk = (dir) => {
    for (const name of fs.readdirSync(dir)) {
      const file = path.join(dir, name);
      if (fs.statSync(file).isDirectory()) walk(file);
      else if (name.endsWith('.rs')) routes.push(fs.readFileSync(file, 'utf8'));
    }
  };
  walk(root);
  const found = new Set();
  for (const match of routes.join('\n').matchAll(/\.route\(\s*"([^"]+)"/g)) {
    const route = match[1];
    if (!route.startsWith('/')) continue;
    found.add(route.startsWith('/api/') ? route : `/api${route}`);
  }
  return [...found];
}

function alignSegment(templateSegment, routeSegment, literals, out) {
  if (templateSegment !== PROBE_SEGMENT) {
    if (templateSegment !== routeSegment) return false;
    out.push(templateSegment);
    return true;
  }
  if (routeSegment.startsWith('{')) {
    out.push(PROBE_SEGMENT);
    return true;
  }
  if (literals.has(routeSegment)) {
    out.push(routeSegment);
    return true;
  }
  return false;
}

function instantiateTemplate(templatePath, routes, literals) {
  const parts = templatePath.split('/');
  const emitted = [];
  for (const route of routes) {
    const routeParts = route.split('/');
    const star = routeParts.findIndex((segment) => segment.startsWith('{*'));
    const out = [];
    if (star >= 0) {
      if (parts.length <= star) continue;
      let matched = true;
      for (let index = 0; index < star; index += 1) {
        if (!alignSegment(parts[index], routeParts[index], literals, out)) {
          matched = false;
          break;
        }
      }
      if (!matched) continue;
      for (let index = star; index < parts.length; index += 1) out.push(parts[index]);
      emitted.push(out.join('/'));
      continue;
    }
    if (routeParts.length !== parts.length) continue;
    let matched = true;
    for (let index = 0; index < parts.length; index += 1) {
      if (!alignSegment(parts[index], routeParts[index], literals, out)) {
        matched = false;
        break;
      }
    }
    if (matched) emitted.push(out.join('/'));
  }
  return emitted;
}

/**
 * Concrete /api paths the checkout dashboard can request.
 * A trailing-slash prefix is not a request. A template segment is probed as
 * each checkout route that can serve it. A template that matches no checkout
 * route is probed as written, so an unresolved path cannot pass silently.
 */
export function candidateApiPaths(dashboardDir) {
  const found = new Set();
  const templates = [];
  const literals = new Set();
  for (const file of firstPartyDashboardFiles(dashboardDir)) {
    const source = fs.readFileSync(file, 'utf8');
    for (const literal of extractLiterals(source)) {
      if (/^[A-Za-z0-9._~-]+$/.test(literal)) literals.add(literal);
      const pattern = /\/api\/[A-Za-z0-9._~/-]+/g;
      for (const match of literal.matchAll(pattern)) {
        if (match[0].endsWith('/')) continue;
        const normalized = normalizeApiPath(match[0]);
        if (!normalized) continue;
        if (normalized.includes(PROBE_SEGMENT)) templates.push(normalized);
        else found.add(normalized);
      }
    }
  }
  const routes = checkoutApiRoutes(dashboardDir);
  for (const template of templates) {
    const concrete = instantiateTemplate(template, routes, literals);
    if (concrete.length === 0) found.add(template);
    else for (const path of concrete) found.add(path);
  }
  return [...found].sort();
}

function sha256(bytes) {
  return crypto.createHash('sha256').update(bytes).digest('hex');
}

function sha256File(file) {
  const hash = crypto.createHash('sha256');
  const handle = fs.openSync(file, 'r');
  try {
    const buffer = Buffer.alloc(1024 * 1024);
    let read = 0;
    while ((read = fs.readSync(handle, buffer, 0, buffer.length, null)) > 0) {
      hash.update(buffer.subarray(0, read));
    }
  } finally {
    fs.closeSync(handle);
  }
  return hash.digest('hex');
}

function resolveBackend(argv0) {
  if (argv0.includes('/') || argv0.includes('\\')) return path.resolve(argv0);
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, argv0);
    if (fs.existsSync(candidate)) return candidate;
  }
  return argv0;
}

function gitText(repo, args) {
  return execFileSync('git', args, {
    cwd: repo,
    env: { ...process.env, GIT_OPTIONAL_LOCKS: '0' },
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  }).trim();
}

function loadDashboard(repo) {
  const helper = path.join(repo, 'crates/orbit-web/src/tests/dashboard_static.mjs');
  return import(pathToFileURL(helper).href);
}

async function loadDeclared(repo) {
  const dash = await loadDashboard(repo);
  const routes = dash.dashboardDeclaredRoutes();
  const loaded = routes.map((route) => {
    const served = dash.dashboardDeclaredAsset(route);
    return { route, data: served.data, mediaType: dash.dashboardMediaType(route) };
  });
  return { dash, loaded };
}

function loopbackUpstream(raw) {
  let url;
  try {
    url = new URL(raw);
  } catch {
    fail('fixture upstream is not a loopback origin');
  }
  const host = url.hostname.toLowerCase();
  const loopback = host === '127.0.0.1' || host === 'localhost' || host === '::1';
  if (url.protocol !== 'http:' || !loopback || !url.port || url.username || url.password) {
    fail('fixture upstream is not a loopback origin');
  }
  if ((url.pathname !== '/' && url.pathname !== '') || url.search || url.hash) {
    fail('fixture upstream is not a loopback origin');
  }
  return url;
}

// Origin-form only. `new URL(req.url, upstream)` treats a network-path (`//host`),
// a leading-slash backslash, and an absolute URI as a new authority, so those
// forms are rejected before any URL is built from the request target.
function resolveUpstreamPath(upstream, pathAndQuery) {
  if (
    typeof pathAndQuery !== 'string'
    || pathAndQuery.length === 0
    || pathAndQuery.length > 8192
    || pathAndQuery.charCodeAt(0) !== 47
    || pathAndQuery.charCodeAt(1) === 47
    || /[\u0000-\u001f\u007f\\]/.test(pathAndQuery)
    || pathAndQuery.includes('#')
  ) {
    return null;
  }
  const queryAt = pathAndQuery.indexOf('?');
  const rawPath = queryAt === -1 ? pathAndQuery : pathAndQuery.slice(0, queryAt);
  let decoded;
  try {
    decoded = decodeURIComponent(rawPath);
  } catch {
    return { error: 400, message: 'bad path encoding' };
  }
  if (
    !decoded.startsWith('/') || decoded.startsWith('//')
    || decoded.includes('\\') || decoded.includes('\0')
    || decoded.split('/').includes('..')
  ) {
    return { error: 400, message: 'path escapes the dashboard asset root' };
  }
  let target;
  try {
    target = new URL(pathAndQuery, upstream);
  } catch {
    return null;
  }
  if (
    target.origin !== upstream.origin
    || target.protocol !== 'http:'
    || target.host !== upstream.host
    || target.username
    || target.password
  ) {
    return null;
  }
  return { pathname: decoded, target };
}

function confinedRequestTarget(reqUrl, upstream) {
  const resolved = resolveUpstreamPath(upstream, reqUrl);
  if (!resolved) {
    return { error: 400, message: 'request target is not confined to the fixture upstream' };
  }
  return resolved;
}

function staticNamespace(pathname) {
  const lower = pathname.toLowerCase();
  return pathname === '/' || lower === '/static' || lower.startsWith('/static/');
}

function confinedDispatch(upstream, target, method, headers) {
  if (target.origin !== upstream.origin || target.protocol !== 'http:' || target.host !== upstream.host) {
    throw new Error('request target is not confined to the fixture upstream');
  }
  const pathAndQuery = `${target.pathname}${target.search}`;
  if (pathAndQuery.charCodeAt(0) !== 47 || pathAndQuery.charCodeAt(1) === 47) {
    throw new Error('request target is not confined to the fixture upstream');
  }
  return http.request({
    protocol: 'http:',
    hostname: upstream.hostname,
    port: upstream.port,
    method,
    path: pathAndQuery,
    headers,
  });
}

function probeStatus(upstream, pathname) {
  const resolved = resolveUpstreamPath(upstream, pathname);
  if (!resolved || resolved.error) return Promise.reject(new Error('probe path is not confined'));
  return new Promise((resolve, reject) => {
    let request;
    try {
      request = confinedDispatch(upstream, resolved.target, 'OPTIONS', { host: upstream.host });
    } catch (error) {
      reject(error);
      return;
    }
    request.on('response', (response) => {
      response.resume();
      resolve(response.statusCode || 0);
    });
    request.setTimeout(2000, () => request.destroy(new Error('timeout')));
    request.on('error', reject);
    request.end();
  });
}

function fetchHealth(upstream) {
  const resolved = resolveUpstreamPath(upstream, '/healthz');
  if (!resolved || resolved.error) return Promise.resolve({ status: 0, body: 'probe path is not confined' });
  return new Promise((resolve) => {
    let request;
    try {
      request = confinedDispatch(upstream, resolved.target, 'GET', { host: upstream.host });
    } catch (error) {
      resolve({ status: 0, body: String(error.message || error) });
      return;
    }
    request.on('response', (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => {
        resolve({
          status: response.statusCode || 0,
          body: Buffer.concat(chunks).toString('utf8').slice(0, 200),
        });
      });
    });
    request.setTimeout(2000, () => request.destroy(new Error('timeout')));
    request.on('error', (error) => resolve({ status: 0, body: String(error.message || error) }));
    request.end();
  });
}

async function establishCompatibility(repo, upstream) {
  const dashboardDir = path.join(repo, 'crates/orbit-web/assets/dashboard');
  let paths;
  try {
    paths = candidateApiPaths(dashboardDir);
  } catch (error) {
    fail(`source/API compatibility not established: ${error.message}`);
  }
  if (paths.length === 0) {
    fail('source/API compatibility not established: no candidate /api literals to probe');
  }
  const absent = [];
  const unestablished = [];
  for (const pathname of paths) {
    let status;
    try {
      status = await probeStatus(upstream, pathname);
    } catch (error) {
      fail(`source/API compatibility not established: probe ${pathname} failed: ${error.message}`);
    }
    const kind = classifyProbeStatus(status);
    if (kind === 'absent') absent.push(`${pathname} (${status})`);
    else if (kind === 'unestablished') unestablished.push(`${pathname} (${status})`);
  }
  if (absent.length > 0 || unestablished.length > 0) {
    const sample = [...absent, ...unestablished].slice(0, 12).join(', ');
    fail(`source/API compatibility not established: ${sample}`);
  }
  return paths;
}

function sendPlain(res, status, message) {
  const body = Buffer.from(message);
  res.writeHead(status, {
    'content-type': 'text/plain; charset=utf-8',
    'content-length': body.length,
    'cache-control': 'no-cache',
    'x-content-type-options': 'nosniff',
    'referrer-policy': 'no-referrer',
    'cross-origin-resource-policy': 'same-origin',
  });
  res.end(body);
}

function proxy(req, res, upstream, target) {
  const headers = { ...req.headers, host: upstream.host };
  const unsafe = /^(POST|PUT|PATCH|DELETE)$/i.test(req.method || '');
  if (headers.origin || unsafe) headers.origin = upstream.origin;
  let request;
  try {
    request = confinedDispatch(upstream, target, req.method, headers);
  } catch {
    sendPlain(res, 400, 'request target is not confined to the fixture upstream');
    return;
  }
  request.on('response', (response) => {
    res.writeHead(response.statusCode || 502, response.headers);
    response.pipe(res);
  });
  request.on('error', (error) => {
    if (!res.headersSent) sendPlain(res, 502, String(error));
    else res.end();
  });
  req.pipe(request);
}

function backendIdentity(argv0) {
  const resolved = resolveBackend(argv0);
  let version = '';
  let versionError = '';
  try {
    version = execFileSync(resolved, ['--version'], {
      encoding: 'utf8',
      timeout: 5000,
      stdio: ['ignore', 'pipe', 'pipe'],
    }).trim().split('\n')[0] || '';
  } catch (error) {
    versionError = error instanceof Error ? error.message : String(error);
  }
  let fileHash = null;
  try {
    if (fs.statSync(resolved).isFile()) fileHash = sha256File(resolved);
  } catch {
    fileHash = null;
  }
  return {
    argv0,
    resolved_path: resolved,
    version,
    version_error: versionError,
    sha256: fileHash,
    note: 'API backend identity only. Dashboard bytes are the checkout assets and are not this binary.',
  };
}

async function main() {
  const repo = requireEnv('ORBIT_FIXTURE_REPO');
  let loaded;
  let csp;
  try {
    const result = await loadDeclared(repo);
    loaded = result.loaded;
    csp = result.dash.dashboardCsp();
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
  if (process.argv.includes('--check-assets')) return;

  const upstream = loopbackUpstream(requireEnv('ORBIT_FIXTURE_UPSTREAM'));
  const overlay = process.env.ORBIT_FIXTURE_OVERLAY || 'none';
  const experiment = process.env.ORBIT_FIXTURE_EXPERIMENT || '';
  const task = process.env.ORBIT_FIXTURE_TASK || '';
  const provenancePath = requireEnv('ORBIT_FIXTURE_PROVENANCE');
  const launcher = process.env.ORBIT_FIXTURE_LAUNCHER || '';
  let seed = {};
  if (overlay === 'experiment' || overlay === 'scorecard') {
    try {
      seed = JSON.parse(fs.readFileSync(requireEnv('ORBIT_FIXTURE_SEED'), 'utf8'));
    } catch (error) {
      fail(`fixture seed unreadable: ${error.message}`);
    }
  }

  let sourceRevision = '';
  let sourceDirty = false;
  try {
    sourceRevision = gitText(repo, ['rev-parse', 'HEAD']);
    sourceDirty = gitText(repo, ['status', '--porcelain']).length > 0;
  } catch (error) {
    fail(`source revision not established: ${error.message}`);
  }
  if (!/^[0-9a-f]{40}$/.test(sourceRevision)) fail('source revision not established: HEAD is not a commit');

  const probed = await establishCompatibility(repo, upstream);
  const healthz = await fetchHealth(upstream);
  const backend = backendIdentity(requireEnv('ORBIT_FIXTURE_BACKEND'));
  const combined = crypto.createHash('sha256');
  const routes = loaded.map((asset) => {
    const digest = sha256(asset.data);
    combined.update(asset.route);
    combined.update('\0');
    combined.update(asset.data);
    combined.update('\0');
    return { path: asset.route, sha256: digest, bytes: asset.data.length };
  });
  const stylesheet = loaded.find((asset) => asset.route === '/static/dashboard.css');
  const provenance = {
    schema_version: 1,
    source_revision: sourceRevision,
    source_dirty: sourceDirty,
    renderer: {
      role: 'workspace-fixture-front',
      path: FRONT_PATH,
      sha256: sha256File(FRONT_PATH),
      launcher_sha256: launcher && fs.existsSync(launcher) ? sha256File(launcher) : null,
    },
    assets: {
      routes,
      stylesheet_sha256: stylesheet ? sha256(stylesheet.data) : null,
      combined_sha256: combined.digest('hex'),
    },
    backend,
    api_compatibility: {
      established: true,
      method: 'options_route_presence',
      probed: probed.length,
      paths: probed,
      absent: [],
      schema_compared: false,
    },
    healthz,
    static_backend_identity_compared: false,
  };
  fs.mkdirSync(path.dirname(provenancePath), { recursive: true });
  fs.writeFileSync(provenancePath, `${JSON.stringify(provenance, null, 2)}\n`);

  const byRoute = new Map(loaded.map((asset) => [asset.route, asset]));
  const server = http.createServer((req, res) => {
    try {
      const parsed = confinedRequestTarget(req.url || '/', upstream);
      if (parsed.error) {
        sendPlain(res, parsed.error, parsed.message);
        return;
      }
      const pathname = parsed.pathname;
      if (req.method === 'GET' && overlay === 'scorecard' && pathname === '/api/agents/implementers/scorecard' && seed.scorecard) {
        const window = parsed.target.searchParams.get('window') || 'last500';
        const body = Buffer.from(JSON.stringify({ ...seed.scorecard, window, requested_window: window }));
        res.writeHead(200, {
          'content-type': 'application/json',
          'content-length': body.length,
          'cache-control': 'no-cache',
          'x-content-type-options': 'nosniff',
        });
        res.end(body);
        return;
      }
      const experimentBase = `/api/diagnostics/experiments/${encodeURIComponent(experiment)}`;
      if (
        req.method === 'GET' && overlay === 'experiment' && experiment
        && (pathname === experimentBase || pathname === `${experimentBase}/readout`)
      ) {
        const body = Buffer.from(JSON.stringify(pathname === experimentBase ? seed.experiment : seed.readout));
        res.writeHead(200, {
          'content-type': 'application/json',
          'content-length': body.length,
          'cache-control': 'no-cache',
          'x-content-type-options': 'nosniff',
        });
        res.end(body);
        return;
      }
      if (staticNamespace(pathname)) {
        if (req.method !== 'GET' && req.method !== 'HEAD') {
          sendPlain(res, 405, 'method not allowed');
          return;
        }
        const asset = byRoute.get(pathname);
        if (!asset) {
          sendPlain(res, 404, `candidate asset missing: ${pathname}`);
          return;
        }
        const headers = {
          'content-type': asset.mediaType,
          'content-security-policy': csp,
          'content-length': asset.data.length,
          'cache-control': 'no-cache',
          'x-content-type-options': 'nosniff',
          'referrer-policy': 'no-referrer',
          'cross-origin-resource-policy': 'same-origin',
        };
        res.writeHead(200, headers);
        res.end(req.method === 'HEAD' ? undefined : asset.data);
        return;
      }
      proxy(req, res, upstream, parsed.target);
    } catch (error) {
      if (!res.headersSent) sendPlain(res, 500, `candidate asset unreadable: ${error.message}`);
      else res.end();
    }
  });
  server.on('connection', (socket) => {
    const addr = socket.remoteAddress;
    if (addr !== '127.0.0.1' && addr !== '::ffff:127.0.0.1') socket.destroy();
  });
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    const lines = [
      `ORBIT_VISUAL_SOURCE=${provenance.source_revision}`,
      `ORBIT_VISUAL_RENDERER_SHA256=${provenance.renderer.sha256}`,
      `ORBIT_VISUAL_ASSETS_SHA256=${provenance.assets.combined_sha256}`,
      `ORBIT_VISUAL_BACKEND_SHA256=${provenance.backend.sha256 || '-'}`,
      `ORBIT_VISUAL_BACKEND_VERSION=${(provenance.backend.version || 'unknown').replace(/[\r\n]/g, ' ')}`,
      `ORBIT_VISUAL_PROVENANCE=${provenancePath}`,
    ];
    if (task) lines.push(`ORBIT_VISUAL_TASK=${task}`);
    if (overlay === 'experiment' || overlay === 'scorecard') lines.push(`ORBIT_VISUAL_EXPERIMENT=${experiment}`);
    lines.push(`ORBIT_VISUAL_URL=http://127.0.0.1:${port}`);
    process.stdout.write(`${lines.join('\n')}\n`);
  });
}

const invokedDirectly = process.argv[1] && path.resolve(process.argv[1]) === FRONT_PATH;
if (invokedDirectly) {
  main().catch((error) => fail(error instanceof Error ? error.message : String(error)));
}
