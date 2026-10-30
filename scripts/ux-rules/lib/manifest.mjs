// View manifest loader (visual-review 2_design.md §3.1).
//
// A deliberately small TOML reader for the manifest subset: top-level
// scalars, [app.<name>] tables and [[view]] arrays whose values are
// strings, integers, booleans or string arrays. Anything outside that
// subset is a load error, not a guess.

import { readFile } from "node:fs/promises";

export const STATE_WORDS = ["default", "empty", "loading", "error"];
const VIEW_ID = /^[a-z0-9]+(?:-[a-z0-9]+)*(?:\.[a-z0-9]+(?:-[a-z0-9]+)*)*$/;

function fail(source, message) {
  throw new Error(`${source}: ${message}`);
}

function parseValue(raw, source, where) {
  const text = raw.trim();
  if (text.startsWith('"')) {
    try {
      const value = JSON.parse(text);
      if (typeof value !== "string") fail(source, `${where}: expected a string`);
      return value;
    } catch {
      fail(source, `${where}: invalid quoted string ${text}`);
    }
  }
  if (text === "true") return true;
  if (text === "false") return false;
  if (/^-?\d+$/.test(text)) return Number(text);
  if (text.startsWith("[")) {
    if (!text.endsWith("]")) fail(source, `${where}: unterminated array`);
    const inner = text.slice(1, -1).trim();
    if (inner === "") return [];
    const items = [];
    let depth = 0;
    let inString = false;
    let current = "";
    for (const ch of inner) {
      if (inString) {
        current += ch;
        if (ch === '"') inString = false;
      } else if (ch === '"') {
        inString = true;
        current += ch;
      } else if (ch === "[") {
        depth += 1;
        current += ch;
      } else if (ch === "]") {
        depth -= 1;
        current += ch;
      } else if (ch === "," && depth === 0) {
        items.push(current);
        current = "";
      } else {
        current += ch;
      }
    }
    if (current.trim() !== "") items.push(current);
    return items.map((item) => parseValue(item, source, where));
  }
  fail(source, `${where}: unsupported value ${text}`);
}

function stripComment(line) {
  let inString = false;
  for (let index = 0; index < line.length; index += 1) {
    const ch = line[index];
    if (ch === '"') inString = !inString;
    if (ch === "#" && !inString) return line.slice(0, index);
  }
  return line;
}

export function parseManifest(text, source) {
  const manifest = { schema_version: undefined, apps: {}, views: [], source };
  let current = manifest;
  const lines = text.split("\n");
  let pending = "";
  const logical = [];
  for (const rawLine of lines) {
    const line = stripComment(rawLine).trim();
    if (pending !== "") {
      pending = `${pending} ${line}`;
      if (line.includes("]") || !pending.includes("[")) {
        logical.push(pending);
        pending = "";
      }
      continue;
    }
    if (line === "") continue;
    const valuePart = line.includes("=") ? line.slice(line.indexOf("=") + 1) : "";
    if (valuePart.trim().startsWith("[") && !valuePart.trim().endsWith("]")) {
      pending = line;
      continue;
    }
    logical.push(line);
  }
  if (pending !== "") fail(source, "unterminated array at end of file");
  for (const line of logical) {
    const appMatch = line.match(/^\[app\.([A-Za-z0-9_-]+)\]$/);
    if (appMatch) {
      const name = appMatch[1];
      if (manifest.apps[name]) fail(source, `duplicate app ${name}`);
      manifest.apps[name] = { name };
      current = manifest.apps[name];
      continue;
    }
    if (line === "[[view]]") {
      const view = {};
      manifest.views.push(view);
      current = view;
      continue;
    }
    if (line.startsWith("[")) fail(source, `unsupported table header ${line}`);
    const eq = line.indexOf("=");
    if (eq < 1) fail(source, `expected key = value, got ${line}`);
    const key = line.slice(0, eq).trim();
    if (!/^[a-z_][a-z0-9_]*$/.test(key)) fail(source, `unsupported key ${key}`);
    current[key] = parseValue(line.slice(eq + 1), source, key);
  }
  validateManifest(manifest);
  return manifest;
}

function isStringArray(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function validateManifest(manifest) {
  const source = manifest.source;
  if (manifest.schema_version !== 1) {
    fail(source, `schema_version must be 1, got ${String(manifest.schema_version)}`);
  }
  const apps = Object.values(manifest.apps);
  if (apps.length === 0) fail(source, "manifest declares no app");
  const themeWords = new Set(["light", "dark"]);
  for (const app of apps) {
    if (!isStringArray(app.setup) || app.setup.length === 0) {
      fail(source, `app ${app.name}: setup must be a non-empty string array`);
    }
    if (!isStringArray(app.themes) || app.themes.length === 0) {
      fail(source, `app ${app.name}: themes must be a non-empty string array`);
    }
    for (const theme of app.themes) themeWords.add(theme);
    if (app.color_scheme_invariant === undefined) app.color_scheme_invariant = false;
    if (typeof app.color_scheme_invariant !== "boolean") {
      fail(source, `app ${app.name}: color_scheme_invariant must be a boolean`);
    }
    if (app.design_rules !== undefined && !isStringArray(app.design_rules)) {
      fail(source, `app ${app.name}: design_rules must be a string array`);
    }
    if (app.tokens !== undefined && !isStringArray(app.tokens)) {
      fail(source, `app ${app.name}: tokens must be a string array`);
    }
    if (
      app.setup_timeout_s !== undefined &&
      (!Number.isInteger(app.setup_timeout_s) || app.setup_timeout_s <= 0)
    ) {
      fail(source, `app ${app.name}: setup_timeout_s must be a positive integer`);
    }
  }
  if (manifest.views.length === 0) fail(source, "manifest declares no view");
  const seen = new Set();
  for (const view of manifest.views) {
    for (const field of ["id", "app", "title", "route", "fixture", "ready"]) {
      if (typeof view[field] !== "string" || view[field] === "") {
        fail(source, `view ${String(view.id ?? "?")}: ${field} is required`);
      }
    }
    if (!VIEW_ID.test(view.id)) {
      fail(source, `view id ${view.id} must be lowercase hyphenated dot-separated parts`);
    }
    for (const part of view.id.split(".")) {
      if (STATE_WORDS.includes(part) || themeWords.has(part)) {
        fail(
          source,
          `view id ${view.id} contains state or theme word "${part}" as a dot-separated part`,
        );
      }
    }
    if (seen.has(view.id)) fail(source, `duplicate view id ${view.id}`);
    seen.add(view.id);
    if (!manifest.apps[view.app]) {
      fail(source, `view ${view.id}: unknown app ${view.app}`);
    }
    if (view.responsive === undefined) view.responsive = true;
    if (typeof view.responsive !== "boolean") {
      fail(source, `view ${view.id}: responsive must be a boolean`);
    }
    if (view.states === undefined) view.states = ["default"];
    if (!isStringArray(view.states) || view.states.length === 0) {
      fail(source, `view ${view.id}: states must be a non-empty string array`);
    }
    for (const state of view.states) {
      if (!STATE_WORDS.includes(state)) {
        fail(source, `view ${view.id}: unknown state ${state}`);
      }
    }
    if (view.clip !== undefined && typeof view.clip !== "string") {
      fail(source, `view ${view.id}: clip must be a selector string`);
    }
    if (view.mask === undefined) view.mask = [];
    if (!isStringArray(view.mask)) {
      fail(source, `view ${view.id}: mask must be a string array`);
    }
  }
}

export async function loadManifest(manifestPath) {
  const text = await readFile(manifestPath, "utf8");
  return parseManifest(text, manifestPath);
}
