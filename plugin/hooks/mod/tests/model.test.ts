// The pure rules the mod's hooks lean on.

import { expect, test } from 'claude-code/testing'

import { federatedHosts, remoteArgv } from '../cli'
import type { OrbitShip, OrbitStepState } from '../../../types'
import { parseRunEvents, PIPELINE, shipPosition, stepStates, track, withTaskTrailer } from '../model'

const shipWithSteps = (steps: OrbitStepState[]): OrbitShip => ({
  taskId: 'ORB-7', phase: 'flying', runId: 'jrun-test', checks: [], steps,
  startedAt: null, finishedAt: null, message: null,
})

test('terminal audit outcomes count only successful steps and locate failures or stops', () => {
  const cases: { outcome: string | null; state: OrbitStepState; glyph: string }[] = [
    { outcome: 'success', state: 'done', glyph: '●' },
    { outcome: 'failed', state: 'failed', glyph: '✗' },
    { outcome: 'error', state: 'failed', glyph: '✗' },
    { outcome: 'held', state: 'stopped', glyph: '■' },
    { outcome: 'cancelled', state: 'stopped', glyph: '■' },
    { outcome: 'skipped', state: 'skipped', glyph: '↷' },
    { outcome: 'unknown', state: 'stopped', glyph: '■' },
    { outcome: null, state: 'stopped', glyph: '■' },
  ]
  for (const { outcome, state, glyph } of cases) {
    for (const isFailed of [false, true]) {
      const events = parseRunEvents(JSON.stringify({
        run_id: 'jrun-test', job_id: 'task_pr_pipeline', events: [
          { event_type: 'step.started', step_id: 'worktree' },
          { event_type: 'step.finished', step_id: 'worktree', outcome: 'success', error_message: null },
          { event_type: 'step.started', step_id: 'implement_bundle' },
          { event_type: 'step.finished', step_id: 'implement_bundle', outcome, error_message: null },
        ],
      }))
      const steps = stepStates(events, isFailed)
      expect(steps).toEqual(['done', state, ...PIPELINE.slice(2).map(() => 'pending')])
      const position = shipPosition(shipWithSteps(steps))
      expect(position.done).toBe(state === 'done' ? 2 : 1)
      expect(position.label).toBe(state === 'failed' || state === 'stopped' ? PIPELINE[1]?.label : 'flying')
      expect(track(steps.slice(0, 3))).toBe(`●━${glyph}${state === 'done' ? '━' : '─'}○`)
    }
  }
})

test('the latest audit transition replaces earlier attempts and explicit skips stay incomplete', () => {
  const cases: { rows: object[]; state: OrbitStepState; isFailed: boolean }[] = [
    { rows: [{ event_type: 'step.skipped', reason: 'when: false' }], state: 'skipped', isFailed: false },
    { rows: [{ event_type: 'step.started' }], state: 'active', isFailed: false },
    { rows: [{ event_type: 'step.started' }], state: 'failed', isFailed: true },
    { rows: [{ event_type: 'step.finished', outcome: 'error' }, { event_type: 'step.started' }], state: 'active', isFailed: false },
    { rows: [{ event_type: 'step.finished', outcome: 'error' }, { event_type: 'step.started' }, { event_type: 'step.finished', outcome: 'success' }], state: 'done', isFailed: false },
    { rows: [{ event_type: 'step.finished', outcome: 'success' }, { event_type: 'step.started' }, { event_type: 'step.finished', outcome: 'failed' }], state: 'failed', isFailed: true },
  ]
  for (const { rows, state, isFailed } of cases) {
    const events = parseRunEvents(JSON.stringify({ events: [
      ...rows.map(row => ({ ...row, step_id: 'worktree' })),
      { event_type: 'activity.finished', step_id: 'worktree', outcome: 'success' },
      { event_type: 'step.finished', step_id: 'unrelated', outcome: 'success' },
    ] }))
    const steps = stepStates(events, isFailed)
    expect(steps).toEqual([state, ...PIPELINE.slice(1).map(() => 'pending')])
    expect(shipPosition(shipWithSteps(steps)).done).toBe(state === 'done' ? 1 : 0)
  }
})

test('a commit gains the task trailer once, and only a commit', () => {
  const rewritten = withTaskTrailer(`git commit -m "fix"`, 'ORB-7')
  expect(rewritten).not.toBeNull()
  expect(withTaskTrailer(rewritten ?? '', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git -C repo commit -m x', 'ORB-7')).not.toBeNull()
  expect(withTaskTrailer('git commit --amend --no-edit', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git status', 'ORB-7')).toBeNull()
})

test('only a git commit at a command position gains the trailer', () => {
  expect(withTaskTrailer('git commit-tree abc -m x', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git commit-graph write --reachable', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git log --grep="git commit"', 'ORB-7')).toBeNull()
  expect(withTaskTrailer(`echo 'git commit' && git status`, 'ORB-7')).toBeNull()
  expect(withTaskTrailer('echo git commit', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('git add . && git commit -m "a git commit"', 'ORB-7')).toBe(
    `git add . && git commit --trailer 'Task: ORB-7' -m "a git commit"`,
  )
  expect(withTaskTrailer('git status; git -c user.name=x commit -m x', 'ORB-7')).toBe(
    `git status; git -c user.name=x commit --trailer 'Task: ORB-7' -m x`,
  )
  expect(withTaskTrailer('git add .\ngit commit -m x', 'ORB-7')).toBe(
    `git add .\ngit commit --trailer 'Task: ORB-7' -m x`,
  )
  expect(withTaskTrailer('GIT_EDITOR=true git commit -m x', 'ORB-7')).toBe(
    `GIT_EDITOR=true git commit --trailer 'Task: ORB-7' -m x`,
  )
  expect(withTaskTrailer('echo "it\\" quote" && (git commit)', 'ORB-7')).toBe(
    `echo "it\\" quote" && (git commit --trailer 'Task: ORB-7')`,
  )
})

test('line continuations preserve whether a command can start', () => {
  const command = 'git add . && \\\n  git commit -m x'
  expect(withTaskTrailer(command, 'ORB-7')).toBe(
    'git add . && \\\n  git commit --trailer \'Task: ORB-7\' -m x',
  )
  expect(withTaskTrailer('echo \\\n  git commit -m x', 'ORB-7')).toBeNull()
})

test('shell conditions and negation allow a commit command', () => {
  for (const command of [
    'if git commit -m x; then echo ok; fi',
    'if false; then :; elif git commit -m x; then echo ok; fi',
    'while git commit -m x; do break; done',
    'until git commit -m x; do break; done',
    '! git commit -m x',
    'if ! git commit -m x; then echo failed; fi',
  ]) {
    expect(withTaskTrailer(command, 'ORB-7')).toBe(
      command.replace('git commit', "git commit --trailer 'Task: ORB-7'"),
    )
  }
  expect(withTaskTrailer('echo ! git commit -m x', 'ORB-7')).toBeNull()
  expect(withTaskTrailer('echo if git commit -m x', 'ORB-7')).toBeNull()
})

test('heredoc bodies are skipped before finding the next commit', () => {
  for (const prelude of [
    "cat <<EOF\nit's\nEOF\n",
    "cat <<'EOF'\nit's\nEOF\n",
    'cat <<"EOF"\nit\'s\nEOF\n',
    "cat <<E\\OF\nit's\nEOF\n",
    "cat <<E'OF'\nit's\nEOF\n",
    "cat <<\"EO\\F\"\nit's\nEO\\F\n",
    "cat <<''\nit's\n\n",
    "cat <<-EOF\n\tit's\n\tEOF\n",
    "cat <<FIRST <<'SECOND'\nit's\nFIRST\n\"\nSECOND\n",
    "cat <<EOF && \\\n  echo ready\nit's\nEOF\n",
    "cat <<EOF # a comment with <<IGNORED\nit's\nEOF\n",
    'cat <<<"it\'s"\n',
    "printf '%s' '<<EOF'\n",
    'echo $((1 << 2))\n',
    '((value = (1 << 2)))\n',
  ]) {
    expect(withTaskTrailer(`${prelude}git commit -m x`, 'ORB-7')).toBe(
      `${prelude}git commit --trailer 'Task: ORB-7' -m x`,
    )
  }
  const prelude = 'cat <<EOF\ngit commit -m example\nEOF\n'
  expect(withTaskTrailer(`${prelude}git commit -m x`, 'ORB-7')).toBe(
    `${prelude}git commit --trailer 'Task: ORB-7' -m x`,
  )
  expect(withTaskTrailer(prelude, 'ORB-7')).toBeNull()
  expect(withTaskTrailer('cat <<EOF\ngit commit -m example', 'ORB-7')).toBeNull()
})

test('remote arguments reach the owner as one quoted word each', () => {
  const argv = remoteArgv('box', ['task', 'list', "it's; rm -rf ~"])
  expect(argv.slice(0, 7)).toEqual(['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '--', 'box'])
  expect(argv.at(-1)?.endsWith(`'it'\\''s; rm -rf ~'`)).toBe(true)
})

test('federated hosts come from each ssh line of either host file, refusing option-shaped hosts', () => {
  const toml = '[[destinations]]\nssh = "dk-server-2"\nmachine_id = "hm_1"\n\n[[destinations]]\nssh = "-oProxyCommand=x"\n\n[[destinations]]\n  ssh = "daniel@box.ts.net" # tailnet\n'
  expect(federatedHosts(toml)).toEqual(['dk-server-2', 'daniel@box.ts.net'])
  expect(federatedHosts('')).toEqual([])
  const hosts = 'schema_version = 1\n\n[[hosts]]\nname = "box"\nmachine_id = "hm_1"\nssh = "dk-server-2"\ntask_prefix = "ORB"\n'
  expect(federatedHosts(hosts)).toEqual(['dk-server-2'])
})
