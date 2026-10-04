// State contract of the Orbit mod (plugin/hooks/mod): the values the band,
// status line and panes draw from, held by Claude Code for the session.

/** One open or recently finished task, slimmed from `orbit task list --json`. */
export type OrbitTask = {
  id: string
  title: string
  status: string
  priority: string
  complexity: string | null
  type: string
  crew: string | null
  deps: string[]
  criteria: string[]
  runId: string | null
  updatedAt: string
}

/** The checkout's workspace as last read, and where it was read from. */
export type OrbitSnapshot = {
  workspace: string
  /** SSH host the workspace was read through; null when read locally. */
  host: string | null
  tasks: OrbitTask[]
  /** Tasks that reached `done` in the last 24 hours. */
  doneRecently: OrbitTask[]
  at: number
}

export type OrbitStepState = 'pending' | 'active' | 'done' | 'failed'

export type OrbitCheck = { label: string; isOk: boolean; detail: string }

/** A ship this session is preparing, flying or has landed. */
export type OrbitShip = {
  taskId: string
  phase: 'preflight' | 'launching' | 'flying' | 'landed' | 'failed'
  runId: string | null
  checks: OrbitCheck[]
  steps: OrbitStepState[]
  startedAt: number | null
  finishedAt: number | null
  message: string | null
}

export type OrbitView = 'board' | 'ship' | 'map'

declare module 'claude-code' {
  interface PluginState {
    orbit: {
      snapshot: OrbitSnapshot | null
      error: string | null
      isBandHidden: boolean
      active: string | null
      ship: OrbitShip | null
      view: OrbitView
      selected: string | null
      /** Board sections the person has open; null keeps the defaults. */
      openLanes: string[] | null
      flash: string | null
    }
  }
}
