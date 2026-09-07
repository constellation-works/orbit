import { readFile } from 'node:fs/promises';

const filePath = process.argv[2] ?? 'public/.well-known/security.txt';
const errors = [];

let content;

try {
  const bytes = await readFile(filePath);
  content = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
} catch (error) {
  errors.push(`${filePath} must be a readable UTF-8 file: ${error.message}`);
}

if (content !== undefined) {
  if (/^\uFEFF/u.test(content)) {
    errors.push('the file must not begin with a UTF-8 byte-order mark');
  }

  if (/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F]/u.test(content)) {
    errors.push('the file contains a disallowed control character');
  }

  if (/<\s*(?:!doctype|html|head|body)\b/i.test(content)) {
    errors.push('the file must contain plain text, not an HTML document');
  }

  const fields = new Map();
  for (const [index, rawLine] of content.split('\n').entries()) {
    const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;

    if (line === '' || line.startsWith('#')) {
      continue;
    }

    const match = /^(?<name>[A-Za-z][A-Za-z0-9-]*):[ \t]+(?<value>\S(?:.*\S)?)$/u.exec(line);
    if (!match) {
      errors.push(`line ${index + 1} is not an RFC 9116 field`);
      continue;
    }

    const name = match.groups.name;
    const value = match.groups.value;
    const values = fields.get(name.toLowerCase()) ?? [];
    values.push(value);
    fields.set(name.toLowerCase(), values);
  }

  const valuesFor = name => fields.get(name) ?? [];
  const contacts = valuesFor('contact');
  const expires = valuesFor('expires');
  const canonicals = valuesFor('canonical');
  const policies = valuesFor('policy');

  if (contacts.length === 0) {
    errors.push('Contact is required');
  }

  for (const contact of contacts) {
    try {
      const url = new URL(contact);
      if (url.protocol !== 'https:') {
        errors.push(`Contact must use HTTPS: ${contact}`);
      }
    } catch {
      errors.push(`Contact must be an absolute URI: ${contact}`);
    }
  }

  if (expires.length !== 1) {
    errors.push('exactly one Expires field is required');
  } else if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/u.test(expires[0])) {
    errors.push('Expires must be an RFC 3339 UTC timestamp');
  } else {
    const expiresAt = Date.parse(expires[0]);
    if (!Number.isFinite(expiresAt) || expiresAt <= Date.now()) {
      errors.push(`Expires must be in the future: ${expires[0]}`);
    }
  }

  if (canonicals.length === 0) {
    errors.push('Canonical is required');
  }

  for (const canonical of canonicals) {
    try {
      const url = new URL(canonical);
      if (url.href !== 'https://orbit-cli.com/.well-known/security.txt') {
        errors.push(`Canonical must identify the public security.txt URL: ${canonical}`);
      }
    } catch {
      errors.push(`Canonical must be an absolute URI: ${canonical}`);
    }
  }

  if (policies.length === 0) {
    errors.push('Policy is required');
  }

  for (const policy of policies) {
    try {
      const url = new URL(policy);
      if (url.protocol !== 'https:') {
        errors.push(`Policy must use HTTPS: ${policy}`);
      }
    } catch {
      errors.push(`Policy must be an absolute URI: ${policy}`);
    }
  }
}

if (errors.length > 0) {
  console.error(`Invalid security.txt at ${filePath}:`);
  for (const error of errors) {
    console.error(`- ${error}`);
  }
  process.exit(1);
}

console.log(`Valid security.txt: ${filePath}`);
