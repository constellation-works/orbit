// How the Orbit mod reaches Orbit: the `orbit` CLI, run in the session's
// checkout when this machine owns the workspace, or over SSH on the owner
// host the `ownerHost` option names when the checkout is a replica. Pure
// helpers only: register.tsx runs the commands they build.

import { firstLine } from './model'

/** Where the workspace's tasks are read: its registered name, the checkout, and the SSH host (null: this machine). */
export type Target = { workspace: string; cwd: string; host: string | null }

const HOST = /^[A-Za-z0-9][A-Za-z0-9._@-]*$/
const NAME = /^[A-Za-z0-9][A-Za-z0-9._-]*$/

export class OrbitError extends Error {}

/** A host the person configured, or null; refuses anything SSH could read as an option. */
export function ownerHost(value: unknown): string | null {
  if (typeof value !== 'string' || value.trim() === '') return null
  const host = value.trim()
  if (!HOST.test(host)) throw new OrbitError(`ownerHost "${host}" is not a host name or SSH alias`)
  return host
}

/** `orbit` from PATH, or from ~/.orbit/bin where the installer puts it. */
export const localArgv = (args: readonly string[]): string[] => ['/bin/sh', '-c', 'PATH="$HOME/.orbit/bin:$PATH" exec orbit "$@"', 'orbit', ...args]

const quote = (arg: string): string => `'${arg.replace(/'/g, `'\\''`)}'`

export const remoteArgv = (host: string, args: readonly string[]): string[] => [
  'ssh',
  '-o',
  'BatchMode=yes',
  '-o',
  'ConnectTimeout=5',
  '--',
  host,
  `PATH="$HOME/.orbit/bin:$PATH" orbit ${args.map(quote).join(' ')}`,
]

/** The argv that runs `orbit <args> --workspace <name>` for the target. */
export const argvFor = (target: Target, args: readonly string[]): string[] => {
  const full = [...args, '--workspace', target.workspace]
  return target.host === null ? localArgv(full) : remoteArgv(target.host, full)
}

/** The workspace `orbit workspace show --format json` reports, or null when the checkout is not registered. */
export function readShown(stdout: string): { name: string; role: string } | null {
  const parsed: unknown = JSON.parse(stdout)
  if (typeof parsed !== 'object' || parsed === null) return null
  const record = parsed as { workspace?: { name?: unknown }; checkout?: { role?: unknown }; registered?: unknown }
  const name = record.workspace?.name
  if (record.registered === false || typeof name !== 'string') return null
  return { name, role: typeof record.checkout?.role === 'string' ? record.checkout.role : 'owner' }
}

/** Decides the target from what `workspace show` said about the checkout. */
export function targetFrom(shown: { name: string; role: string } | null, cwd: string, host: string | null, root: string): Target {
  if (shown !== null && shown.role !== 'replica') return { workspace: shown.name, cwd, host: null }
  if (shown !== null) {
    if (host === null) throw new OrbitError(`${shown.name} is a replica here; set the plugin's ownerHost option to read its owner`)
    return { workspace: shown.name, cwd, host }
  }
  if (host === null) throw new OrbitError(`no Orbit workspace at ${root}; register it, or set the plugin's ownerHost option to read its owner`)
  const name = root.split('/').filter(Boolean).pop() ?? ''
  if (!NAME.test(name)) throw new OrbitError(`no Orbit workspace for ${root}`)
  return { workspace: name, cwd, host }
}

/** The failure of a finished `orbit` run, or null when it succeeded. */
export function failure(target: Target, args: readonly string[], exitCode: number, stdout: string, stderr: string): string | null {
  if (exitCode === 0) return null
  if (target.host !== null && exitCode === 255) return `${target.host} unreachable`
  return firstLine(stderr || stdout) || `orbit ${args[0] ?? ''} exited ${exitCode}`
}

export const reason = (thrown: unknown): string => (thrown instanceof Error ? firstLine(thrown.message) : String(thrown))
