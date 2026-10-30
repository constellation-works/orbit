// Visual capture driver (visual-review design §4).
//
// One driver serves the delivery capture and the implementer's
// `orbit visual capture` self-check (§5). Orbit writes a request JSON
// (resolved mocks, view/app definitions from the view manifest, output
// directory, kit location) and runs this script with the browser kit's
// node after sourcing the kit's env.sh, so PLAYWRIGHT_MODULE names the
// pinned Playwright build. Pixel comparison uses the kit's pinned
// `pngjs`, so every host compares pixels with the same code.
//
// Usage: node capture-driver.mjs --request <file>
// Prints one JSON report as the last stdout line. Exit 1 is a fatal
// driver error; a mock that fails to render is reported per mock
// (`error.code` render_timeout / setup_failed) and the run continues.

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { copyFile, mkdir, readFile, readdir, stat, unlink, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

const ANIMATION_OFF =
  "*,*::before,*::after{animation:none !important;transition:none !important;caret-color:transparent !important}";
const activeFixtureStops = new Set();

function fail(message) {
  throw new Error(message);
}

export function viewportForWidth(width) {
  const height = width === 1440 ? 900 : width === 375 ? 812 : Math.round((width * 10) / 16);
  return { width, height };
}

export function browserContextOptions(target) {
  return {
    viewport: viewportForWidth(target.width),
    deviceScaleFactor: 1,
    colorScheme: target.theme,
    reducedMotion: "reduce",
    timezoneId: "UTC",
    locale: "en-US",
  };
}

function isLoopback(urlText) {
  try {
    const url = new URL(urlText);
    if (url.protocol === "data:" || url.protocol === "blob:" || url.protocol === "about:") {
      return true;
    }
    return ["127.0.0.1", "localhost", "[::1]", "::1"].includes(url.hostname);
  } catch {
    return false;
  }
}

async function loadPlaywright() {
  const modulePath = process.env.PLAYWRIGHT_MODULE;
  if (!modulePath) {
    fail("browser_kit_missing: PLAYWRIGHT_MODULE is not set; run `orbit browser-kit install` and source the kit's env.sh");
  }
  const mod = await import(pathToFileURL(modulePath).href);
  const playwright = mod.default ?? mod;
  if (!playwright.chromium) {
    fail(`browser_kit_missing: PLAYWRIGHT_MODULE ${modulePath} does not export chromium`);
  }
  let version = "";
  try {
    const pkg = JSON.parse(await readFile(path.join(path.dirname(modulePath), "package.json"), "utf8"));
    version = pkg.version ?? "";
  } catch {
    version = "";
  }
  return { chromium: playwright.chromium, version };
}

function loadPngjs(kitProject) {
  if (!kitProject) {
    fail("browser_kit_missing: the capture request names no kit npm project for the pixel-compare helpers; run `orbit browser-kit install`");
  }
  try {
    const require = createRequire(path.join(kitProject, "package.json"));
    return require("pngjs").PNG;
  } catch (error) {
    fail(`browser_kit_missing: the kit's pixel-compare helper pngjs is not installed under ${kitProject}; run \`orbit browser-kit install\` (${error.message})`);
  }
}

// Pixel-compare two PNG buffers. Returns {equal, region} where region is
// the [x, y, w, h] bounding box of differing pixels, or null when equal.
export function comparePngPixels(PNG, leftBuffer, rightBuffer) {
  if (leftBuffer.equals(rightBuffer)) return { equal: true, region: null };
  const left = PNG.sync.read(leftBuffer);
  const right = PNG.sync.read(rightBuffer);
  if (left.width !== right.width || left.height !== right.height) {
    return {
      equal: false,
      region: [0, 0, Math.max(left.width, right.width), Math.max(left.height, right.height)],
    };
  }
  let minX = left.width;
  let minY = left.height;
  let maxX = -1;
  let maxY = -1;
  for (let y = 0; y < left.height; y += 1) {
    for (let x = 0; x < left.width; x += 1) {
      const idx = (left.width * y + x) * 4;
      if (
        left.data[idx] !== right.data[idx] ||
        left.data[idx + 1] !== right.data[idx + 1] ||
        left.data[idx + 2] !== right.data[idx + 2] ||
        left.data[idx + 3] !== right.data[idx + 3]
      ) {
        if (x < minX) minX = x;
        if (x > maxX) maxX = x;
        if (y < minY) minY = y;
        if (y > maxY) maxY = y;
      }
    }
  }
  if (maxX < 0) return { equal: true, region: null };
  return { equal: false, region: [minX, minY, maxX - minX + 1, maxY - minY + 1] };
}

// Spawn an app setup command and wait for ORBIT_VISUAL_URL= on stdout.
// Extra ORBIT_VISUAL_<NAME>= lines resolve {name} route placeholders.
export async function startFixtureServer(app, fixture, repoRoot) {
  const argv = app.setup.map((part) => part.replaceAll("{fixture}", fixture));
  const child = spawn(argv[0], argv.slice(1), {
    cwd: repoRoot,
    env: { ...process.env },
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
  });
  const timeoutMs = (app.setup_timeout_s ?? 120) * 1000;
  // Wait on "exit", not only "close": "close" also waits for the stdio
  // pipes to close, which a descendant that escaped the process group and
  // inherited those pipes can prevent forever. "close" is kept as a
  // fallback because a failed spawn may emit it without "exit".
  const exited = new Promise((resolve) => {
    child.once("exit", resolve);
    child.once("close", resolve);
  });
  let stopPromise;
  const signalGroup = (signal) => {
    if (!child.pid) return;
    try {
      process.kill(-child.pid, signal);
    } catch {
      try {
        child.kill(signal);
      } catch {
        // Already gone.
      }
    }
  };
  const stop = () => {
    if (!stopPromise) {
      signalGroup("SIGTERM");
      stopPromise = (async () => {
        try {
          await Promise.race([exited, new Promise((resolve) => setTimeout(resolve, 100))]);
          signalGroup("SIGKILL");
          // Release our ends of the pipes after the kill. An escaped
          // descendant holding the far ends open would otherwise keep the
          // pipe handles (and this process) alive indefinitely.
          child.stdout?.destroy();
          child.stderr?.destroy();
          child.unref();
          // Bound the post-KILL wait so a signal handler awaiting stop()
          // cannot hang termination on a wedged fixture server.
          await Promise.race([
            exited,
            new Promise((resolve) => setTimeout(resolve, 5000)),
          ]);
        } finally {
          activeFixtureStops.delete(stop);
        }
      })();
    }
    return stopPromise;
  };
  activeFixtureStops.add(stop);

  try {
    const found = await new Promise((resolve, reject) => {
      let buffer = "";
      let stderr = "";
      const timer = setTimeout(
        () => reject(new Error(`setup for app ${app.name} timed out: ${stderr.slice(-800)}`)),
        timeoutMs,
      );
      const scan = () => {
        const placeholders = {};
        for (const match of buffer.matchAll(/^ORBIT_VISUAL_([A-Z0-9_]+)=(.+)$/gm)) {
          placeholders[match[1].toLowerCase()] = match[2].trim();
        }
        if (placeholders.url) {
          clearTimeout(timer);
          resolve({ baseUrl: placeholders.url, placeholders });
          return true;
        }
        const plain = buffer.match(/https?:\/\/(?:127\.0\.0\.1|localhost|\[::1\]):\d+/);
        if (plain) {
          clearTimeout(timer);
          resolve({ baseUrl: plain[0], placeholders });
          return true;
        }
        return false;
      };
      child.stdout.on("data", (chunk) => {
        buffer += chunk.toString();
        scan();
      });
      child.stderr.on("data", (chunk) => {
        stderr += chunk.toString();
      });
      child.on("error", (error) => {
        clearTimeout(timer);
        reject(error);
      });
      child.on("exit", (code) => {
        if (!scan()) {
          clearTimeout(timer);
          reject(new Error(`setup for app ${app.name} exited ${code}: ${stderr.slice(-800)}`));
        }
      });
    });
    return { ...found, stop };
  } catch (error) {
    await stop();
    throw error;
  }
}

export function resolveRoute(route, placeholders) {
  let missing = "";
  const resolved = route.replace(/\{([a-z0-9_-]+)\}/g, (whole, name) => {
    if (placeholders && placeholders[name]) return placeholders[name];
    missing = name;
    return whole;
  });
  return { resolved, missing };
}

// Open one fresh §4.2 context at the mock's view/state/theme/width.
// Returns {context, page, diagnostics, takeScreenshot}.
async function openCapturePage(browser, mock, server, frozenInstant, state, colorScheme) {
  const context = await browser.newContext(
    browserContextOptions({ width: mock.width, theme: colorScheme ?? mock.theme }),
  );
  const page = await context.newPage();
  const diagnostics = {
    consoleErrors: [],
    pageErrors: [],
    failedRequests: [],
    offLoopback: [],
    blockedNonGetOnLoad: [],
  };
  page.on("console", (message) => {
    if (message.type() === "error") diagnostics.consoleErrors.push(message.text().slice(0, 500));
  });
  page.on("pageerror", (error) => {
    diagnostics.pageErrors.push(String(error?.message ?? error).slice(0, 500));
  });
  page.on("requestfailed", (request) => {
    const url = request.url();
    let sameOrigin = false;
    try {
      sameOrigin = new URL(url).origin === new URL(server.baseUrl).origin;
    } catch {
      sameOrigin = false;
    }
    if (sameOrigin) {
      diagnostics.failedRequests.push({ url, failure: request.failure()?.errorText ?? "failed" });
    }
  });
  await page.route("**/*", async (route) => {
    const request = route.request();
    const url = request.url();
    if (!isLoopback(url)) {
      diagnostics.offLoopback.push(url);
      await route.abort("blockedbyclient").catch(() => {});
      return;
    }
    const method = request.method().toUpperCase();
    if (method !== "GET" && method !== "HEAD" && method !== "OPTIONS") {
      const record = { method, url, rule: state.currentRule || "page-load" };
      state.blocked.push(record);
      if (!state.currentRule) diagnostics.blockedNonGetOnLoad.push(record);
      await route.abort("blockedbyclient").catch(() => {});
      return;
    }
    if (mock.state === "error" && url.includes("/api/")) {
      await route
        .fulfill({ status: 500, contentType: "application/json", body: '{"error":"fixture"}' })
        .catch(() => {});
      return;
    }
    if (mock.state === "loading" && url.includes("/api/")) {
      // Held until capture: never answered on this page.
      return;
    }
    await route.continue().catch(() => {});
  });
  await page.clock.install({ time: new Date(frozenInstant) });
  await page.clock.pauseAt(new Date(frozenInstant));
  await page.addInitScript((css) => {
    const style = document.createElement("style");
    style.textContent = css;
    document.addEventListener("DOMContentLoaded", () => document.head.appendChild(style));
  }, ANIMATION_OFF);
  const view = mock.view_def;
  const { resolved, missing } = resolveRoute(view.route, server.placeholders);
  if (missing) {
    await context.close().catch(() => {});
    fail(`route placeholder {${missing}} of view ${view.id} was not printed by its setup command`);
  }
  const url = new URL(resolved.startsWith("#") ? `/${resolved}` : resolved, server.baseUrl).href;
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60000 });
  await page.addStyleTag({ content: ANIMATION_OFF });
  // The frozen clock never advances on its own; advance it a fixed step so
  // deferred renders scheduled with timers still happen, deterministically.
  await page.clock.runFor(3000).catch(() => {});
  if (mock.state !== "loading") {
    await page.locator(view.ready).first().waitFor({ state: "visible", timeout: 30000 });
    await page.waitForLoadState("networkidle", { timeout: 15000 }).catch(() => {});
  }
  await page.evaluate(() => document.fonts?.ready);
  await Promise.race([
    page.evaluate(
      () =>
        new Promise((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(resolve));
        }),
    ),
    new Promise((resolve) => setTimeout(resolve, 2000)),
  ]);
  for (const mask of view.mask ?? []) {
    await page
      .locator(mask)
      .evaluateAll((elements) => {
        for (const element of elements) {
          element.style.setProperty("background-color", "#888", "important");
          element.style.setProperty("color", "#888", "important");
        }
      })
      .catch(() => {});
  }
  const takeScreenshot = async () => {
    if (view.clip) {
      return page.locator(view.clip).first().screenshot();
    }
    return page.screenshot();
  };
  return { context, page, diagnostics, takeScreenshot };
}

// The ctx the workspace UI rules measure with (scripts/ux-rules shape).
function ruleContext(state, page, target, diagnostics, baseUrl) {
  return {
    page,
    target,
    diagnostics,
    baseUrl,
    currentRule: "",
    drainBlocked() {
      const drained = state.blocked;
      state.blocked = [];
      return drained;
    },
  };
}

async function loadRuleModules(rulesDir) {
  if (!rulesDir) return [];
  let names = [];
  try {
    names = (await readdir(path.join(rulesDir, "ui"))).filter((n) => n.endsWith(".mjs")).sort();
  } catch {
    return [];
  }
  const modules = [];
  for (const name of names) {
    const loaded = await import(pathToFileURL(path.join(rulesDir, "ui", name)).href);
    if (loaded.rule && typeof loaded.check === "function") modules.push(loaded);
  }
  return modules;
}

function ruleApplies(rule, mock) {
  const targets = rule.targets ?? {};
  const views = targets.views ?? "*";
  if (views !== "*" && !views.includes(mock.view)) return false;
  const states = targets.states ?? ["default"];
  if (!states.includes(mock.state)) return false;
  // Same width selection as expandTargets in scripts/ux-rules/lib/ui-context.mjs:
  // a rule runs only at its declared widths (default [1440]), and a
  // non-responsive view only at the first of them.
  const widths = targets.widths ?? [1440];
  const viewWidths = mock.view_def.responsive === false ? widths.slice(0, 1) : widths;
  return viewWidths.includes(mock.width);
}

export async function runRulesForMock(modules, state, mock, page, diagnostics, baseUrl) {
  const target = {
    id: `${mock.view}|${mock.theme}|${mock.width}|${mock.state}`,
    view: mock.view,
    viewDef: mock.view_def,
    app: mock.app_def.name,
    appDef: { ...mock.app_def },
    width: mock.width,
    theme: mock.theme,
    state: mock.state,
  };
  const ctx = ruleContext(state, page, target, diagnostics, baseUrl);
  const results = [];
  for (const loaded of modules) {
    if (!ruleApplies(loaded.rule, mock)) continue;
    state.currentRule = loaded.rule.id;
    ctx.currentRule = loaded.rule.id;
    state.blocked = [];
    let produced;
    try {
      produced = await loaded.check(ctx);
    } catch (error) {
      produced = {
        rule: loaded.rule.id,
        target: { id: target.id, view: target.view, width: target.width, theme: target.theme, state: target.state },
        status: "error",
        severity: loaded.rule.severity,
        measurements: { message: error instanceof Error ? error.message : String(error) },
        evidence: [],
        fingerprint: [loaded.rule.id, target.view, target.theme, String(target.width), target.state].join("|"),
      };
    }
    state.currentRule = "";
    ctx.currentRule = "";
    const list = (Array.isArray(produced) ? produced : [produced]).filter(Boolean);
    for (const result of list) {
      results.push({ mock: mock.artifact, ...result });
    }
  }
  return results;
}

// Write <dir>/mock.png: a byte copy for a PNG mock, a browser re-encode
// at natural size for jpg/jpeg/webp mocks.
async function writeMockPng(browser, mock, destination) {
  if (mock.mock_ext.toLowerCase() === "png") {
    await copyFile(mock.mock_file, destination);
    return;
  }
  const context = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await context.newPage();
  try {
    await page.goto(pathToFileURL(mock.mock_file).href);
    const size = await page.evaluate(() => {
      const img = document.querySelector("img");
      return img ? { width: img.naturalWidth, height: img.naturalHeight } : null;
    });
    if (!size || size.width === 0 || size.height === 0) {
      fail(`mock ${mock.artifact} did not decode as an image`);
    }
    await page.setViewportSize({
      width: Math.min(size.width, 4000),
      height: Math.min(size.height, 4000),
    });
    await page.locator("img").first().screenshot({ path: destination });
  } finally {
    await context.close().catch(() => {});
  }
}

// Side-by-side (§4.1): mock left, capture right, scaled to a common
// height, with a label strip naming both. Composed in the browser so
// every mock format decodes the same way.
async function writeSideBySide(browser, mock, dir) {
  const context = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await context.newPage();
  try {
    // Data URLs: a setContent page may not load file:// subresources.
    const dataUrl = async (file) =>
      `data:image/png;base64,${(await readFile(file)).toString("base64")}`;
    const mockUrl = await dataUrl(path.join(dir, "mock.png"));
    const captureUrl = await dataUrl(path.join(dir, "capture.png"));
    await page.setContent(
      `<!doctype html><meta charset="utf-8"><style>
        body{margin:0;background:#111;font:14px system-ui,sans-serif}
        .strip{display:flex;height:28px;color:#eee}
        .strip div{padding:5px 8px;white-space:nowrap;overflow:hidden}
        .row{display:flex;align-items:flex-start}
        img{display:block}
      </style>
      <div class="strip"><div id="lm"></div><div id="lc"></div></div>
      <div class="row"><img id="m"><img id="c"></div>`,
    );
    const dims = await page.evaluate(
      async ({ mockUrl, captureUrl, mockLabel, captureLabel }) => {
        const load = (id, src) =>
          new Promise((resolve, reject) => {
            const img = document.getElementById(id);
            img.onload = () => resolve({ width: img.naturalWidth, height: img.naturalHeight });
            img.onerror = () => reject(new Error(`could not load ${src}`));
            img.src = src;
          });
        const [m, c] = await Promise.all([load("m", mockUrl), load("c", captureUrl)]);
        const common = Math.min(m.height, c.height);
        let mw = Math.round((m.width * common) / m.height);
        let cw = Math.round((c.width * common) / c.height);
        let height = common;
        const maxWidth = 3000;
        if (mw + cw > maxWidth) {
          const scale = maxWidth / (mw + cw);
          mw = Math.max(1, Math.round(mw * scale));
          cw = Math.max(1, Math.round(cw * scale));
          height = Math.max(1, Math.round(common * scale));
        }
        const mImg = document.getElementById("m");
        const cImg = document.getElementById("c");
        mImg.style.width = `${mw}px`;
        mImg.style.height = `${height}px`;
        cImg.style.width = `${cw}px`;
        cImg.style.height = `${height}px`;
        const lm = document.getElementById("lm");
        const lc = document.getElementById("lc");
        lm.textContent = mockLabel;
        lc.textContent = captureLabel;
        lm.style.width = `${mw - 16}px`;
        lc.style.width = `${cw - 16}px`;
        return { width: mw + cw, height: height + 28 };
      },
      {
        mockUrl,
        captureUrl,
        mockLabel: `mock: ${mock.artifact}`,
        captureLabel: `capture: ${mock.view} ${mock.state} ${mock.theme} ${mock.width}px`,
      },
    );
    await page.setViewportSize({ width: dims.width, height: dims.height });
    await page.screenshot({ path: path.join(dir, "side-by-side.png") });
  } finally {
    await context.close().catch(() => {});
  }
}

// The colour preference opposite `theme`, for the color_scheme_leak check
// (§7); null for a theme that is neither light nor dark.
function oppositeScheme(theme) {
  if (theme === "dark") return "light";
  if (theme === "light") return "dark";
  return null;
}

// Keep an image under the artifact limit (§4.5): PNG first, then WebP from
// a browser canvas at full quality, then at 0.9. Chromium's canvas encoder
// is lossy at every quality, so a WebP is recorded `lossy: true`. Returns
// the file name used, its encoding and whether it is lossy.
async function fitImageLimit(browser, dir, base, maxBytes) {
  const pngName = `${base}.png`;
  const pngPath = path.join(dir, pngName);
  if (!maxBytes || (await stat(pngPath)).size <= maxBytes) {
    return { file: pngName, encoding: "png", lossy: false };
  }
  const context = await browser.newContext({ deviceScaleFactor: 1 });
  const page = await context.newPage();
  try {
    const dataUrl = `data:image/png;base64,${(await readFile(pngPath)).toString("base64")}`;
    for (const quality of [1, 0.9]) {
      const encoded = await page.evaluate(
        async ({ dataUrl, quality }) => {
          const img = new Image();
          img.src = dataUrl;
          await img.decode();
          const canvas = document.createElement("canvas");
          canvas.width = img.naturalWidth;
          canvas.height = img.naturalHeight;
          canvas.getContext("2d").drawImage(img, 0, 0);
          return canvas.toDataURL("image/webp", quality);
        },
        { dataUrl, quality },
      );
      const bytes = Buffer.from(encoded.slice(encoded.indexOf(",") + 1), "base64");
      if (bytes.length <= maxBytes || quality === 0.9) {
        const webpName = `${base}.webp`;
        await writeFile(path.join(dir, webpName), bytes);
        await unlink(pngPath).catch(() => {});
        return { file: webpName, encoding: "webp", lossy: true };
      }
    }
  } finally {
    await context.close().catch(() => {});
  }
  return { file: pngName, encoding: "png", lossy: false };
}

function sha256Hex(buffer) {
  return createHash("sha256").update(buffer).digest("hex");
}

async function captureMock({ browser, PNG, request, mock, server, ruleModules, ruleResults }) {
  const dir = path.join(request.out_dir, mock.name);
  await mkdir(dir, { recursive: true });
  const entry = {
    artifact: mock.artifact,
    name: mock.name,
    view: mock.view,
    state: mock.state,
    theme: mock.theme,
    width: mock.width,
    viewport: [mock.width, viewportForWidth(mock.width).height],
    fixture: mock.view_def.fixture,
  };
  const state = { currentRule: "", blocked: [] };
  const shots = [];
  let firstDiagnostics = null;
  const wanted = request.captures_per_mock === 2 ? 2 : 1;
  for (let attempt = 0; attempt < wanted; attempt += 1) {
    const opened = await openCapturePage(browser, mock, server, request.frozen_instant, state);
    try {
      shots.push(await opened.takeScreenshot());
      if (attempt === 0) {
        firstDiagnostics = opened.diagnostics;
        if (ruleModules.length > 0) {
          ruleResults.push(
            ...(await runRulesForMock(ruleModules, state, mock, opened.page, opened.diagnostics, server.baseUrl)),
          );
        }
      }
    } finally {
      await opened.context.close().catch(() => {});
    }
  }
  let stability = "single";
  let diffRegion = null;
  if (wanted === 2) {
    const first = comparePngPixels(PNG, shots[0], shots[1]);
    if (first.equal) {
      stability = "stable";
    } else {
      const opened = await openCapturePage(browser, mock, server, request.frozen_instant, state);
      let third;
      try {
        third = await opened.takeScreenshot();
      } finally {
        await opened.context.close().catch(() => {});
      }
      const matchesFirst = comparePngPixels(PNG, shots[0], third).equal;
      const matchesSecond = comparePngPixels(PNG, shots[1], third).equal;
      if (matchesFirst || matchesSecond) {
        stability = "stable_after_retry";
        diffRegion = first.region;
        shots[0] = matchesSecond && !matchesFirst ? shots[1] : shots[0];
      } else {
        stability = "unstable";
        diffRegion = first.region;
      }
    }
  }
  // color_scheme_leak (§7): a colour-scheme-invariant app must render the
  // same under the opposite preference. Only the delivery's double capture
  // checks it; the self-check captures once.
  const opposite = oppositeScheme(mock.theme);
  if (wanted === 2 && mock.app_def.color_scheme_invariant && opposite) {
    const opened = await openCapturePage(browser, mock, server, request.frozen_instant, state, opposite);
    try {
      const other = await opened.takeScreenshot();
      const compared = comparePngPixels(PNG, shots[0], other);
      entry.color_scheme_leak = !compared.equal;
      if (!compared.equal && compared.region) entry.color_scheme_region = compared.region;
    } finally {
      await opened.context.close().catch(() => {});
    }
  }
  entry.captured_at = new Date().toISOString();
  await writeFile(path.join(dir, "capture.png"), shots[0]);
  await writeMockPng(browser, mock, path.join(dir, "mock.png"));
  await writeSideBySide(browser, mock, dir);
  const captureFile = await fitImageLimit(browser, dir, "capture", request.max_image_bytes);
  const sideFile = await fitImageLimit(browser, dir, "side-by-side", request.max_image_bytes);
  entry.capture = `${mock.name}/${captureFile.file}`;
  entry.capture_encoding = captureFile.encoding;
  entry.capture_lossy = captureFile.lossy;
  entry.mock = `${mock.name}/mock.png`;
  entry.side_by_side = `${mock.name}/${sideFile.file}`;
  entry.side_by_side_encoding = sideFile.encoding;
  entry.side_by_side_lossy = sideFile.lossy;
  entry.stability = stability;
  if (diffRegion) entry.diff_region = diffRegion;
  entry.sha256 = sha256Hex(await readFile(path.join(dir, captureFile.file)));
  entry.side_by_side_sha256 = sha256Hex(await readFile(path.join(dir, sideFile.file)));
  entry.mock_sha256 = sha256Hex(await readFile(mock.mock_file));
  if (firstDiagnostics && firstDiagnostics.offLoopback.length > 0) {
    entry.off_loopback_requests = firstDiagnostics.offLoopback.slice(0, 10);
  }
  return entry;
}

async function main() {
  const requestPath = process.argv[process.argv.indexOf("--request") + 1];
  if (!requestPath) fail("usage: node capture-driver.mjs --request <file>");
  const request = JSON.parse(await readFile(requestPath, "utf8"));
  const { chromium, version } = await loadPlaywright();
  const PNG = loadPngjs(request.kit_npm_project);
  const ruleModules = await loadRuleModules(request.rules_dir);
  const browser = await chromium.launch();
  const browserVersion = browser.version();
  const servers = new Map();
  let shuttingDown = false;
  let receivedSignal = null;
  const shutdownOnSignal = (signal, exitCode) => {
    if (receivedSignal) {
      process.exit(1);
      return;
    }
    receivedSignal = signal;
    shuttingDown = true;
    const stops = [...activeFixtureStops].map((stop) => stop());
    void (async () => {
      try {
        await Promise.race([
          Promise.allSettled(stops),
          new Promise((resolve) => setTimeout(resolve, 5000)),
        ]);
      } finally {
        process.exit(exitCode);
      }
    })();
  };
  const onSigterm = () => shutdownOnSignal("SIGTERM", 143);
  const onSigint = () => shutdownOnSignal("SIGINT", 130);
  process.on("SIGTERM", onSigterm);
  process.on("SIGINT", onSigint);
  const ruleResults = [];
  const entries = [];
  const serverFor = async (mock) => {
    if (shuttingDown) throw new Error("capture driver is shutting down");
    const fixture =
      mock.state === "empty" ? `${mock.view_def.fixture}:empty` : mock.view_def.fixture;
    const key = `${mock.app_def.name}:${fixture}`;
    if (!servers.has(key)) {
      servers.set(
        key,
        await startFixtureServer(
          {
            name: mock.app_def.name,
            setup: mock.app_def.setup,
            setup_timeout_s: mock.app_def.setup_timeout_s,
          },
          fixture,
          request.repo_root,
        ),
      );
    }
    return servers.get(key);
  };
  try {
    for (const mock of request.mocks) {
      let server;
      try {
        server = await serverFor(mock);
      } catch (error) {
        entries.push({
          artifact: mock.artifact,
          name: mock.name,
          view: mock.view,
          state: mock.state,
          theme: mock.theme,
          width: mock.width,
          error: {
            code: "setup_failed",
            message: error instanceof Error ? error.message : String(error),
          },
        });
        continue;
      }
      try {
        entries.push(
          await captureMock({ browser, PNG, request, mock, server, ruleModules, ruleResults }),
        );
      } catch (error) {
        entries.push({
          artifact: mock.artifact,
          name: mock.name,
          view: mock.view,
          state: mock.state,
          theme: mock.theme,
          width: mock.width,
          error: {
            code: "render_timeout",
            message: error instanceof Error ? error.message : String(error),
          },
        });
      }
    }
  } finally {
    await Promise.all([...activeFixtureStops].map((stop) => stop()));
    process.off("SIGTERM", onSigterm);
    process.off("SIGINT", onSigint);
    await browser.close().catch(() => {});
  }
  ruleResults.sort((a, b) => String(a.fingerprint).localeCompare(String(b.fingerprint)));
  const summary = { pass: 0, fail: 0, error: 0, skipped: 0 };
  for (const result of ruleResults) {
    if (Object.hasOwn(summary, result.status)) summary[result.status] += 1;
  }
  const rules = {
    schema_version: 1,
    source: request.rules_dir ? "scripts/ux-rules/ui" : null,
    results: ruleResults,
    summary,
  };
  await writeFile(path.join(request.out_dir, "rules.json"), `${JSON.stringify(rules, null, 2)}\n`);
  const report = {
    schema_version: 1,
    outcome: "captured",
    kit: { playwright: version, browser_version: browserVersion },
    frozen_instant: request.frozen_instant,
    captures_per_mock: request.captures_per_mock === 2 ? 2 : 1,
    mocks: entries,
    rules: "rules.json",
    rules_summary: summary,
  };
  process.stdout.write(`${JSON.stringify(report)}\n`);
}

const invokedDirectly =
  process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;
if (invokedDirectly) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}

