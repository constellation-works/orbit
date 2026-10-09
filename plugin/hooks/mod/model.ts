// Pure logic of the Orbit mod: parsing `orbit` CLI output and deriving what
// the band, panes and hooks show. Nothing here touches `$`.

import type { OrbitShip, OrbitSnapshot, OrbitStepState, OrbitTask } from '../../types'

export const OPEN_STATUSES = ['proposed', 'backlog', 'in-progress', 'review', 'blocked'] as const

export type OpenStatus = (typeof OPEN_STATUSES)[number]

export type Lane = { status: OpenStatus; name: string; glyph: string; color: string }

/** Board order, left to right. Glyphs carry status as well as color. */
export const LANES: readonly Lane[] = [
  { status: 'proposed', name: 'Proposed', glyph: '○', color: 'gray' },
  { status: 'backlog', name: 'Backlog', glyph: '·', color: 'white' },
  { status: 'in-progress', name: 'In progress', glyph: '▶', color: 'cyan' },
  { status: 'review', name: 'Review', glyph: '◎', color: 'yellow' },
  { status: 'blocked', name: 'Blocked', glyph: '■', color: 'red' },
]

export const ACCENT = '#F5A524'
export const DONE_COLOR = 'green'

export const laneOf = (status: string): Lane | undefined => LANES.find(lane => lane.status === status)

/** The top-level steps of `task_pr_pipeline`, in the order a ship runs them. */
export const PIPELINE: readonly { id: string; label: string }[] = [
  { id: 'worktree', label: 'worktree' },
  { id: 'implement_bundle', label: 'implement' },
  { id: 'commit', label: 'commit' },
  { id: 'prepare_branch', label: 'branch' },
  { id: 'sync_base', label: 'sync' },
  { id: 'review_gate_admit', label: 'gate in' },
  { id: 'review', label: 'review' },
  { id: 'review_gate_settle', label: 'gate out' },
  { id: 'push', label: 'push' },
  { id: 'pr_open', label: 'PR' },
  { id: 'promote_tasks', label: 'promote' },
  { id: 'complete_pr', label: 'complete' },
]

const TASK_ID = /\b[A-Z][A-Z0-9]{1,11}-\d{1,9}\b/g
const RUN_ID = /\bjrun-[A-Za-z0-9-]+\b/

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

const text = (value: unknown): string => (typeof value === 'string' ? value : '')

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string') : []

/** Reads `orbit task list --json` output into slim tasks; tolerates either a bare array or `{ tasks }`. */
export function parseTasks(stdout: string): OrbitTask[] {
  const parsed: unknown = JSON.parse(stdout)
  const rows = Array.isArray(parsed) ? parsed : isRecord(parsed) && Array.isArray(parsed.tasks) ? parsed.tasks : []

  return rows.filter(isRecord).flatMap(row => {
    const id = text(row.id)
    if (id === '') return []
    const deps = strings(row.resolved_dependencies).length > 0 ? strings(row.resolved_dependencies) : strings(row.dependencies)

    return [
      {
        id,
        title: text(row.title),
        status: text(row.status),
        priority: text(row.priority) || 'medium',
        complexity: text(row.complexity) || null,
        type: text(row.type) || 'feature',
        crew: text(row.resolved_crew) || text(row.crew) || null,
        deps,
        criteria: strings(row.acceptance_criteria).slice(0, 12),
        runId: text(row.job_run_id) || null,
        updatedAt: text(row.updated_at),
      },
    ]
  })
}

/** The tasks of a done listing that reached done within `windowMs` of `now`. */
export function recentlyDone(done: OrbitTask[], now: number, windowMs = 24 * 3600_000): OrbitTask[] {
  return done.filter(task => {
    const at = Date.parse(task.updatedAt)
    return Number.isFinite(at) && now - at <= windowMs
  })
}

export type Counts = Record<OpenStatus, number>

export function tally(tasks: readonly OrbitTask[]): Counts {
  const counts: Counts = { proposed: 0, backlog: 0, 'in-progress': 0, review: 0, blocked: 0 }
  for (const task of tasks) {
    if (task.status in counts) counts[task.status as OpenStatus] += 1
  }
  return counts
}

const PRIORITY_RANK: Record<string, number> = { critical: 0, high: 1, medium: 2, low: 3 }

/** Highest priority first, then most recently updated. */
export function byUrgency(a: OrbitTask, b: OrbitTask): number {
  const rank = (PRIORITY_RANK[a.priority] ?? 2) - (PRIORITY_RANK[b.priority] ?? 2)
  return rank !== 0 ? rank : b.updatedAt.localeCompare(a.updatedAt)
}

export const lane = (tasks: readonly OrbitTask[], status: OpenStatus): OrbitTask[] =>
  tasks.filter(task => task.status === status).sort(byUrgency)

export type Change = { id: string; title: string; to: 'blocked' | 'review' | 'done' }

/** What moved between two snapshots of the same workspace that is worth a toast. */
export function changes(before: OrbitSnapshot | null, after: OrbitSnapshot): Change[] {
  if (before === null || before.workspace !== after.workspace || before.host !== after.host) return []
  const was = new Map(before.tasks.map(task => [task.id, task.status]))
  const open = new Set(after.tasks.map(task => task.id))
  const found: Change[] = []

  for (const task of after.tasks) {
    const prior = was.get(task.id)
    if (prior === undefined || prior === task.status) continue
    if (task.status === 'blocked' || task.status === 'review') found.push({ id: task.id, title: task.title, to: task.status })
  }
  const announced = new Set(before.doneRecently.map(task => task.id))
  for (const task of after.doneRecently) {
    if (was.has(task.id) && !open.has(task.id) && !announced.has(task.id)) found.push({ id: task.id, title: task.title, to: 'done' })
  }
  return found
}

export function toastFor(change: Change): string {
  if (change.to === 'blocked') return `■ ${change.id} blocked · ${clip(change.title, 60)}`
  if (change.to === 'review') return `◎ ${change.id} ready for review · ${clip(change.title, 60)}`
  return `✓ ${change.id} done · ${clip(change.title, 60)}`
}

/** Task ids a prompt mentions that the snapshot knows, first three, in order. */
export function mentions(prompt: string, snapshot: OrbitSnapshot): string[] {
  const known = new Set([...snapshot.tasks, ...snapshot.doneRecently].map(task => task.id))
  const found: string[] = []
  for (const match of prompt.matchAll(TASK_ID)) {
    const id = match[0]
    if (known.has(id) && !found.includes(id)) found.push(id)
    if (found.length === 3) break
  }
  return found
}

/** The context block a mentioned task attaches to the prompt. */
export function card(task: OrbitTask, workspace: string): string {
  const lines = [
    `Orbit task ${task.id} (workspace ${workspace}): ${task.title}`,
    `Status ${task.status} · priority ${task.priority} · ${task.type}${task.complexity ? ` · ${task.complexity}` : ''}${task.crew ? ` · crew ${task.crew}` : ''}`,
  ]
  if (task.deps.length > 0) lines.push(`Depends on: ${task.deps.join(', ')}`)
  if (task.runId) lines.push(`Last run: ${task.runId}`)
  if (task.criteria.length > 0) lines.push('Acceptance criteria:', ...task.criteria.map(item => `- ${item}`))
  lines.push('Read the full record with the Orbit task show tool before changing the task.')
  return lines.join('\n')
}

// Sticky: tried only where a command can start. The subcommand must end at
// whitespace or a shell separator, so `commit-tree` and `commit-graph` miss.
const COMMIT = /git(?:\s+-[cC]\s+\S+)*\s+commit(?=[\s;&|()<>]|$)/y
const COMMAND_START = new Set([';', '&', '|', '(', ')', '{', '\n'])

/** End index of the first `git commit` at a command position outside quotes. */
function commitEnd(command: string): number | null {
  let quote: string | null = null
  let atStart = true
  for (let i = 0; i < command.length; i++) {
    const ch = command[i]
    if (quote !== null) {
      if (ch === '\\' && quote === '"') i++
      else if (ch === quote) quote = null
      continue
    }
    if (/\s/.test(ch)) continue
    if (atStart) {
      COMMIT.lastIndex = i
      if (COMMIT.test(command)) return COMMIT.lastIndex
    }
    atStart = COMMAND_START.has(ch)
    if (ch === '\\') i++
    else if (ch === "'" || ch === '"') quote = ch
  }
  return null
}

/**
 * The command with a `Task:` trailer on its first `git commit`, or null when
 * the command makes no commit there (quoted text and other `git commit-*`
 * subcommands do not count) or already names a trailer or a task line.
 */
export function withTaskTrailer(command: string, taskId: string): string | null {
  const end = commitEnd(command)
  if (end === null || /--trailer\b|\bTask:/.test(command)) return null
  if (/\s--amend\b/.test(command)) return null
  return `${command.slice(0, end)} --trailer 'Task: ${taskId}'${command.slice(end)}`
}

export const findRunId = (output: string): string | null => RUN_ID.exec(output)?.[0] ?? null

export type ShipRun = {
  job: string
  state: string
  error: string | null
  startedAt: number | null
  taskIds: string[]
  children: string[]
}

/** Reads the run and durable gate/delivery dispatches from `orbit run show --json`. */
export function parseShipRun(stdout: string): ShipRun {
  const parsed: unknown = JSON.parse(stdout)
  const run = isRecord(parsed) && isRecord(parsed.run) ? parsed.run : {}
  const pipeline = isRecord(parsed) && isRecord(parsed.pipeline_state) ? parsed.pipeline_state : {}
  const dispatches = Array.isArray(pipeline.child_dispatches) ? pipeline.child_dispatches.filter(isRecord) : []
  const startedAt = Date.parse(text(run.started_at))
  return {
    job: text(run.job_id),
    state: text(run.state) || 'running',
    error: text(run.error_message) || null,
    startedAt: Number.isFinite(startedAt) ? startedAt : null,
    taskIds: strings(run.task_ids),
    children: dispatches
      .filter(child => child.job_name === 'task_gate_pipeline' || child.job_name === 'task_pr_pipeline')
      .map(child => text(child.child_run_id))
      .filter(id => id !== ''),
  }
}

export type RunEvent = { type: string; step: string | null }

/** Reads `orbit run events --json` into the step transitions the ship track needs. */
export function parseRunEvents(stdout: string): RunEvent[] {
  const parsed: unknown = JSON.parse(stdout)
  const rows = Array.isArray(parsed) ? parsed : isRecord(parsed) && Array.isArray(parsed.events) ? parsed.events : []
  return rows.filter(isRecord).map(row => ({
    type: text(row.event_type) || text(row.type),
    step: text(row.step_id) || null,
  }))
}

/** Each pipeline step's state from the run's events; the running step is `active`, or `failed` once the run failed. */
export function stepStates(events: readonly RunEvent[], isFailed: boolean): OrbitStepState[] {
  const started = new Set<string>()
  const finished = new Set<string>()
  for (const event of events) {
    if (event.step === null) continue
    if (event.type === 'step.started') started.add(event.step)
    if (event.type === 'step.finished') finished.add(event.step)
  }
  return PIPELINE.map(({ id }) => {
    if (finished.has(id)) return 'done'
    if (started.has(id)) return isFailed ? 'failed' : 'active'
    return 'pending'
  })
}

/** The step a ship is on (or stopped at), and how many it has finished. */
export function shipPosition(ship: OrbitShip): { label: string; done: number } {
  const done = ship.steps.filter(state => state === 'done').length
  const at = ship.steps.findIndex(state => state === 'active' || state === 'failed')
  const label = at >= 0 ? (PIPELINE[at]?.label ?? '') : ship.phase === 'landed' ? 'landed' : ship.phase
  return { label, done }
}

export const STEP_GLYPH: Record<OrbitStepState, string> = { done: '●', active: '◉', failed: '✗', pending: '○' }

/** One-line track: `●━●━◉─○─…`. */
export const track = (steps: readonly OrbitStepState[]): string =>
  steps.map((state, index) => `${index === 0 ? '' : steps[index - 1] === 'done' ? '━' : '─'}${STEP_GLYPH[state]}`).join('')

export function bar(done: number, total: number, width: number): { filled: string; empty: string } {
  const cells = total <= 0 ? 0 : Math.round((done / total) * width)
  return { filled: '━'.repeat(cells), empty: '─'.repeat(Math.max(0, width - cells)) }
}

export function clock(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000))
  const mm = String(Math.floor(seconds / 60)).padStart(2, '0')
  const ss = String(seconds % 60).padStart(2, '0')
  return `T+${mm}:${ss}`
}

export const ago = (ms: number): string =>
  ms < 60_000
    ? 'just now'
    : ms < 3600_000
      ? `${Math.floor(ms / 60_000)}m ago`
      : ms < 48 * 3600_000
        ? `${Math.floor(ms / 3600_000)}h ago`
        : `${Math.floor(ms / (24 * 3600_000))}d ago`

export function clip(value: string, width: number): string {
  if (width <= 1) return value.slice(0, Math.max(0, width))
  return value.length <= width ? value : `${value.slice(0, width - 1)}…`
}

/** The first line of a failure, trimmed for a band or a toast. */
export function firstLine(value: string, width = 120): string {
  const line = value.split('\n').find(part => part.trim() !== '') ?? ''
  return clip(line.trim().replace(/^error:\s*/i, ''), width)
}

/** Status line text: the ship in flight, else the task this session is on, else blocked work. */
export function statusLine(snapshot: OrbitSnapshot | null, active: string | null, ship: OrbitShip | null): string | undefined {
  if (ship !== null && (ship.phase === 'launching' || ship.phase === 'flying')) {
    const { label, done } = shipPosition(ship)
    return `orbit ⇡ ${ship.taskId} · ${label} ${done}/${PIPELINE.length}`
  }
  if (active !== null) {
    const task = snapshot?.tasks.find(item => item.id === active)
    return `orbit ◉ ${active}${task ? ` · ${task.status}` : ''}`
  }
  const blocked = snapshot ? tally(snapshot.tasks).blocked : 0
  return blocked > 0 ? `orbit ■ ${blocked} blocked` : undefined
}

/** Preflight checks for shipping a task, from the snapshot and `orbit run readiness`. */
export function preflight(task: OrbitTask | undefined, snapshot: OrbitSnapshot, readiness: unknown): { checks: { label: string; isOk: boolean; detail: string }[] } {
  if (task === undefined) return { checks: [{ label: 'Task is open', isOk: false, detail: 'not in this workspace’s open tasks' }] }
  const open = new Map(snapshot.tasks.map(item => [item.id, item.status]))
  const unmet = task.deps.filter(dep => open.has(dep))
  const explained = isRecord(readiness) && Array.isArray(readiness.tasks) ? readiness.tasks.filter(isRecord).find(row => row.task_id === task.id) : undefined
  const conflicts = explained && Array.isArray(explained.conflicts) ? explained.conflicts.filter(isRecord) : []
  const holders = [...new Set(conflicts.map(row => text(row.locking_task_id)).filter(id => id !== ''))]

  const checks = [
    { label: 'Task is in backlog', isOk: task.status === 'backlog', detail: `${task.id} is ${task.status}` },
    { label: 'Dependencies are done', isOk: unmet.length === 0, detail: unmet.length === 0 ? (task.deps.length === 0 ? 'none' : task.deps.join(', ')) : `waiting on ${unmet.join(', ')}` },
    { label: 'No ship already running', isOk: task.runId === null || task.status !== 'in-progress', detail: task.runId ?? 'no run' },
  ]
  if (explained !== undefined) {
    const reason = text(explained.reason)
    checks.push({
      label: 'Files are free to lock',
      isOk: holders.length === 0,
      detail: holders.length === 0 ? 'no conflicting task' : `held by ${holders.join(', ')}`,
    })
    checks.push({ label: 'Ready to start', isOk: explained.eligible === true, detail: explained.eligible === true ? 'eligible now' : reason.replace(/_/g, ' ') || 'not eligible' })
  } else {
    checks.push({ label: 'Readiness', isOk: true, detail: 'not reported; the run waits for locks if needed' })
  }
  return { checks }
}
