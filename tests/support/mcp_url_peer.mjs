// Independent MCP 2025-11-25 URL peer. No SDK or runtime validators are shared.
import http from 'node:http';
import { createInterface } from 'node:readline';

const mode = process.argv[2] ?? 'stdio';
const scenario = process.argv[3] ?? 'required';
let tools = 0, forms = 0, urls = 0, fetches = 0, parent, stream;
const callbacks = new Map();
const rpc = (id, result) => ({ jsonrpc: '2.0', id, result });
const notification = id => ({ jsonrpc: '2.0', method: 'notifications/elicitation/complete', params: {
  elicitationId: id, url: 'https://private.invalid/never-disclose', _meta: { private: 'notification-private' },
} });
const barrier = () => ({ jsonrpc: '2.0', method: 'test/url_barrier', params: {} });
const sse = messages => messages.map(message => `data: ${JSON.stringify(message)}\n\n`).join('');
const stdout = message => process.stdout.write(`${JSON.stringify(message)}\n`);
let base;
function params(id) {
  return { mode: 'url', elicitationId: id, url: `${base}/flow?private=original-query`,
    message: 'private URL explanation', _meta: { private: 'original-url-metadata' } };
}
function pulse() {
  return [notification('unknown-private-id'), notification('form-only'),
    notification(scenario === 'callback' ? 'callback-url' : 'ready'),
    notification(scenario === 'callback' ? 'callback-url' : 'ready'), barrier()];
}
function handle(request, send, finish) {
  if (request.method === 'initialize') {
    send(rpc(request.id, { protocolVersion: '2025-11-25', serverInfo: { name: 'url-peer', version: '1' }, capabilities: { tools: {} } }));
  } else if (request.method === 'notifications/initialized' || request.method === 'notifications/cancelled') {
    finish?.();
  } else if (request.method === 'tools/call') {
    tools++;
    if (scenario === 'idle-callback') {
      parent = { id: request.id, send };
      stream.write(sse([{ jsonrpc: '2.0', id: 'url', method: 'elicitation/create', params: params('callback-url') }]));
    } else if (scenario === 'callback') {
      parent = { id: request.id, send };
      callbacks.set('form', parent);
      send({ jsonrpc: '2.0', id: 'form', method: 'elicitation/create', params: {
        message: 'Form explanation', requestedSchema: { type: 'object', properties: { choice: { type: 'string', enum: ['ok'] } }, required: ['choice'] },
      } });
    } else {
      const elicitations = [params('ready'), params('waiting')];
      if (scenario === 'malformed') elicitations.push({ ...params('bad'), mode: 'form' });
      const error = { jsonrpc: '2.0', id: request.id, error: {
        code: -32042, message: 'private original RPC error', data: { elicitations, private: 'original-error-metadata' },
      } };
      // One write/end deliberately coalesces the error and trailing frames.
      if (scenario === 'required' && mode !== 'json') send([notification('unknown-private-id'), error, ...pulse()]);
      else send(error);
    }
  } else if (request.method === 'test/pulse') {
    if (mode === 'json') {
      if (!stream) throw new Error('idle GET must already be attached');
      stream.write(sse(pulse()));
      send(rpc(request.id, {}));
    } else send([...pulse(), rpc(request.id, {})]);
  } else if (request.method === 'test/stats') {
    send(rpc(request.id, { tools, forms, urls, fetches }));
  } else if (request.id === 'form') {
    forms++;
    if (request.result?.action !== 'accept' || request.result.content?.choice !== 'ok') throw new Error('invalid form reply');
    callbacks.set('url', parent);
    parent.send({ jsonrpc: '2.0', id: 'url', method: 'elicitation/create', params: params('callback-url') });
    finish?.();
  } else if (request.id === 'url') {
    urls++;
    if (request.result?.action !== 'accept' || 'content' in request.result) throw new Error('URL consent is not form content');
    parent.send(rpc(parent.id, { content: [], structuredContent: { accepted: true } }));
    finish?.();
  } else throw new Error(`unexpected request ${JSON.stringify(request)}`);
}

const server = http.createServer(async (req, res) => {
  if (req.url.startsWith('/flow')) { fetches++; res.end('secret external user interaction'); return; }
  if (req.method === 'DELETE') { stream?.end(); res.writeHead(204).end(); return; }
  if (req.method === 'GET') {
    if (mode !== 'json') { res.writeHead(405).end(); return; }
    stream = res;
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write(': attached\n\n');
    return;
  }
  let bytes = '';
  for await (const chunk of req) bytes += chunk;
  const request = JSON.parse(bytes);
  const callbackResponse = !request.method;
  const send = messages => {
    messages = Array.isArray(messages) ? messages : [messages];
    const isCallback = messages.some(message => message.method && message.id !== undefined);
    if (mode === 'json' && !isCallback && !res.headersSent) {
      if (messages.length !== 1) throw new Error('JSON response must contain exactly one message');
      res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(messages[0]));
    } else {
      if (!res.headersSent) res.writeHead(200, { 'content-type': 'text/event-stream' });
      if (isCallback) res.write(sse(messages));
      else res.end(sse(messages));
    }
  };
  handle(request, send, () => { if (!res.headersSent) res.writeHead(202).end(); });
  if (callbackResponse && !res.headersSent) res.writeHead(202).end();
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
base = `http://127.0.0.1:${server.address().port}`;
if (mode === 'stdio') {
  for await (const line of createInterface({ input: process.stdin })) {
    handle(JSON.parse(line), values => (Array.isArray(values) ? values : [values]).forEach(stdout));
  }
  server.close();
} else process.stdout.write(`${server.address().port}\n`);
