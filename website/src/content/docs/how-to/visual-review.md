---
title: Author visual view manifests and mocks
description: "Write the TOML view manifest for one visual view, name its mocks, run the self-check capture, and attach the mocks to a task."
---

Visual review compares a UI delivery with mock images attached to its task. You supply two things: a view manifest in the repository, which tells Orbit how to serve and render each view, and named mocks on the task, which say which view, state, theme and width each image shows. The settings that turn delivery review on are in [Visual review configuration](../../reference/config/#visual-review).

## Before you start

- A verified browser kit on the host. Check it with `orbit config browser-kit status` and install it with `orbit config browser-kit install`. See [Setup commands](../../reference/cli/#setup).
- A `[visual_review]` entry in the workspace config that lists your manifest. Paths are relative to the repository root:

  ```toml
  [visual_review]
  manifests = ["visual/views.toml"]
  ```

  Orbit parses every manifest as TOML, whatever its suffix. The `views.json` name in the configuration reference is only an example of a path.
- A way to serve your app on a loopback address for one fixture. You write that command; Orbit does not ship one.

## Write the view manifest

A manifest is one TOML file. It declares apps, which say how to start an app and which themes it supports, and views, which say how to reach one screen. Orbit merges every manifest you list. The example below declares one app and one view. Add another `[[view]]` block for each further screen.

```toml
schema_version = 1

[app.storefront]
setup = ["scripts/serve-storefront.sh", "{fixture}"]
setup_timeout_s = 120
themes = ["light", "dark"]

[[view]]
id = "product-list"
app = "storefront"
title = "Product list"
route = "#products"
fixture = "catalog"
ready = "#product-grid"
responsive = true
states = ["default", "empty", "loading", "error"]
mask = [".live-clock"]
```

`schema_version` must be `1`. The `setup` script in this example is one you write.

### App fields

| Field | Required | Meaning |
|---|---|---|
| `themes` | yes | Non-empty list, normally `light` and `dark`. The first is the theme a mock gets when its name has no theme part. |
| `setup` | yes, to capture | Command as an argument list that serves the app for one fixture. Each `{fixture}` element is replaced with the view's fixture name. |
| `setup_timeout_s` | no, `120` | Seconds the setup command has to print its URL. |
| `color_scheme_invariant` | no, `false` | Set `true` only for a single-theme app. Delivery then renders the opposite colour scheme too and records `color_scheme_leak` if the pixels differ. |

### View fields

| Field | Required | Meaning |
|---|---|---|
| `id` | yes | Lowercase letters, digits and hyphens. Mock names begin with it. It must not be a state or theme word, such as `error`, `dark` or any theme an app declares. Orbit refuses the manifest if it is. |
| `app` | yes | An `[app.<name>]` table in this manifest or in one listed before it. |
| `title` | no | Name shown in prompts and reports. |
| `route` | yes | Path or hash added to the app's URL. A `{name}` placeholder takes the value of the `ORBIT_VISUAL_<NAME>` line that setup printed. |
| `fixture` | yes | Passed to `setup` as `{fixture}`. The app decides what the name means. |
| `ready` | yes | Selector that must be visible before the capture is taken. |
| `responsive` | no, `true` | `false` allows only the default width. |
| `states` | no, `["default"]` | Any of `default`, `empty`, `loading` and `error`. |
| `clip` | no | Selector of one element to capture instead of the viewport. Use it for a view that is one panel of a page. |
| `mask` | no, `[]` | Selectors painted a flat grey before capture. Use them for content that cannot be frozen, such as live clocks. |

### The setup contract

Orbit runs `setup` as an argument list, not through a shell, with the repository root as its working directory. It runs setup once per app and fixture. Mocks that share a fixture share one server, which Orbit stops when capture ends. Setup must:

1. Bind a loopback address, such as `127.0.0.1`, on a free port.
2. Print `ORBIT_VISUAL_URL=<URL>` on stdout once the server accepts requests, then keep running.
3. Optionally print more `ORBIT_VISUAL_<NAME>=<value>` lines. Each `{name}` in a route takes the value of the matching line. With this output:

   ```text
   ORBIT_VISUAL_URL=http://127.0.0.1:41873
   ORBIT_VISUAL_PRODUCT=sku-1042
   ```

   the route `#products/{product}` resolves to `#products/sku-1042`.

4. Accept the fixture name it is given. For a view that lists `empty`, setup receives `<fixture>:empty`, such as `catalog:empty`.

If setup exits before it prints a URL, or prints none within `setup_timeout_s`, the mock fails with `setup_failed`. The message carries the last 800 characters of its stderr.

### How a view renders

Each capture opens a fresh browser page:

- **Route.** A route that starts with `#` is appended to the base URL as `/#…`. Any other route resolves against the base URL, so `/products` becomes `<base>/products`.
- **Ready.** Orbit waits up to 30 seconds for `ready` to become visible, then waits for network idle for up to 15 seconds without failing. It does not wait for `ready` in the `loading` state.
- **Theme.** The theme is the page's `prefers-color-scheme`. Orbit sets no theme class, attribute or storage key, so the app must follow the colour scheme itself.
- **Viewport.** The width is the mock's width. The height is 900 at 1440 and 812 at 375. Any other width gets its width times 10 divided by 16.
- **Determinism.** The clock is frozen at `2026-01-01T12:00:00Z` in UTC, the locale is `en-US`, reduced motion is set, and CSS animations and transitions are off. Orbit waits for fonts to load.
- **Network.** Requests to other hosts are blocked, and the report lists them. Requests other than GET, HEAD and OPTIONS are blocked during capture.

Each state changes what the page receives:

| State | What the page gets |
|---|---|
| `default` | The fixture as it is. |
| `empty` | Setup receives `<fixture>:empty`. |
| `loading` | Requests whose URL contains `/api/` never get a response, and Orbit does not wait for `ready`. |
| `error` | Requests whose URL contains `/api/` get HTTP 500 with the body `{"error":"fixture"}`. |

The `loading` and `error` states only work when the app loads its data from URLs under `/api/`.

A capture shows fixture data, not the mock's data. Pick the view whose fixture is closest to the content the mock shows. The reviewer is told that data values differ.

## Name the mocks

A mock is an image artifact under `mocks/`:

```text
mocks/<view>[.<state>][.<theme>][.<width>].<ext>
```

- `<view>` is a view id from the manifest.
- `<state>` is `default`, `empty`, `loading` or `error`. Omitted, it is `default`.
- `<theme>` is a theme the view's app declares. Omitted, it is the app's first theme.
- `<width>` is a positive whole number of CSS pixels. Omitted, it is `visual_review.default_width`, which is 1440 unless you change it.
- `<ext>` is `png`, `jpg`, `jpeg` or `webp`, in any letter case.

Parts after the view id are recognised by value, so their order does not matter. Each part may appear once. A state must be in the view's `states`, a theme must be in the app's `themes`, and a width other than the default is allowed only on a responsive view.

| Artifact | View | State | Theme | Width |
|---|---|---|---|---|
| `mocks/product-list.png` | `product-list` | `default` | `light` (first theme) | 1440 |
| `mocks/product-list.375.png` | `product-list` | `default` | `light` | 375 |
| `mocks/product-list.empty.dark.375.webp` | `product-list` | `empty` | `dark` | 375 |

The bytes must be a raster image. Export HTML or SVG mocks to PNG, JPG or WebP at the width the name gives. A mock of one panel names a view whose `clip` selects that panel.

## Attach mocks to a task

Operators, orchestrator sessions and the pilot attach mocks. An implement or review run cannot write under `mocks/`, so the implementer cannot change a mock.

1. Save each mock under the checkout's `.orbit/tmp/` directory.
2. Tag the task `verify-mock-ui`. `--tag` replaces the whole tag list, so repeat every tag the task keeps:

   ```sh
   orbit task update ORB-12345 --tag ui,verify-mock-ui   # keep the task's other tags
   ```

3. Attach each file under its mock name:

   ```sh
   orbit task artifact put ORB-12345 .orbit/tmp/product-list.375.png --path mocks/product-list.375.png
   ```

4. Check the attachments:

   ```sh
   orbit task show ORB-12345 --fields artifacts
   ```

Orbit checks the names and bytes when the task is promoted to `backlog` and again at delivery admission. It reads the manifest as committed at the delivery's base revision, so commit manifest changes before admission. Attaching the same path again replaces a mock. The delivery freezes each mock's bytes at admission, so a replacement takes effect from the next delivery.

## Run the self-check

The self-check renders your working tree beside each mock. Run it while you build:

```sh
orbit visual capture --id ORB-12345
orbit visual capture --id ORB-12345 --mock mocks/product-list.375.png --out "$ORBIT_SCRATCH_DIR/visual"
```

It needs a verified kit, a manifest that can be read from the working tree, and at least one mock on the task. `--mock` takes the full `mocks/…` path. Without `--out`, output goes to `.orbit/tmp/visual/<task-id>` in the checkout. A relative `--out` resolves against the checkout root.

For each mock, Orbit writes a directory named after the mock without `mocks/` and its extension, such as `product-list.375/`. It holds:

- `mock.png`, the mock as PNG;
- `capture.png`, the page as rendered;
- `side-by-side.png`, the mock on the left and the capture on the right, labelled.

The top of the output directory also holds `rules.json`, with the UI rule results for those captures. Add `--json` for the full report, which lists each mock's paths and any error. An error on one mock does not stop the others.

## Self-check and delivery evidence

| | Self-check, `orbit visual capture` | Delivery capture |
|---|---|---|
| When | When you run it, in any `visual_review.mode`, including `off`. | When a delivery of a `verify-mock-ui` task runs with `visual_review.mode` set to `shadow` or `live`. |
| What it renders | Your working tree, uncommitted changes included, once per mock. | The final candidate, twice per mock, to detect flaky renders. |
| Where it goes | Only the output directory. Nothing is attached or recorded on the task. | Orbit attaches `visual/capture.json` to the task, and `visual/review.json` from the comparison review. |
| What it counts as | Your own check, not evidence of record. | The capture of record. |

An implementer's handoff must not claim visual verification from a self-check run. Without a verified kit, delivery capture is recorded as not established, never as a pass. Set [`visual_review.require_established`](../../reference/config/#visual-review) to `true` to hold the delivery instead.

## Common refusals

Names and bytes, checked at promotion, at delivery admission and by the self-check:

| Code | Cause | Fix |
|---|---|---|
| `no_view_manifest` | `visual_review.manifests` is empty, or the listed manifests declare no view. | List the manifest and commit it. |
| `no_mocks` | The task has no `mocks/` artifact. | Attach the mocks. |
| `invalid_mock_name` | A part does not parse, the view part is not a lowercase hyphenated id, the extension is not allowed, or a part appears twice. | Rename the file to the pattern above. |
| `unknown_mock_view` | The view id is not in the manifest at the base revision. | Rename the mock, or add the view and commit it. |
| `undeclared_mock_variant` | The state is not in the view's `states`, the theme is not in the app's `themes`, or a non-default width is used on a `responsive = false` view. | Rename the mock, or declare the variant in the manifest. |
| `invalid_mock_image` | The bytes are not a raster image, such as an HTML or SVG export. | Export to PNG, JPG or WebP. |
| `mocks_reserved` | An implement or review run tried to write under `mocks/`. | Ask an operator in a task comment. |

Manifest loading, which fails with the manifest path at the start of the message:

| Message | Cause |
|---|---|
| `schema_version N is not supported (expected 1)` | `schema_version` is not `1`. |
| `view id … contains state or theme word …` | A view id is a state or theme word. |
| `app … themes must not be empty` | An app's `themes` list is empty. |
| `view … states must not be empty`, or `unknown state …` | `states` is empty, or names a state other than the four above. |
| `view … names unknown app …` | `app` names no `[app.<name>]` table in this manifest or an earlier one. |
| A TOML parse error | The file is not valid TOML. |

Capture, from the self-check and delivery:

| Code | Where it appears | Cause |
|---|---|---|
| `browser_kit_missing` | The command fails. | No verified kit. Run `orbit config browser-kit install`. |
| `kit_pin_mismatch` | The command fails. | The kit's Playwright does not match this release. Run `orbit config browser-kit install`. |
| `no_view_manifest` | The command fails. | A configured manifest cannot be read from the working tree, or declares no view. |
| `app … declares no setup command` | The command fails. | The view's app has no `setup`. |
| `setup_failed` | That mock's `error`. | Setup exited, or printed no `ORBIT_VISUAL_URL` within `setup_timeout_s`. |
| `render_timeout` | That mock's `error`. | The `ready` selector stayed hidden for 30 seconds, the `clip` element never appeared, or setup did not print a placeholder that a route names. |
| `image_over_budget` | That mock's `error`, delivery only. | A capture stayed over the 1 MiB artifact limit even as WebP. The self-check does not apply that limit. |

## Related

- [Visual review configuration](../../reference/config/#visual-review): the `[visual_review]` settings.
- [CLI commands](../../reference/cli/#visual-capture): `orbit visual capture` and `accept-dispute`.
- [Task artifacts](../../concepts/tasks/#task-artifacts): attaching, listing and reading artifacts.
- [Run Orbit across hosts](../multi-host/): a `verify-mock-ui` task runs only on hosts with a verified kit.
