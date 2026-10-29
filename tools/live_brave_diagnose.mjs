// Independent upstream Zod assessment of saved actual Place Search evidence.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const [response, schemaModule] = process.argv.slice(2);
const { PlaceSearchApiResponseSchema } = await import(pathToFileURL(resolve(schemaModule)));
const raw = JSON.parse(readFileSync(response, 'utf8'));
const parsed = PlaceSearchApiResponseSchema.safeParse(raw.structuredContent);
console.log(JSON.stringify({
  successful_protocol: raw.isError !== true,
  structured_keys: Object.keys(raw.structuredContent ?? {}),
  upstream_zod_valid: parsed.success,
  parsed_structured_keys: parsed.success ? Object.keys(parsed.data) : [],
  stripped_root_keys: parsed.success ? Object.keys(raw.structuredContent).filter(key => !(key in parsed.data)) : [],
  issues: parsed.success ? [] : parsed.error.issues.map(issue => ({
    path: issue.path, code: issue.code, expected: issue.expected,
  })),
}, null, 2));
