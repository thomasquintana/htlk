// Independent session-local state: no log-based lookup or global handle fallback.
import { createServer } from 'node:http';
import { createInterface } from 'node:readline';
import { appendFileSync, readFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';

const [transport, mode, log] = process.argv.slice(2);
const tools = JSON.parse(readFileSync(new URL('../../examples/mcp_capability_spike/tools.json', import.meta.url)));
const sessions = new Map();
const audit = (session, method, params = {}) => appendFileSync(log, JSON.stringify({ session, method, params, pid: process.pid }) + '\n');
function createSession() {
  const session = { id: randomUUID(), initialized: false, pages: new Map(), sandboxes: new Map() };
  sessions.set(session.id, session);
  return session;
}
function reply(session, request) {
  const { id, method, params = {} } = request;
  audit(session.id, method, params);
  if (method === 'initialize') {
    if (session.initialized) throw new Error('Repeated initialization destroys continuity');
    session.initialized = true;
    return { jsonrpc: '2.0', id, result: { protocolVersion: '2025-11-25', capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' } } };
  }
  if (!session.initialized) throw new Error('Session not initialized');
  if (!('id' in request)) return;
  if (method === 'tools/list') return { jsonrpc: '2.0', id, result: { tools } };
  if (method !== 'tools/call') throw new Error('Unexpected method: ' + method);
  if (mode === 'stateful-hold' && params.arguments.marker === 'hold') return null;
  if (mode === 'stateful-error') return { jsonrpc: '2.0', id, result: { content: [], isError: true, structuredContent: {} } };
  const args = params.arguments;
  let structuredContent;
  if (params.name === 'browser_open') {
    const page = randomUUID();
    session.pages.set(page, { marker: args.marker });
    structuredContent = { page };
  } else if (params.name === 'sandbox_create') {
    const sandbox = randomUUID();
    session.sandboxes.set(sandbox, { marker: '' });
    structuredContent = { sandbox };
  } else {
    const page = params.name === 'browser_inspect';
    const handle = page ? args.page : args.sandbox;
    const objects = page ? session.pages : session.sandboxes;
    if (!objects.has(handle)) return { jsonrpc: '2.0', id, result: { content: [], isError: true, structuredContent: {} } };
    const state = objects.get(handle);
    if (page) structuredContent = { page: handle, marker: state.marker };
    else if (params.name === 'sandbox_execute') {
      state.marker = args.marker; // Observable deterministic sandbox mutation.
      structuredContent = { sandbox: handle, executed: true };
    } else if (params.name === 'sandbox_inspect') structuredContent = { sandbox: handle, marker: state.marker };
    else if (params.name === 'sandbox_destroy') {
      objects.delete(handle);
      structuredContent = { sandbox: handle, destroyed: !objects.has(handle) };
    } else throw new Error('Unexpected tool: ' + params.name);
  }
  return { jsonrpc: '2.0', id, result: { content: [], structuredContent } };
}

if (transport === 'stdio') {
  const session = createSession();
  const close = () => { audit(session.id, 'session/closed', { pages: session.pages.size, sandboxes: session.sandboxes.size }); process.exit(0); };
  process.on('SIGTERM', close);
  // Native cleanup kills stdio servers; use stdin EOF evidence when graceful.
  for await (const line of createInterface({ input: process.stdin })) {
    const response = reply(session, JSON.parse(line));
    if (response) process.stdout.write(JSON.stringify(response) + '\n');
  }
  close();
} else {
  createServer(async (request, response) => {
    const sid = request.headers['mcp-session-id'];
    if (request.method === 'DELETE') {
      const session = sessions.get(sid);
      if (!session) { response.writeHead(404).end(); return; }
      audit(sid, 'session/closed', { pages: session.pages.size, sandboxes: session.sandboxes.size });
      sessions.delete(sid);
      response.writeHead(204).end();
      return;
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const message = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const session = message.method === 'initialize' && !sid ? createSession() : sessions.get(sid);
    if (!session) { response.writeHead(404).end(); return; }
    if (mode === 'stateful-expire' && message.method === 'tools/call') {
      audit(session.id, 'session/expired');
      sessions.delete(session.id);
      response.writeHead(404).end(); return;
    }
    const body = reply(session, message);
    if (body === null) return;
    if (body === undefined) { response.writeHead(202).end(); return; }
    const sse = transport === 'sse';
    response.writeHead(200, { 'content-type': sse ? 'text/event-stream' : 'application/json', 'mcp-session-id': session.id });
    response.end(sse ? `event: message\ndata: ${JSON.stringify(body)}\n\n` : JSON.stringify(body));
  }).listen(0, '127.0.0.1', function () {
    process.stdout.write(`http://127.0.0.1:${this.address().port}/mcp\n`);
  });
}
