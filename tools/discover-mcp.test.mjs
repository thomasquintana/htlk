import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';

test('discovery captures all four inventories without content calls and rejects repeated cursors', () => {
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
      assert(!requests.some(request => ['tools/call', 'resources/read', 'prompts/get'].includes(request.method)));
      if (mode === 'cursor') {
        assert.notEqual(result.status, 0);
        assert.match(result.stderr, /Repeated discovery cursor/);
      } else {
        assert.equal(result.status, 0, result.stderr);
        const captured = JSON.parse(readFileSync(catalog, 'utf8'));
        assert.deepEqual(captured.tools.tools[0].descriptor, { name: 'echo', inputSchema: { type: 'object',
          properties: { value: { type: ['string', 'null'] } }, required: ['value'], additionalProperties: false } });
        assert.deepEqual(captured.resources.resources[0], { server: 'fixture', descriptor: { name: 'fixed', uri: 'context://fixed' } });
        assert.deepEqual(captured.resources.templates[0], { server: 'fixture', descriptor: { name: 'topic', uriTemplate: 'context://topics/{topic}' } });
        assert.deepEqual(captured.prompts.prompts[0], { server: 'fixture', descriptor: { name: 'review', arguments: [{ name: 'subject', required: true }] } });
        const report = JSON.parse(readFileSync(catalog + '.discovery.json', 'utf8'));
        assert.equal(report.tools_call_count, 0);
        assert.equal(report.content_read_count, 0);
        for (const key of ['tools', 'prompts', 'resources', 'templates']) {
          assert.deepEqual(report.inventories[key], { status: 'complete', pages: 2, items: 1 });
        }
      }
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

// Independent stdio peer records exact requests, including absent vs empty cursor.
const peer = `
import {createInterface} from 'node:readline';
import {appendFileSync} from 'node:fs';
const [mode,log]=process.argv.slice(1);
const send=value=>process.stdout.write(JSON.stringify(value)+'\\n');
const descriptor={tools:{name:'tool',inputSchema:{type:'object'}},prompts:{name:'prompt'},resources:{name:'resource',uri:'test://resource'},resourceTemplates:{name:'template',uriTemplate:'test://resource/{arg}'}};
const all={tools:{},prompts:{},resources:{}};
for await(const line of createInterface({input:process.stdin})) {
 const r=JSON.parse(line);appendFileSync(log,JSON.stringify(r)+'\\n');
 if(r.method==='initialize') {
   const capabilities=mode==='empty'?{}:mode==='tools-only'||mode==='wait'?{tools:{}}:mode==='prompts-only'?{prompts:{}}:mode==='resources-only'?{resources:{}}:mode==='bad-capability'?{tools:true}:all;
   send({jsonrpc:'2.0',id:r.id,result:{protocolVersion:'2025-11-25',serverInfo:{name:'fixture',version:'1'},capabilities}});
 } else if(r.method==='notifications/initialized') {
   send({jsonrpc:'2.0',method:'notifications/resources/updated',params:{uri:'test://idle'}});
   send({jsonrpc:'2.0',method:'notifications/tools/list_changed'});
 } else if(r.method.endsWith('/list')) {
   if(mode==='wait')continue;
   const field={'tools/list':'tools','prompts/list':'prompts','resources/list':'resources','resources/templates/list':'resourceTemplates'}[r.method];
   if(!field)process.exit(51);
   const has=Object.hasOwn(r.params,'cursor');
   let result={[field]:[descriptor[field]]};
   if(mode==='supported-empty')result[field]=[];
   if(mode==='empty-cursor'&&!has){result[field]=[];result.nextCursor='';}
   if(mode==='empty-cursor'&&has&&r.params.cursor!=='')process.exit(52);
   if(mode==='template-cycle'&&field==='resourceTemplates')result.nextCursor=!has?'a':r.params.cursor==='a'?'b':'a';
   if(mode==='page-limit')result.nextCursor='more';
   if(mode==='items')result[field].push(descriptor[field]);
   if(mode==='bytes')result.padding='x'.repeat(3000);
   if(mode==='bad-prompts'&&field==='prompts')result[field]=null;
   send({jsonrpc:'2.0',id:r.id,result});
 } else process.exit(53);
}
`;

function discover(mode, discoveryLimits = {}) {
  const directory = mkdtempSync(join(tmpdir(), 'htlk-discovery-'));
  const log = join(directory, 'peer.jsonl');
  const config = join(directory, 'host.json');
  const catalog = join(directory, 'catalog.json');
  writeFileSync(log, '');
  writeFileSync(config, JSON.stringify({ transport: 'stdio', alias: 'fixture', deployment_id: 'test',
    program: process.execPath, arguments: ['--input-type=module', '-e', peer, mode, log],
    environment: {}, discovery_limits: discoveryLimits }));
  const result = spawnSync(process.execPath, ['tools/discover-mcp.mjs', config, catalog], { encoding: 'utf8', timeout: 5000 });
  const requests = readFileSync(log, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
  assert.equal(result.error, undefined);
  assert(!requests.some(request => ['tools/call', 'resources/read', 'prompts/get'].includes(request.method)));
  return { directory, catalog, result, requests };
}

test('discovery gates each inventory independently and distinguishes unsupported from complete empty pages', () => {
  for (const [mode, methods] of [
    ['empty', []], ['tools-only', ['tools/list']], ['prompts-only', ['prompts/list']],
    ['resources-only', ['resources/list', 'resources/templates/list']],
    ['supported-empty', ['tools/list', 'prompts/list', 'resources/list', 'resources/templates/list']],
  ]) {
    const run = discover(mode);
    try {
      assert.equal(run.result.status, 0, run.result.stderr);
      assert.deepEqual(run.requests.filter(request => request.method.endsWith('/list')).map(request => request.method), methods);
      const report = JSON.parse(readFileSync(run.catalog + '.discovery.json', 'utf8'));
      const expected = { tools: 'tools/list', prompts: 'prompts/list', resources: 'resources/list', templates: 'resources/templates/list' };
      for (const [key, method] of Object.entries(expected)) {
        assert.deepEqual(report.inventories[key], methods.includes(method)
          ? { status: 'complete', pages: 1, items: mode === 'supported-empty' ? 0 : 1 }
          : { status: 'unsupported', pages: 0, items: 0 });
      }
    } finally { rmSync(run.directory, { recursive: true, force: true }); }
  }
});

test('discovery preserves empty opaque cursors for every inventory', () => {
  const run = discover('empty-cursor');
  try {
    assert.equal(run.result.status, 0, run.result.stderr);
    for (const method of ['tools/list', 'prompts/list', 'resources/list', 'resources/templates/list']) {
      const calls = run.requests.filter(request => request.method === method);
      assert.deepEqual(calls.map(call => call.params), [{}, { cursor: '' }]);
    }
    const report = JSON.parse(readFileSync(run.catalog + '.discovery.json', 'utf8'));
    for (const inventory of Object.values(report.inventories)) assert.deepEqual(inventory, { status: 'complete', pages: 2, items: 1 });
  } finally { rmSync(run.directory, { recursive: true, force: true }); }
});

test('incomplete or malformed inventories fail without publishing partial catalogs', () => {
  for (const [mode, limits, message] of [
    ['template-cycle', {}, /Repeated discovery cursor/],
    ['page-limit', { pages: 1 }, /Discovery page limit exhausted/],
    ['items', { items: 1 }, /Catalog item limit exceeded/],
    ['bytes', { frame_bytes: 512 }, /MCP discovery frame limit exceeded/],
    ['bytes', { total_bytes: 1000 }, /MCP discovery total byte limit exceeded/],
    ['wait', { timeout_ms: 200 }, /MCP discovery timeout/],
    ['bad-prompts', {}, /Invalid discovery page/],
    ['bad-capability', {}, /Invalid server capability/],
  ]) {
    const run = discover(mode, limits);
    try {
      assert.notEqual(run.result.status, 0, mode);
      assert.match(run.result.stderr, message, mode);
      assert(!existsSync(run.catalog), mode);
      assert(!existsSync(run.catalog + '.discovery.json'), mode);
      if (mode === 'template-cycle') assert.equal(run.requests.filter(request => request.method === 'resources/templates/list').length, 3);
      if (mode === 'page-limit') assert.equal(run.requests.filter(request => request.method === 'tools/list').length, 1);
    } finally { rmSync(run.directory, { recursive: true, force: true }); }
  }
});
