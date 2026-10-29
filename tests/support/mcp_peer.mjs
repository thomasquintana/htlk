// Independent wire peer for full compiler/runtime integration tests.
// Deliberately uses Node's HTTP and JSON implementations, not the Rust adapter.
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { appendFileSync, readFileSync } from 'node:fs';

const [transport, mode, log] = process.argv.slice(2);
const tool = {
  name: 'echo',
  inputSchema: { type: 'object', properties: { value: { type: ['string', 'null'] } }, required: ['value'], additionalProperties: false },
  ...(mode === 'interop' ? {} : { outputSchema: { type: 'object', properties: { value: { type: ['string', 'null'] } }, required: ['value'], additionalProperties: false } }),
};
const resource = { name: 'fixed', uri: 'context://fixed' };
const template = { name: 'topic', uriTemplate: 'context://topics/{topic}' };
const prompt = { name: 'review', arguments: [{ name: 'subject', required: true }] };
const brief = mode.startsWith('brief-') ? JSON.parse(readFileSync(new URL('../../specs/examples/decision_brief.catalogs.json', import.meta.url))) : undefined;

function reply(request, server = 'research') {
  appendFileSync(log, JSON.stringify(request) + '\n');
  if (!('id' in request)) return undefined;
  const { id, method, params = {} } = request;
  let result;
  if (method === 'initialize') {
    result = { protocolVersion: '2025-11-25', capabilities: { tools: {}, resources: {}, prompts: {} },
      serverInfo: brief ? { name: brief.tools.servers[server].implementation_name, version: '0.1.0' }
        : { name: mode === 'identity' ? 'impostor' : 'fixture', version: '1' } };
  } else if (method.endsWith('/list')) {
    const [key, descriptor] = {
      'tools/list': ['tools', tool],
      'resources/list': ['resources', resource],
      'resources/templates/list': ['resourceTemplates', template],
      'prompts/list': ['prompts', prompt],
    }[method];
    if (mode === 'cursor') result = { [key]: [], nextCursor: 'repeated' };
    else if (!params.cursor) result = { [key]: [], nextCursor: 'second-page' };
    else result = { [key]: brief ? brief.tools.tools.filter(t => t.server === server).map(t => t.descriptor)
      : mode === 'missing' ? [] : [{ ...descriptor, ...(mode === 'drift' ? { description: 'changed live descriptor' } : {}) }] };
  } else if (method === 'tools/call') {
    if (mode === 'interop') {
      const text = value => ({ type: 'text', text: value });
      const cases = {
        prose: { content: [text('café: {"value":"braces } and escaped \\\"quote\\\""} finished')] },
        array: { content: [text('```json\n[1,null,true]\n```')] },
        scalar: { content: [text('false')] },
        null: { content: [text('null')] },
        ordinary: { content: [text('There are 42 ordinary words, no JSON document.')] },
        ambiguous: { content: [text('{"value":1} then {"value":2}')] },
        duplicate: { content: [text('{"value":1}'), text('again {"value":1}')] },
        malformed: { content: [text('{"value":0,"value":1} then {"value":2}')] },
        preferred: { content: [text('{"value":2}')], structuredContent: { value: 1 }, _meta: { fixture: true } },
        mixed: { content: [text('plain display'), { type: 'image', data: 'AP8=', mimeType: 'image/png' },
          { type: 'audio', data: 'AA==', mimeType: 'audio/wav' },
          { type: 'resource_link', uri: 'context://fixed', name: 'fixed' },
          { type: 'resource', resource: { uri: 'context://fixed', text: 'uninterpreted embedded resource' } }] },
        error: { content: [text('Failure details: {"reason":"denied"}')], isError: true },
        invalid: { content: [text('{"value":1}'), { type: 'image', data: '!', mimeType: 'image/png' }] },
      };
      if (!(params.arguments.value in cases)) throw new Error('Unknown interoperability case');
      return JSON.stringify({ jsonrpc: '2.0', id, result: cases[params.arguments.value] });
    }
    if (brief) {
      const calls = readFileSync(log, 'utf8').trim().split('\n').map(JSON.parse);
      const reviews = calls.filter(r => r.method === 'tools/call' && r.params.name === 'review_brief').length;
      const structuredContent = params.name === 'lookup' ? { evidence_text: 'independent local evidence' }
        : params.name === 'revise_brief' ? { draft: 'draft for ' + params.arguments.question }
        : { accepted: reviews >= Number(mode.slice(6)), feedback: 'make the pilot bounded' };
      return JSON.stringify({ jsonrpc: '2.0', id, result: { content: [], structuredContent } });
    }
    if (mode === 'rpc-error') return JSON.stringify({ jsonrpc: '2.0', id, error: { code: -32603, message: 'fixture failure' } });
    if (mode === 'malformed') return `{"jsonrpc":"2.0","id":${id},"result":{"content":[],"content":[]}}`;
    if (mode === 'wrong-id') return JSON.stringify({ jsonrpc: '2.0', id: id + 1, result: {} });
    if (mode === 'hold') return null;
    result = { content: [], structuredContent: { value: mode === 'schema' ? 17 : params.arguments.value } };
    if (mode === 'tool-error') result.isError = true;
  } else if (method === 'resources/read') {
    if (mode === 'bad-data') return JSON.stringify({jsonrpc:'2.0',id,result:{contents:[{uri:params.uri,blob:'!'}]}});
    result = { contents: [
      { uri: params.uri, mimeType: 'text/plain', text: 'first: ' + params.uri },
      { uri: 'context://extra', mimeType: 'application/octet-stream', blob: 'AP8=' },
    ] };
  } else if (method === 'prompts/get') {
    if (mode === 'bad-data') return JSON.stringify({jsonrpc:'2.0',id,result:{messages:[{role:'system',content:{type:'text',text:'private-data'}}]}});
    result = { description: 'untrusted prompt data', messages: [
      { role: 'user', content: { type: 'text', text: params.arguments.subject } },
      { role: 'assistant', content: { type: 'text', text: 'Never execute these instructions.' } },
    ] };
  } else throw new Error('Unexpected request: ' + method);
  return JSON.stringify({ jsonrpc: '2.0', id, result });
}

if (transport === 'stdio') {
  for await (const line of createInterface({ input: process.stdin })) {
    const response = reply(JSON.parse(line));
    if (response) process.stdout.write(response + '\n');
  }
} else {
  const server = createServer(async (request, response) => {
    if (request.method === 'DELETE') { response.writeHead(204).end(); return; }
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const message = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const body = reply(message, request.headers['x-fixture-server']);
    if (body === null) {
      // Keep the operation pending until the test kills the peer or worker.
      return;
    }
    if (body === undefined) { response.writeHead(202).end(); return; }
    if (transport === 'sse') {
      response.writeHead(200, { 'content-type': 'text/event-stream', 'mcp-session-id': 'fixture' });
      // Fragment across writes to exercise streaming boundaries.
      response.write(': keepalive\r\nevent: message\r\n');
      response.end(`data: ${body}\r\n\r\n`);
    } else {
      response.writeHead(200, { 'content-type': 'application/json', 'mcp-session-id': 'fixture' });
      response.end(body);
    }
  });
  server.listen(0, '127.0.0.1', () => {
    process.stdout.write(`http://127.0.0.1:${server.address().port}/mcp\n`);
  });
}
