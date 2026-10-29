import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';

test('discovery captures schema-less descriptors without tool calls and rejects repeated cursors', () => {
  const directory = mkdtempSync(join(tmpdir(), 'htlk-discovery-'));
  try {
    for (const mode of ['interop', 'cursor']) {
      const log = join(directory, mode + '.jsonl');
      const config = join(directory, mode + '.host.json');
      const catalog = join(directory, mode + '.catalog.json');
      writeFileSync(log, '');
      writeFileSync(config, JSON.stringify({ transport: 'stdio', alias: 'fixture', deployment_id: 'test',
        program: process.execPath, arguments: [resolve('tests/support/mcp_peer.mjs'), 'stdio', mode, log], environment: {} }));
      const result = spawnSync(process.execPath, ['tools/discover-mcp.mjs', config, catalog], { encoding: 'utf8', timeout: 15000 });
      assert.equal(result.error, undefined);
      const requests = readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
      assert(!requests.some(request => request.method === 'tools/call'));
      if (mode === 'cursor') {
        assert.notEqual(result.status, 0);
        assert.match(result.stderr, /Repeated discovery cursor/);
      } else {
        assert.equal(result.status, 0, result.stderr);
        const captured = JSON.parse(readFileSync(catalog, 'utf8'));
        assert.deepEqual(captured.tools.tools[0].descriptor, { name: 'echo', inputSchema: { type: 'object',
          properties: { value: { type: ['string', 'null'] } }, required: ['value'], additionalProperties: false } });
        assert.equal(JSON.parse(readFileSync(catalog + '.discovery.json', 'utf8')).tools_call_count, 0);
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
