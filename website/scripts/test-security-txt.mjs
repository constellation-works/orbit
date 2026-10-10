import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const validatorPath = join(scriptDirectory, 'validate-security-txt.mjs');

function securityTxt(expires) {
  return Buffer.from([
    'Contact: https://example.com/security\n',
    `Expires: ${expires}\n`,
    'Canonical: https://orbit-cli.com/.well-known/security.txt\n',
    'Policy: https://example.com/security/policy\n',
  ].join(''));
}

const fixture = securityTxt('2099-12-31T23:59:59Z');
const expiryCases = [
  { label: 'a valid future date', expires: '2099-12-31T23:59:59Z', valid: true },
  { label: 'February 29 in a future leap year', expires: '2096-02-29T00:00:00Z', valid: true },
  {
    label: 'February 29 in a future non-leap year',
    expires: '2099-02-29T00:00:00Z',
    valid: false,
    message: /existing UTC calendar date/u,
  },
  {
    label: 'February 30 in the future',
    expires: '2099-02-30T00:00:00Z',
    valid: false,
    message: /existing UTC calendar date/u,
  },
  {
    label: 'an expired timestamp',
    expires: '2000-01-01T00:00:00Z',
    valid: false,
    message: /must be in the future/u,
  },
];
const fixtureDirectory = await mkdtemp(join(tmpdir(), 'security-txt-validator-'));

function validate(filePath) {
  return spawnSync(process.execPath, [validatorPath, filePath], { encoding: 'utf8' });
}

try {
  const bomPath = join(fixtureDirectory, 'bom.txt');
  const malformedPath = join(fixtureDirectory, 'malformed.txt');

  await writeFile(bomPath, Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), fixture]));
  await writeFile(malformedPath, Buffer.concat([fixture, Buffer.from([0xff])]));

  for (const [index, testCase] of expiryCases.entries()) {
    const filePath = join(fixtureDirectory, `expires-${index}.txt`);
    await writeFile(filePath, securityTxt(testCase.expires));

    const result = validate(filePath);
    if (testCase.valid) {
      assert.equal(result.status, 0, `${testCase.label} must pass: ${result.stderr}`);
    } else {
      assert.notEqual(result.status, 0, `${testCase.label} must fail`);
      assert.match(result.stderr, testCase.message, `${testCase.label} must be reported`);
    }
  }

  const bomResult = validate(bomPath);
  assert.notEqual(bomResult.status, 0, 'a UTF-8 BOM must make validation fail');
  assert.match(bomResult.stderr, /byte-order mark/u, 'the BOM rejection must be reported');

  const malformedResult = validate(malformedPath);
  assert.notEqual(malformedResult.status, 0, 'malformed UTF-8 must make validation fail');
} finally {
  await rm(fixtureDirectory, { recursive: true, force: true });
}
