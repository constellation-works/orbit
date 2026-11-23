# Configuration: executor image input

## Contents

- [Executor image input (`image_input`)](#executor-image-input-image_input)
  - [Declare it on the executor](#declare-it-on-the-executor)
  - [Opt a crew out with `vision = false`](#opt-a-crew-out-with-vision--false)
  - [The canary and the visual hold](#the-canary-and-the-visual-hold)
  - [Probe with `orbit agents vision-probe`](#probe-with-orbit-agents-vision-probe)

## Executor image input (`image_input`)

Image input lets a crew's model read image files. Any activity whose input
asks for images needs it. Images never travel in prompt text: Orbit stages each one as a
file in the run's scratch directory, and the executor presents it to the model.

### Declare it on the executor

Declare `spec.image_input` in the executor's installed definition, the YAML file
`resources/executors/<executor>.yaml` under the Orbit home (`~/.orbit`), beside
`sandbox` and `model_flag`. Like the other executor settings, it is not in
`config.toml`, so `orbit config` does not show it.

```yaml
spec:
  image_input:
    mode: attach_args          # or native_read
    formats: [png, jpeg, webp] # png is required: it is the canary's format
    max_images: 40             # canary included; at least 2
    attach_args: ["--image", "{path}"]   # attach_args mode only
```

- `mode: native_read` presents the staged images through the CLI's own
  file-read tool. The prompt lists their paths and tells the agent to read each
  one. It takes no `attach_args`.
- `mode: attach_args` appends `attach_args` to the argv once per image, with
  `{path}` replaced by that image's staged path. Only scratch paths reach argv,
  never image content. The template must contain `{path}`.
- `formats` must include `png` and must not repeat a format.
- `max_images` is at least 2, and it counts the canary.

A definition with an invalid declaration fails to load with
`invalid spec.image_input: …`, naming the problem. An unknown key is refused the
same way.

**Omission and inheritance.** An executor that omits `image_input` cannot take
images. A request for images on a crew that uses it fails before launch with the
non-retryable `image_input_unavailable` error, which names the executor and
`spec.image_input`. An installed definition that omits the field takes the
shipped default executor's declaration of the same name, so existing installs
inherit it without reseeding. A value set on the installed definition wins.

The shipped declarations are:

- **claude**: `native_read`, for PNG, JPEG, WebP and GIF.
- **codex**: `attach_args` with `--image {path}`, for PNG, JPEG and WebP.
- **muse**: `attach_args` with `--image {path}`, for PNG.
- **gemini**, **antigravity**, **grok** and **copilot** declare none.

Each shipped declaration rests on a lane that passed the canary probe below.
To test a lane that declares none, add a candidate declaration to its installed
definition, probe that crew, and keep the declaration only if its row reads `seen`.

### Opt a crew out with `vision = false`

A crew can take images only when its executor declares `image_input` and the
crew's `vision` is not `false`. `vision` defaults to `true`. Set `vision = false`
on a crew whose model keeps returning `image_not_seen` while the same executor
passes for other crews, such as a text-only model:

```bash
orbit config set crews.<crew>.vision false   # set it back to true to re-enable
```

A `vision = false` crew still does ordinary work. Its image requests fail with
`image_input_unavailable` and its probe row reads `vision_off`.

### The canary and the visual hold

Every image-input invocation first gets a canary: a generated PNG that shows a
random six-character code. Orbit creates it for that invocation in scratch and
never writes it to a task. The agent must return the code as `result.canary`.

- A delivery result without the code, or with another code, is a canary miss:
  a retryable `[image_not_seen]` step failure. The image-input probe records
  the same miss on that crew's row and the probe run still completes, so one
  crew does not fail the probe batch. Each check emits an
  `activity.image_canary_checked` audit event with the crew, the mode and whether
  the image was seen. The canary proves the image arrived, not that the model
  judged it well.
- A crew whose newest two or more canary checks all missed shows a visual
  hold in `orbit agents vision-probe` until 30 minutes after its last miss. Each further consecutive miss
  doubles the hold, up to 24 hours. The next passing check clears it. The hold
  is read from the audit events, so probes and deliveries both count.

### Probe with `orbit agents vision-probe`

```bash
orbit agents vision-probe                         # every enabled crew
orbit agents vision-probe --crew <crew>           # repeatable: only the named crews
orbit agents vision-probe --timeout-seconds 900   # the default wait
```

The probe runs one canary per crew outside any delivery and waits for each run
to settle. Without `--crew`, every enabled crew gets a row, including crews that
cannot take images; those rows give the reason. A crew named with `--crew` that
cannot take images fails the command before anything launches, so fix its
declaration or leave it out.

Each row has these columns:

- `CREW`, `EXECUTOR` and `MODEL` name the crew and what runs it.
- `MODE` is the executor's declared mode, or `-` when it declares none.
- `RESULT` is the outcome, described below.
- `HOLD` reads `N misses, until <time>` while the crew is held, else `-`.
- `DETAIL` carries the error text, when there is one.

The `--json` output adds the `run_id` of each probe run that launched; read a
run with `orbit run show <run_id>`.

| RESULT | Meaning | What to do |
|---|---|---|
| `seen` | The model returned the canary code. | Keep the declaration. The crew can take images. |
| `image_not_seen` | The run finished without the code. The probe run itself still completes. | If the same model keeps missing, set `vision = false` on that crew. |
| `no_image_input` | The crew's executor declares no `image_input`. | Add a candidate declaration as above, or leave the crew out of image work. |
| `vision_off` | The crew sets `vision = false`, so it was not probed. | Set `vision` back to `true` and probe again. |
| `no_cli_runtime` | The crew's provider has no CLI runtime on this host. | Check `orbit doctor providers`, then follow [Provider setup](providers.md#provider-setup). |
| `failed` | The run failed before the canary was checked. | Read the run with `orbit run show <run_id>` and fix what it reports. |
| `timeout` | The run had not settled when the probe stopped waiting. | Probe again with a larger `--timeout-seconds`. If it still times out, read the run with `orbit run show <run_id>`. |
