// UI runner context: §4.2 browser settings, target expansion, evidence.
//
// One page per (view,width,theme,state) target is opened once and every
// rule that declares the target measures it. The runner sets
// ctx.currentRule around each check; a non-GET request attempted in that
// window is blocked and returned as that rule's error.

import { spawn } from "node:child_process";
import { execFile } from "node:child_process";
import { mkdir, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export const FROZEN_INSTANT = "2026-01-01T12:00:00.000Z";
const ANIMATION_OFF =
  "*,*::before,*::after{animation:none !important;transition:none !important;caret-color:transparent !important}";

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

export function uiResultOf(rule, target, status, fields = {}) {
  const measurements = { ...(fields.measurements ?? {}) };
  if (fields.selector) measurements.selector = fields.selector;
  return {
    rule: rule.id,
    target: {
      id: target.id,
      argv: [],
      view: target.view,
      width: target.width,
      theme: target.theme,
      state: target.state,
    },
    status,
    severity: rule.severity,
    measurements,
    evidence: fields.evidence ?? [],
    fingerprint: [rule.id, target.view, target.theme, String(target.width), target.state].join("|"),
  };
}

export function expandTargets(rule, manifests) {
  const wanted = rule.targets ?? {};
  const viewsWanted = wanted.views ?? "*";
  const widths = wanted.widths ?? [1440];
  const states = wanted.states ?? ["default"];
  const targets = [];
  for (const manifest of manifests) {
    for (const view of manifest.views) {
      if (viewsWanted !== "*" && !viewsWanted.includes(view.id)) continue;
      const app = manifest.apps[view.app];
      const viewWidths = view.responsive === false ? widths.slice(0, 1) : widths;
      for (const width of viewWidths) {
        for (const theme of app.themes) {
          for (const state of states) {
            if (!view.states.includes(state)) continue;
            targets.push({
              id: `${view.id}|${theme}|${width}|${state}`,
              view: view.id,
              viewDef: view,
              app: app.name,
              appDef: app,
              manifest: manifest.source,
              width,
              theme,
              state,
            });
          }
        }
      }
    }
  }
  return targets;
}

function safeName(value) {
  return String(value).replace(/[^A-Za-z0-9._-]+/g, "_").slice(0, 100);
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
  const modulePath =
    process.env.PLAYWRIGHT_MODULE ||
    path.join(process.env.HOME ?? "", ".local/chromium-deps/kit/node_modules/playwright/index.mjs");
  const mod = await import(modulePath);
  const playwright = mod.default ?? mod;
  if (!playwright.chromium) {
    throw new Error(`PLAYWRIGHT_MODULE ${modulePath} does not export chromium`);
  }
  let version = "";
  try {
    const packageUrl = new URL("./package.json", new URL(`file://${modulePath}`));
    version = JSON.parse(await (await import("node:fs/promises")).readFile(packageUrl, "utf8")).version ?? "";
  } catch {
    version = "";
  }
  return { chromium: playwright.chromium, version };
}

// Spawn an app setup command and wait for ORBIT_VISUAL_URL= on stdout.
// A plain loopback URL in stdout (astro preview) is accepted too.
// Extra ORBIT_VISUAL_<NAME>= lines resolve {name} route placeholders.
export async function startFixtureServer(app, fixture, repoRoot) {
  const argv = app.setup.map((part) => part.replaceAll("{fixture}", fixture));
  const child = spawn(argv[0], argv.slice(1), {
    cwd: repoRoot,
    env: { ...process.env, ORBIT_VISUAL_FROZEN_INSTANT: FROZEN_INSTANT },
    stdio: ["ignore", "pipe", "pipe"],
    detached: true,
  });
  const timeoutMs = (app.setup_timeout_s ?? 120) * 1000;
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
  return {
    ...found,
    stop() {
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        try {
          child.kill("SIGTERM");
        } catch {
          // Already gone.
        }
      }
    },
  };
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

export async function createUiContext(options) {
  const outDir = path.resolve(options.outDir);
  await mkdir(outDir, { recursive: true });
  const git = await execFileAsync("git", ["rev-parse", "HEAD"], {
    cwd: options.repoRoot,
    env: { ...process.env, GIT_OPTIONAL_LOCKS: "0" },
  }).catch(() => ({ stdout: "" }));
  const { chromium, version } = await loadPlaywright();
  const browser = await chromium.launch();
  const ctx = {
    outDir,
    repoRoot: options.repoRoot,
    commit: git.stdout.trim(),
    host: "",
    kit: { playwright: version, browser_revision: browser.version() },
    frozenInstant: options.frozenInstant ?? FROZEN_INSTANT,
    currentRule: "",
    blocked: [],
    page: null,
    target: null,
    diagnostics: null,
    baseUrl: "",
  };
  ctx.close = async () => {
    await browser.close().catch(() => {});
  };
  ctx.openTarget = (target, baseUrl, placeholders) =>
    openTarget(ctx, browser, target, baseUrl, placeholders);
  ctx.screenshotFor = (rule, target, result) => screenshotFor(ctx, rule, target, result);
  ctx.writeEvidence = (ruleId, targetId, label, text) =>
    writeEvidence(ctx, ruleId, targetId, label, text);
  ctx.fetchJson = async (urlPath) => {
    const response = await fetch(new URL(urlPath, ctx.baseUrl));
    const text = await response.text();
    let json = null;
    try {
      json = JSON.parse(text);
    } catch {
      json = null;
    }
    return { status: response.status, json, text: text.slice(0, 2000) };
  };
  ctx.drainBlocked = () => {
    const drained = ctx.blocked;
    ctx.blocked = [];
    return drained;
  };
  return ctx;
}

async function openTarget(ctx, browser, target, baseUrl, placeholders) {
  ctx.blocked = [];
  ctx.currentRule = "";
  const context = await browser.newContext(browserContextOptions(target));
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
      sameOrigin = new URL(url).origin === new URL(baseUrl).origin;
    } catch {
      sameOrigin = false;
    }
    if (sameOrigin) {
      diagnostics.failedRequests.push({
        url,
        failure: request.failure()?.errorText ?? "failed",
      });
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
      const record = { method, url, rule: ctx.currentRule || "page-load" };
      ctx.blocked.push(record);
      if (!ctx.currentRule) diagnostics.blockedNonGetOnLoad.push(record);
      await route.abort("blockedbyclient").catch(() => {});
      return;
    }
    if (target.state === "error" && url.includes("/api/")) {
      await route
        .fulfill({ status: 500, contentType: "application/json", body: '{"error":"fixture"}' })
        .catch(() => {});
      return;
    }
    if (target.state === "loading" && url.includes("/api/")) {
      // Held until capture: never answered on this page.
      return;
    }
    await route.continue().catch(() => {});
  });
  await page.clock.install({ time: new Date(ctx.frozenInstant) });
  await page.clock.pauseAt(new Date(ctx.frozenInstant));
  await page.addInitScript(() => {
    const style = document.createElement("style");
    style.textContent = "*,*::before,*::after{animation:none !important;transition:none !important;caret-color:transparent !important}";
    document.addEventListener("DOMContentLoaded", () => document.head.appendChild(style));
  });
  const view = target.viewDef;
  const { resolved } = resolveRoute(view.route, placeholders);
  const url = new URL(resolved.startsWith("#") ? `/${resolved}` : resolved, baseUrl).href;
  ctx.baseUrl = baseUrl;
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 60000 });
  await page.addStyleTag({ content: ANIMATION_OFF });
  // The frozen clock never advances on its own; advance it a fixed step so
  // deferred renders scheduled with timers still happen, deterministically.
  await page.clock.runFor(3000).catch(() => {});
  if (target.state !== "loading") {
    await page
      .locator(view.ready)
      .first()
      .waitFor({ state: "visible", timeout: 30000 });
    await page
      .waitForLoadState("networkidle", { timeout: 15000 })
      .catch(() => {});
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
  ctx.page = page;
  ctx.target = target;
  ctx.diagnostics = diagnostics;
  ctx.currentContext = context;
  return { page, diagnostics };
}

export async function closeTarget(ctx) {
  if (ctx.currentContext) {
    await ctx.currentContext.close().catch(() => {});
    ctx.currentContext = null;
    ctx.page = null;
  }
}

// Failing-element handles a result can carry, in preference order: an
// explicit unique `evidence_selector` a rule stamped on the measured node,
// then the failure rows `verdict` records (`selector` plus the measured
// `text`/`width_px`, which together disambiguate a non-unique selector),
// then a lone measurements selector.
export class EvidenceCaptureError extends Error {
  constructor(message, diagnostic) {
    super(message);
    this.name = "EvidenceCaptureError";
    this.code = "evidence_capture_failed";
    this.diagnostic = diagnostic;
  }
}

export function evidenceCaptureDiagnostic(result, reason, candidates) {
  return {
    status: "failed",
    kind: "evidence_capture_failed",
    reason,
    rule: result?.rule ?? "",
    fingerprint: result?.fingerprint ?? "",
    candidates: candidates.map((candidate) =>
      candidate.evidence_selector
        ? { evidence_selector: candidate.evidence_selector }
        : { selector: candidate.selector, text: candidate.text ?? "", width_px: candidate.width_px ?? null },
    ),
    action:
      "No screenshot is attached for this finding. Re-run the rule on a fresh page and confirm the measured element still resolves uniquely; the underlying finding, fingerprint and severity are preserved without screenshot proof.",
  };
}

export function failureEvidenceCandidates(result) {
  const candidates = [];
  const measurements = result?.measurements ?? {};
  if (typeof measurements.evidence_selector === "string" && measurements.evidence_selector) {
    candidates.push({ evidence_selector: measurements.evidence_selector });
  }
  if (Array.isArray(measurements.failures)) {
    for (const failure of measurements.failures) {
      if (!failure || typeof failure !== "object") continue;
      if (typeof failure.evidence_selector === "string" && failure.evidence_selector) {
        candidates.push({ evidence_selector: failure.evidence_selector });
      } else if (typeof failure.selector === "string" && failure.selector) {
        candidates.push({
          selector: failure.selector,
          text: typeof failure.text === "string" ? failure.text : "",
          width_px: typeof failure.width_px === "number" ? failure.width_px : null,
        });
      }
    }
  }
  if (candidates.length === 0 && typeof measurements.selector === "string" && measurements.selector) {
    candidates.push({ selector: measurements.selector, text: "", width_px: null });
  }
  return candidates;
}

async function resolveFailureLocator(ctx, candidate) {
  if (candidate.evidence_selector) {
    const locator = ctx.page.locator(candidate.evidence_selector);
    return (await locator.count().catch(() => 0)) === 1 ? locator : null;
  }
  const locator = ctx.page.locator(candidate.selector);
  const count = await locator.count().catch(() => 0);
  if (count === 0) return null;
  if (count === 1) return locator.first();
  if (!candidate.text && candidate.width_px == null) return null;
  const wantedText = candidate.text.replace(/\s+/g, " ").trim().slice(0, 80);
  const matches = [];
  for (let index = 0; index < count; index += 1) {
    const item = locator.nth(index);
    const facts = await item
      .evaluate((element) => ({
        text: (element.textContent ?? "").replace(/\s+/g, " ").trim().slice(0, 80),
        width: Math.round(element.getBoundingClientRect().width),
      }))
      .catch(() => null);
    if (!facts) continue;
    if (wantedText && facts.text !== wantedText) continue;
    if (candidate.width_px != null && Math.abs(facts.width - candidate.width_px) > 1) continue;
    matches.push(item);
  }
  return matches.length === 1 ? matches[0] : null;
}

async function captureScrollState(page) {
  return page.evaluateHandle(() => {
    // Every element is recorded, including zero offsets: framing can
    // scroll an ancestor that started at zero, and restoring only the
    // initially-nonzero entries would leave that scroll behind.
    const elements = [...document.querySelectorAll("*")];
    const entries = elements.map((element) => ({
      element,
      top: element.scrollTop,
      left: element.scrollLeft,
    }));
    return {
      x: window.scrollX,
      y: window.scrollY,
      scrollingElement: document.scrollingElement,
      scrollingTop: document.scrollingElement?.scrollTop ?? 0,
      scrollingLeft: document.scrollingElement?.scrollLeft ?? 0,
      entries,
    };
  });
}

async function restoreScrollState(page, saved) {
  const apply = () =>
    page
      .evaluate((state) => {
        for (const { element, top, left } of state.entries) {
          // Detached or replaced nodes must not transfer their offsets to a
          // different element that happens to occupy the same DOM position.
          if (element.isConnected && element.ownerDocument === document) {
            element.scrollTop = top;
            element.scrollLeft = left;
          }
        }
        if (state.scrollingElement?.isConnected && state.scrollingElement === document.scrollingElement) {
          state.scrollingElement.scrollTop = state.scrollingTop;
          state.scrollingElement.scrollLeft = state.scrollingLeft;
          window.scrollTo(state.x, state.y);
        }
      }, saved)
      .catch(() => {});
  try {
    await apply();
    // A smooth scroll started by framing can still be in flight and
    // overwrite the first restore. The page clock is frozen, so wait on
    // the runner's real clock and apply again once it has settled.
    await new Promise((resolve) => setTimeout(resolve, 600));
    await apply();
  } finally {
    await saved.dispose().catch(() => {});
  }
}

// Scroll the measured element so its start sits below the top of its
// scroll container, leaving the rows and headings above it in frame, then
// capture the viewport. A viewport capture of a very tall element shows
// its beginning and its neighbours; an element capture of it is an
// unreadable strip divorced from the card that contains it.
async function frameFailureInViewport(page, locator) {
  await locator.evaluate((element) => {
    const contextTopPx = 160;
    element.scrollIntoView({ block: "start", inline: "nearest", behavior: "instant" });
    let rect = element.getBoundingClientRect();
    if (Math.abs(rect.top - contextTopPx) > 1) {
      let scroller = null;
      for (let node = element.parentElement; node; node = node.parentElement) {
        const style = getComputedStyle(node);
        if (node.scrollHeight > node.clientHeight + 1 && /(auto|scroll|overlay)/.test(style.overflowY)) {
          scroller = node;
          break;
        }
      }
      const target = scroller ?? document.scrollingElement;
      if (target) target.scrollTop += rect.top - contextTopPx;
      rect = element.getBoundingClientRect();
      if (Math.abs(rect.top - contextTopPx) > 1 && document.scrollingElement && target !== document.scrollingElement) {
        document.scrollingElement.scrollTop += rect.top - contextTopPx;
      }
    }
  });
  await Promise.race([
    page.evaluate(
      () =>
        new Promise((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(resolve));
        }),
    ),
    new Promise((resolve) => setTimeout(resolve, 500)),
  ]);
}

async function removeEvidenceHandles(page) {
  await page
    .evaluate(() => {
      for (const element of document.querySelectorAll("[data-ux-evidence-target]")) {
        element.removeAttribute("data-ux-evidence-target");
      }
    })
    .catch(() => {});
}

async function screenshotFor(ctx, rule, target, result) {
  if (!ctx.page) return "";
  const directory = path.join(ctx.outDir, "evidence", rule.id);
  await mkdir(directory, { recursive: true });
  const name = `${safeName(target.view)}-${target.theme}-${target.width}-${target.state}.png`;
  const destination = path.join(directory, name);
  const view = target.viewDef;
  // When the result identifies the measured element, frame that element
  // with its surrounding context instead of the manifest clip, which is
  // chosen for whole-view captures and can omit or stretch the defect.
  // Any scroll changed for framing is restored before returning, so later
  // rules measuring the same page see the state the load left.
  const candidates = failureEvidenceCandidates(result);
  let resolvedAny = false;
  let lastCaptureError = null;
  for (const candidate of candidates) {
    const locator = await resolveFailureLocator(ctx, candidate).catch(() => null);
    if (!locator) continue;
    resolvedAny = true;
    const saved = await captureScrollState(ctx.page).catch(() => null);
    try {
      await frameFailureInViewport(ctx.page, locator);
      await ctx.page.screenshot({ path: destination });
      return path.posix.join("evidence", rule.id, name);
    } catch (error) {
      lastCaptureError = error;
    } finally {
      if (saved) await restoreScrollState(ctx.page, saved);
      await removeEvidenceHandles(ctx.page);
    }
  }
  if (candidates.length > 0) {
    // An explicit measured target was named but could not be framed.
    // Returning the manifest clip here would present an unrelated image
    // as proof of this finding, so fail explicitly instead: remove any
    // partial or stale file at the destination, clean up, and throw a
    // diagnostic the runner retains. The finding itself is untouched.
    await removeEvidenceHandles(ctx.page);
    await rm(destination, { force: true }).catch(() => {});
    const reason = resolvedAny
      ? `targeted viewport capture failed: ${lastCaptureError instanceof Error ? lastCaptureError.message : String(lastCaptureError ?? "unknown error")}`
      : "measured evidence target did not resolve to exactly one element";
    throw new EvidenceCaptureError(
      `evidence capture failed for ${rule.id} on ${target.view}-${target.theme}-${target.width}-${target.state}: ${reason}; no screenshot attached`,
      evidenceCaptureDiagnostic(result, reason, candidates),
    );
  }
  // Genuinely untargeted result: no measured element was named, so the
  // whole-view manifest clip (or viewport) remains the right capture.
  // Temporary handles from the measurement must not outlive the capture.
  await removeEvidenceHandles(ctx.page);
  // An element capture scrolls its clip into view; restore afterwards so
  // the fallback path leaves scroll state as it found it too.
  const fallbackSaved = await captureScrollState(ctx.page).catch(() => null);
  try {
    if (view?.clip) {
      await ctx.page.locator(view.clip).first().screenshot({ path: destination }).catch(async () => {
        await ctx.page.screenshot({ path: destination });
      });
    } else {
      await ctx.page.screenshot({ path: destination });
    }
  } finally {
    if (fallbackSaved) await restoreScrollState(ctx.page, fallbackSaved);
  }
  return path.posix.join("evidence", rule.id, name);
}

// Attach a failure screenshot, or retain an explicit capture diagnostic.
// On success the PNG path joins `result.evidence`. On failure no PNG is
// attached: the diagnostic is recorded in `result.measurements` and as a
// text evidence file, so the exported evidence never claims a screenshot
// that does not frame the measured element. Status, fingerprint and
// severity of the underlying finding are never changed here.
export async function attachFailureScreenshot(ctx, rule, target, result) {
  try {
    const shot = await screenshotFor(ctx, rule, target, result);
    if (shot && !result.evidence.includes(shot)) result.evidence.push(shot);
    return shot;
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    const diagnostic =
      error?.diagnostic ?? evidenceCaptureDiagnostic(result, message, failureEvidenceCandidates(result));
    result.measurements = { ...result.measurements, evidence_capture: diagnostic };
    try {
      const write = ctx.writeEvidence
        ? (label, text) => ctx.writeEvidence(rule.id, target.id, label, text)
        : (label, text) => writeEvidence(ctx, rule.id, target.id, label, text);
      const text = [
        `evidence capture failed for ${rule.id} on ${target.id}`,
        `fingerprint: ${result.fingerprint}`,
        `reason: ${diagnostic.reason}`,
        `action: ${diagnostic.action}`,
        "",
      ].join("\n");
      const diagPath = await write("evidence-capture-failed", text);
      if (diagPath && !result.evidence.includes(diagPath)) result.evidence.push(diagPath);
    } catch {
      result.measurements = {
        ...result.measurements,
        evidence_capture: { ...diagnostic, diagnostic_write_failed: true },
      };
    }
    return "";
  }
}

// Shared in-page measurement helpers, injected as a preamble so every
// rule measures visibility, geometry and selectors the same way.
const MEASURE_PREAMBLE = `
const H = {
  px(value) {
    const parsed = Number.parseFloat(value);
    return Number.isFinite(parsed) ? parsed : 0;
  },
  visible(element) {
    if (!(element instanceof Element)) return false;
    const style = getComputedStyle(element);
    if (style.display === "none" || style.visibility === "hidden") return false;
    const rect = element.getBoundingClientRect();
    return rect.width > 0 && rect.height > 0;
  },
  rect(element) {
    const r = element.getBoundingClientRect();
    return { top: r.top, left: r.left, right: r.right, bottom: r.bottom, width: r.width, height: r.height };
  },
  selectorOf(element) {
    if (element.id) return "#" + element.id;
    const key = element.getAttribute("data-key");
    if (key) return '[data-key="' + key + '"]';
    const classes = typeof element.className === "string"
      ? element.className.trim().split(/\\s+/).filter(Boolean).slice(0, 2)
      : [];
    return element.tagName.toLowerCase() + (classes.length ? "." + classes.join(".") : "");
  },
  activeRoot() {
    const panes = [...document.querySelectorAll(".tab-pane")].filter((pane) => H.visible(pane));
    return panes[panes.length - 1] ?? document.body;
  },
  all(selector, root) {
    return [...(root ?? H.activeRoot()).querySelectorAll(selector)].filter((element) => H.visible(element));
  },
};
`;

export async function measureInPage(ctx, body) {
  return ctx.page.evaluate(`(() => { ${MEASURE_PREAMBLE}\n return (${body.toString()})(H); })()`);
}

export function verdict(ctx, rule, matched, failures, extra = {}) {
  const target = ctx.target;
  if (matched === 0) {
    return uiResultOf(rule, target, "skipped", {
      measurements: { matched: 0, reason: "no element matched on this target", ...extra },
    });
  }
  if (failures.length > 0) {
    return uiResultOf(rule, target, "fail", {
      measurements: { matched, failures: failures.slice(0, 20), ...extra },
    });
  }
  return uiResultOf(rule, target, "pass", {
    measurements: { matched, ...extra },
  });
}

async function writeEvidence(ctx, ruleId, targetId, label, text) {
  const directory = path.join(ctx.outDir, "evidence", ruleId);
  await mkdir(directory, { recursive: true });
  const name = `${safeName(targetId)}-${safeName(label)}.txt`;
  await writeFile(path.join(directory, name), text);
  return path.posix.join("evidence", ruleId, name);
}
