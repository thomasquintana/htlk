// Update pinned protocol data, not executable code. Runtime validation is native Rust.
import { createHash } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const commit = 'c4c367f9f58296a7053f5c78a52fd02bfbb56a49';
const upstream = `https://raw.githubusercontent.com/modelcontextprotocol/modelcontextprotocol/${commit}`;
const directory = resolve(dirname(fileURLToPath(import.meta.url)), '../crates/htlk-analyzer/assets');
async function fetchText(path) {
  const response = await fetch(`${upstream}/${path}`);
  if (!response.ok) throw new Error(`Protocol snapshot fetch failed: ${response.status}`);
  return response.text();
}
const text = await fetchText('schema/2025-11-25/schema.json');
const schema = JSON.parse(text);
for (const name of ['Tool', 'Resource', 'ResourceTemplate', 'Prompt', 'GetPromptResult']) {
  if (!(name in schema.$defs)) throw new Error(`Missing protocol definition: ${name}`);
}
writeFileSync(`${directory}/mcp-2025-11-25.schema.json`, text);
writeFileSync(`${directory}/MCP-LICENSE`, await fetchText('LICENSE'));
writeFileSync(`${directory}/mcp-source.json`, `${JSON.stringify({
  protocol: '2025-11-25', commit,
  url: `${upstream}/schema/2025-11-25/schema.json`,
  sha256: createHash('sha256').update(text).digest('hex'),
}, null, 2)}\n`);
console.log(`Pinned MCP protocol data (${Buffer.byteLength(text)} bytes; ${schema.$schema})`);
