// The Orbit mod for Claude Code: a band above the prompt, a status line,
// toasts, and the Board / Ship / Map pane, all read from the `orbit` CLI;
// plus two hooks: task cards for mentioned task ids, and a `Task:` trailer
// on commits made while this session works a task.
//
// Every function that takes `$` lives in this file: the engine follows `$`
// only into functions declared beside the hook that holds it.

import { atom, read, update } from 'claude-code'
import type { EngineInterface, ProcessRunResult, Register, Timer } from 'claude-code'

import type { OrbitShip, OrbitView } from '../../types'
import { argvFor, failure, localArgv, OrbitError, ownerHost, readShown, reason, targetFrom, type Target } from './cli'
import {
  card,
  changes,
  findRunId,
  mentions,
  parseRunEvents,
  parseTasks,
  PIPELINE,
  preflight,
  recentlyDone,
  statusLine,
  stepStates,
  toastFor,
  withTaskTrailer,
} from './model'
import { band } from './views/band'
import type { Actions } from './views/kit'
import { pane } from './views/pane'

const PANE = 'orbit'
const AFTER_TURN_MIN_MS = 30_000
const RETRY_EMPTY_MS = 5000
const SHIP_POLL_MS = 5000
const TASK_UPDATE = /(^|__)orbit_task_update$/
const TASK_ID = /^[A-Z][A-Z0-9]{1,11}-\d{1,9}$/
const SUCCESS = new Set(['success', 'succeeded'])
const FINISHED = new Set([...SUCCESS, 'failed', 'timeout', 'cancelled', 'interrupted'])
const TITLES: Record<OrbitView, string> = { board: 'Orbit · Board', ship: 'Orbit · Ship', map: 'Orbit · Map' }

const COMMANDS = [
  { name: 'orbit-board', description: 'Open the Orbit board for this workspace', argumentHint: '[task id]' },
  { name: 'orbit-ship', description: 'Preflight and ship an Orbit task through the PR pipeline', argumentHint: '[task id]' },
  { name: 'orbit-map', description: 'Open the Orbit orbital map of open tasks' },
  { name: 'orbit-band', description: 'Show or hide the Orbit band above the prompt' },
]

type Settings = { host: string | null; refreshMs: number; commitTrailer: boolean; band: 'on' | 'compact' | 'off' }

const isInFlight = (ship: OrbitShip | null): boolean => ship !== null && (ship.phase === 'launching' || ship.phase === 'flying')

// The session values the mod keeps with Claude Code, declared in
// plugin/types/index.d.ts; a reload of the module keeps them.
const snapshotAtom = atom({ plugin: 'orbit', key: 'snapshot' } as const, null)
const errorAtom = atom({ plugin: 'orbit', key: 'error' } as const, null)
const isBandHiddenAtom = atom({ plugin: 'orbit', key: 'isBandHidden' } as const, false)
const activeAtom = atom({ plugin: 'orbit', key: 'active' } as const, null)
const shipAtom = atom({ plugin: 'orbit', key: 'ship' } as const, null)
const viewAtom = atom({ plugin: 'orbit', key: 'view' } as const, 'board')
const selectedAtom = atom({ plugin: 'orbit', key: 'selected' } as const, null)
const flashAtom = atom({ plugin: 'orbit', key: 'flash' } as const, null)

let settings: Settings = { host: null, refreshMs: 180_000, commitTrailer: true, band: 'on' }
let configError: string | null = null
let cwd = ''
let target: Target | null = null
let locating: Promise<Target> | null = null
let inFlight = false
let lastRefreshAt = 0
let shipTimer: Timer | null = null

/** Which workspace this session's checkout belongs to, and where its tasks are read. */
async function locate($: EngineInterface): Promise<Target> {
  if (target !== null) return target
  locating ??= findTarget($).finally(() => {
    locating = null
  })
  return locating
}

async function findTarget($: EngineInterface): Promise<Target> {
  let shown: { name: string; role: string } | null = null
  try {
    const ran = await $.process.run(localArgv(['workspace', 'show', '--format', 'json']), { cwd, timeoutMs: 10_000 })
    if (ran.exitCode === 0) shown = readShown(ran.stdout)
  } catch {
    shown = null
  }
  let root = cwd
  if (shown === null && settings.host !== null) {
    const top = await $.process.run(['git', 'rev-parse', '--show-toplevel'], { cwd, timeoutMs: 5000 }).catch(() => null)
    if (top !== null && top.exitCode === 0) root = top.stdout.trim()
  }
  target = targetFrom(shown, cwd, settings.host, root)
  return target
}

async function orbit($: EngineInterface, args: readonly string[], timeoutMs = 20_000): Promise<ProcessRunResult> {
  const where = await locate($)
  const ran = await $.process.run(argvFor(where, args), { cwd: where.cwd, timeoutMs })
  const failed = failure(where, args, ran.exitCode, ran.stdout, ran.stderr)
  if (failed !== null) throw new OrbitError(failed)
  return ran
}

async function publishStatus($: EngineInterface): Promise<void> {
  $.ui.status(statusLine(await read($, snapshotAtom), await read($, activeAtom), await read($, shipAtom)))
}

/** Reads the workspace's open tasks and recent completions; toasts what moved since the last read. */
async function refresh($: EngineInterface): Promise<void> {
  if (inFlight || cwd === '' || configError !== null) return
  inFlight = true
  lastRefreshAt = await $.clock.now()
  try {
    const [open, done] = await Promise.all([
      orbit($, ['task', 'list', '--status', 'proposed,backlog,in-progress,review,blocked', '--json', '--limit', '500']),
      orbit($, ['task', 'list', '--status', 'done', '--json', '--limit', '40']),
    ])
    const where = await locate($)
    const at = await $.clock.now()
    const next = { workspace: where.workspace, host: where.host, tasks: parseTasks(open.stdout), doneRecently: recentlyDone(parseTasks(done.stdout), at), at }
    const before = await read($, snapshotAtom)
    for (const change of changes(before, next)) $.ui.toast(toastFor(change), { timeoutMs: 6000 })
    await update($, snapshotAtom, () => next)
    await update($, errorAtom, () => null)
  } catch (thrown) {
    target = null
    await update($, errorAtom, () => reason(thrown))
  } finally {
    inFlight = false
    await publishStatus($)
  }
}

/**
 * Reads the workspace when nothing has been read yet. The read session.start
 * starts can finish before the session is bound and its write go nowhere, so
 * the band, the pane and the commands each ask again while the snapshot is empty.
 */
async function ensureLoaded($: EngineInterface): Promise<void> {
  if (inFlight || configError !== null) return
  if ((await read($, snapshotAtom)) !== null || (await read($, errorAtom)) !== null) return
  if ((await $.clock.now()) - lastRefreshAt < RETRY_EMPTY_MS) return
  void refresh($)
}

async function say($: EngineInterface, text: string | null): Promise<void> {
  await update($, flashAtom, () => text)
}

async function confirm($: EngineInterface, question: string, yes: string): Promise<boolean> {
  try {
    return (await $.ui.ask(question, [yes, 'Cancel'])) === yes
  } catch {
    return false
  }
}

async function write($: EngineInterface, args: string[], done: string): Promise<void> {
  try {
    await orbit($, args)
    await say($, done)
  } catch (thrown) {
    await say($, `Failed: ${reason(thrown)}`)
  }
  await refresh($)
}

async function openPane($: EngineInterface, next: OrbitView): Promise<boolean> {
  await update($, viewAtom, () => next)
  const opened = await $.ui.open({ id: PANE, title: TITLES[next] })
  return opened.isPlaced
}

async function setActive($: EngineInterface, taskId: string | null): Promise<void> {
  await update($, activeAtom, () => taskId)
  await publishStatus($)
}

/** Loads a task's preflight into the Ship view; a ship in flight stays put. */
async function prepareShip($: EngineInterface, taskId: string): Promise<void> {
  const flying = await read($, shipAtom)
  if (flying !== null && isInFlight(flying)) {
    await say($, `${flying.taskId} is still in flight; let it land before preparing another.`)
    return
  }
  const snap = await read($, snapshotAtom)
  if (snap === null) return
  let readiness: unknown = null
  try {
    readiness = JSON.parse((await orbit($, ['run', 'readiness', taskId, '--json'])).stdout)
  } catch {
    readiness = null
  }
  const pending: OrbitShip = {
    taskId,
    phase: 'preflight',
    runId: null,
    checks: preflight(snap.tasks.find(item => item.id === taskId), snap, readiness).checks,
    steps: PIPELINE.map(() => 'pending'),
    startedAt: null,
    finishedAt: null,
    message: null,
  }
  await update($, shipAtom, () => pending)
  await update($, selectedAtom, () => taskId)
}

/** `orbit run ship <id>` once the person confirms; then the track follows the run. */
async function launch($: EngineInterface): Promise<void> {
  const ready = await read($, shipAtom)
  if (ready === null || ready.phase !== 'preflight') return
  if (!(await confirm($, `Ship ${ready.taskId} through the PR pipeline?`, 'Ship it'))) return
  const now = await $.clock.now()
  await update($, shipAtom, (prior): OrbitShip | null => (prior ? { ...prior, phase: 'launching', startedAt: now, message: null } : prior))
  await publishStatus($)
  try {
    const ran = await orbit($, ['run', 'ship', ready.taskId, '--json'], 60_000)
    const runId = findRunId(ran.stdout)
    await update($, shipAtom, (prior): OrbitShip | null => (prior ? { ...prior, phase: 'flying', runId, message: runId ? null : 'Launched; Orbit reported no run id to follow.' } : prior))
    if (runId !== null) watchShip($)
  } catch (thrown) {
    await update($, shipAtom, (prior): OrbitShip | null => (prior ? { ...prior, phase: 'failed', message: reason(thrown) } : prior))
  }
  await publishStatus($)
}

/** Follows an existing run of a task on the Ship view. */
async function track($: EngineInterface, taskId: string, runId: string): Promise<void> {
  const now = await $.clock.now()
  const tracked: OrbitShip = { taskId, phase: 'flying', runId, checks: [], steps: PIPELINE.map(() => 'pending'), startedAt: now, finishedAt: null, message: null }
  await update($, shipAtom, () => tracked)
  watchShip($)
  await pollShip($)
}

function watchShip($: EngineInterface): void {
  shipTimer?.cancel()
  shipTimer = $.clock.every(SHIP_POLL_MS, () => void pollShip($))
}

async function pollShip($: EngineInterface): Promise<void> {
  const flying = await read($, shipAtom)
  if (flying === null || flying.runId === null || flying.phase !== 'flying') {
    shipTimer?.cancel()
    shipTimer = null
    return
  }
  const runId = flying.runId
  try {
    const [shown, events] = await Promise.all([orbit($, ['run', 'show', runId, '--json']), orbit($, ['run', 'events', runId, '--json'])])
    const run = (JSON.parse(shown.stdout) as { run?: { state?: unknown; error_message?: unknown; started_at?: unknown } }).run ?? {}
    const runState = typeof run.state === 'string' ? run.state : 'running'
    const isOver = FINISHED.has(runState)
    const isFailed = isOver && !SUCCESS.has(runState)
    const steps = stepStates(parseRunEvents(events.stdout), isFailed)
    const now = await $.clock.now()
    const started = typeof run.started_at === 'string' ? Date.parse(run.started_at) : Number.NaN
    const message = isFailed && typeof run.error_message === 'string' ? (run.error_message.split('\n')[0] ?? '').slice(0, 240) : null
    await update($, shipAtom, (prior): OrbitShip | null =>
      prior && prior.runId === runId
        ? {
            ...prior,
            steps,
            startedAt: Number.isFinite(started) ? started : prior.startedAt,
            phase: isOver ? (isFailed ? 'failed' : 'landed') : 'flying',
            finishedAt: isOver ? now : null,
            message,
          }
        : prior,
    )
    if (isOver) {
      shipTimer?.cancel()
      shipTimer = null
      $.ui.toast(isFailed ? `✗ ${flying.taskId} ship ${runState} · ${runId}` : `✓ ${flying.taskId} landed · ${runId}`, { timeoutMs: 8000 })
      void refresh($)
    }
  } catch (thrown) {
    await update($, shipAtom, (prior): OrbitShip | null => (prior && prior.runId === runId ? { ...prior, message: `Tracking paused: ${reason(thrown)}` } : prior))
  }
  await publishStatus($)
}

/** The handlers a drawing binds to its Buttons: each a press, never run unasked. */
function actionsFor($: EngineInterface): Actions {
  return {
    refresh: () => void refresh($),
    setView: next => void openPane($, next),
    select: taskId => void update($, selectedAtom, () => taskId),
    clearFlash: () => void say($, null),
    hideBand: () => void update($, isBandHiddenAtom, () => true),
    closePane: () => void $.ui.close({ id: PANE }),
    approve: taskId => void write($, ['task', 'update', taskId, '--approve'], `${taskId} approved: proposed → backlog`),
    accept: async taskId => {
      if (await confirm($, `Accept ${taskId} and mark it done?`, 'Mark done')) await write($, ['task', 'update', taskId, '--approve'], `${taskId} accepted: review → done`)
    },
    reject: async taskId => {
      if (await confirm($, `Reject ${taskId}?`, 'Reject')) await write($, ['task', 'update', taskId, '--status', 'rejected'], `${taskId} rejected`)
    },
    workHere: async taskId => {
      if (!TASK_ID.test(taskId)) return
      await setActive($, taskId)
      await say($, `Working on ${taskId} in this session.`)
      await $.prompt.submit({ text: `Pick up Orbit task ${taskId} in this session: read it, start it with the orbit skill, and work it to review.` })
    },
    rescue: async taskId => {
      if (!TASK_ID.test(taskId)) return
      await setActive($, taskId)
      await say($, `Rescuing ${taskId} in this session.`)
      await $.prompt.submit({ text: `Orbit task ${taskId} is blocked. Diagnose its last run with the orbit-orchestrate skill, then finish the work here and hand it to review.` })
    },
    ship: async taskId => {
      await openPane($, 'ship')
      await prepareShip($, taskId)
    },
    launch: () => void launch($),
    track: async (taskId, runId) => {
      await openPane($, 'ship')
      await track($, taskId, runId)
    },
    resetShip: () => void update($, shipAtom, (prior): OrbitShip | null => (isInFlight(prior) ? prior : null)),
  }
}

export const register: Register = (on, options) => {
  try {
    settings = {
      host: ownerHost(options.ownerHost),
      refreshMs: Math.max(1, Number(options.refreshMinutes) || 3) * 60_000,
      commitTrailer: options.commitTrailer !== false,
      band: options.band === 'compact' || options.band === 'off' ? options.band : 'on',
    }
  } catch (thrown) {
    configError = reason(thrown)
  }

  on('session.start', async ($, e, next) => {
    const started = await next(e)
    for (const command of COMMANDS) await $.command.register(command)
    if (configError !== null) {
      $.ui.toast(`orbit: ${configError}`)
      return started
    }
    cwd = e.cwd
    target = null
    void refresh($)
    $.clock.every(settings.refreshMs, () => void refresh($))
    if (isInFlight(await read($, shipAtom))) watchShip($)
    return started
  })

  on('turn.complete', async ($, e, next) => {
    const result = await next(e)
    if ((await read($, snapshotAtom)) === null || (await $.clock.now()) - lastRefreshAt > AFTER_TURN_MIN_MS) void refresh($)
    return result
  })

  on('command.run', { command: 'orbit-board' }, async ($, e) => {
    const id = e.args.trim()
    if (TASK_ID.test(id)) await update($, selectedAtom, () => id)
    const isPlaced = await openPane($, 'board')
    await ensureLoaded($)
    return { text: isPlaced ? 'Orbit board opened.' : 'The Orbit board opens once the terminal is wide enough.' }
  })

  on('command.run', { command: 'orbit-ship' }, async ($, e) => {
    const id = e.args.trim()
    await openPane($, 'ship')
    await ensureLoaded($)
    if (!TASK_ID.test(id)) return { text: 'Orbit ship pane opened.' }
    await prepareShip($, id)
    return { text: `Ship preflight for ${id} is in the Orbit pane.` }
  })

  on('command.run', { command: 'orbit-map' }, async $ => {
    await openPane($, 'map')
    await ensureLoaded($)
    return { text: 'Orbit map opened.' }
  })

  on('command.run', { command: 'orbit-band' }, async $ => {
    const wasHidden = await read($, isBandHiddenAtom)
    await update($, isBandHiddenAtom, () => !wasHidden)
    if (wasHidden) void refresh($)
    return { text: wasHidden ? 'Orbit band shown.' : 'Orbit band hidden; /orbit-band brings it back.' }
  })

  // A mentioned task's card rides along with the prompt, from the last snapshot.
  on('prompt.submit', async ($, e, next) => {
    const snap = await read($, snapshotAtom)
    if (snap === null) return next(e)
    const known = [...snap.tasks, ...snap.doneRecently]
    const cards = mentions(e.text, snap).flatMap(id => {
      const task = known.find(item => item.id === id)
      return task ? [card(task, snap.workspace)] : []
    })
    if (cards.length === 0) return next(e)
    return next({ ...e, context: [...(e.context ?? []), cards.join('\n\n')] })
  })

  on('tool.call', async ($, e, next) => {
    // A commit made while this session works a task carries the task's trailer.
    if (e.tool === 'Bash' && settings.commitTrailer) {
      const command = (e as { command?: unknown }).command
      const taskId = await read($, activeAtom)
      const rewritten = taskId !== null && typeof command === 'string' ? withTaskTrailer(command, taskId) : null
      if (rewritten !== null) return next({ ...e, command: rewritten } as typeof e)
    }

    // Starting or finishing a task through Orbit's MCP tools moves the band with it.
    if (TASK_UPDATE.test(e.tool)) {
      const input = e as { id?: unknown; status?: unknown }
      const ran = await next(e)
      if (ran.deny === undefined && ran.isError !== true && typeof input.id === 'string') {
        if (input.status === 'in-progress') await setActive($, input.id)
        if ((input.status === 'done' || input.status === 'rejected') && (await read($, activeAtom)) === input.id) await setActive($, null)
        void refresh($)
      }
      return ran
    }
    return next(e)
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => {
    if (e.props.hasSurvey || settings.band === 'off' || (await read($, isBandHiddenAtom))) return next(e)
    const snapshot = await read($, snapshotAtom)
    const error = configError ?? (await read($, errorAtom))
    if (snapshot === null && error === null) {
      await ensureLoaded($)
      return next(e)
    }
    return band(
      $.ui.resolve(e),
      {
        snapshot,
        error,
        active: await read($, activeAtom),
        ship: await read($, shipAtom),
        now: await $.clock.now(),
        columns: e.props.bodyColumns,
        isCompact: settings.band === 'compact',
      },
      actionsFor($),
    )
  })

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const table = $.ui.resolve(e)
    await ensureLoaded($)
    return pane(
      table,
      {
        snapshot: await read($, snapshotAtom),
        error: configError ?? (await read($, errorAtom)),
        view: await read($, viewAtom),
        selected: await read($, selectedAtom),
        active: await read($, activeAtom),
        ship: await read($, shipAtom),
        flash: await read($, flashAtom),
        now: await $.clock.now(),
        columns: e.props.bodyColumns,
      },
      actionsFor($),
      'Svg' in table ? table.Svg : undefined,
    )
  })
}
