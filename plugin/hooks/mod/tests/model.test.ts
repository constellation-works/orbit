// The pure rules the mod's hooks lean on.

import { expect, test } from 'claude-code/testing'

import { federatedHosts, remoteArgv } from '../cli'
import { withTaskTrailer } from '../model'

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
  expect(withTaskTrailer('echo "it\\" quote" && (git commit)', 'ORB-7')).toBe(
    `echo "it\\" quote" && (git commit --trailer 'Task: ORB-7')`,
  )
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
