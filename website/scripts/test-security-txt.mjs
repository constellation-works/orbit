import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const validatorPath = join(scriptDirectory, 'validate-security-txt.mjs');
const fixture = Buffer.from([
  'Contact: https://example.com/security\n',
  'Expires: 2099-12-31T23:59:59Z\n',
  'Canonical: https://orbit-cli.com/.well-known/security.txt\n',
  'Policy: https://example.com/security/policy\n',
].join(''));
const fixtureDirectory = await mkdtemp(join(tmpdir(), 'security-txt-validator-'));

function validate(filePath) {
  return spawnSync(process.execPath, [validatorPath, filePath], { encoding: 'utf8' });
}

try {
  const validPath = join(fixtureDirectory, 'valid.txt');
  const bomPath = join(fixtureDirectory, 'bom.txt');
  const malformedPath = join(fixtureDirectory, 'malformed.txt');

  await writeFile(validPath, fixture);
  await writeFile(bomPath, Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), fixture]));
  await writeFile(malformedPath, Buffer.concat([fixture, Buffer.from([0xff])]));

  const validResult = validate(validPath);
  assert.equal(validResult.status, 0, validResult.stderr);

  const bomResult = validate(bomPath);
  assert.notEqual(bomResult.status, 0, 'a UTF-8 BOM must make validation fail');
  assert.match(bomResult.stderr, /byte-order mark/u, 'the BOM rejection must be reported');

  const malformedResult = validate(malformedPath);
  assert.notEqual(malformedResult.status, 0, 'malformed UTF-8 must make validation fail');
} finally {
  await rm(fixtureDirectory, { recursive: true, force: true });
}
