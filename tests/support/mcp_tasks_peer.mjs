// Independent wire peer: intentionally no SDK or shared Rust validators.
import { createInterface } from 'node:readline';
import { createServer } from 'node:http';

const scenario = process.argv[1] ?? 'normal';
const transport = process.argv[2] ?? 'stdio';
const related = 'io.modelcontextprotocol/related-task';
const counters = { creates: 0, gets: 0, results: 0, cancels: 0, rpcCancels: 0, earlyPoll: 0, lists: 0 };
let task;
let lastPoll = 0;
let waitingResult;
let terminal = false;
let inlineCall;
const tasksCapability = scenario === 'no-cap' ? {} : {
  tasks: { requests: { tools: { call: {} } },
    ...(scenario === 'no-list-cancel' ? {} : { list: {}, cancel: {} }) },
};
function assert(value, message) {
  if (!value) throw new Error(`peer invariant: ${message}`);
}
function snapshot(status = 'working') {
  return { taskId: 'opaque/private/task', status, createdAt: new Date().toISOString(),
    lastUpdatedAt: new Date().toISOString(), ttl: 60000, pollInterval: 25 };
}
function changed(status) {
  task = { ...task, status, lastUpdatedAt: new Date(Math.max(Date.now(), Date.parse(task.lastUpdatedAt) + 1)).toISOString() };
  terminal = ['completed', 'failed', 'cancelled'].includes(status);
  return task;
}
function reply(r, send, result) { send({ jsonrpc: '2.0', id: r.id, result }); }
function error(r, send, code, message, data) { send({ jsonrpc: '2.0', id: r.id, error: { code, message, ...(data ? { data } : {}) } }); }
function result(r, send) {
  if (scenario === 'remote-error') {
    error(r, send, -32050, 'private-underlying-error', { protected: true });
  } else {
    reply(r, send, { content: [{ type: 'text', text: 'actual-operation-result' }],
      isError: scenario === 'tool-error',
      _meta: { [related]: { taskId: scenario === 'wrong-related' ? 'another-task' : task.taskId } },
      counters: { ...counters } });
  }
}
async function receive(r, send, disconnect) {
  if (r.id === 'sampling-task' || r.id === 'elicitation-task') {
    assert(r.result && !r.result.task, 'unsupported augmentation returns the ordinary operation result');
    if (r.id === 'sampling-task') {
      assert(r.result.model === 'fixture-model', 'ordinary sampling result');
      send({ jsonrpc: '2.0', id: 'elicitation-task', method: 'elicitation/create', params: {
        message: 'Continue?', requestedSchema: { type: 'object', properties: {} }, task: ['ignored-unsupported-metadata'],
      } });
    } else {
      assert(r.result.action === 'decline', 'ordinary elicitation result');
      reply(inlineCall, send, { content: [], ignoredAugmentation: true });
    }
  } else if (r.method === 'initialize') {
    // Outbound task use must not claim installed client-direction task services.
    assert(!r.params.capabilities.tasks, 'unimplemented receiving task capability');
    reply(r, send, { protocolVersion: '2025-11-25', serverInfo: { name: 'tasks-peer', version: '1' }, capabilities: { tools: {}, ...tasksCapability } });
  } else if (r.method === 'notifications/initialized') {
    // Deliberately never send notifications/tasks/status.
  } else if (r.method === 'test/stats') {
    reply(r, send, { ...counters });
  } else if (r.method === 'tools/call') {
    if (!r.params.task) {
      if (scenario === 'receiving-unsupported') {
        inlineCall = r;
        send({ jsonrpc: '2.0', id: 'sampling-task', method: 'sampling/createMessage', params: {
          messages: [{ role: 'user', content: { type: 'text', text: 'hello' } }], maxTokens: 10, task: false,
        } });
        return;
      }
      reply(r, send, { content: [], inline: true });
      return;
    }
    counters.creates++;
    assert(counters.creates === 1, 'blind duplicate task creation');
    if (scenario === 'create-error') { error(r, send, -32051, 'private-creation-error', { refused: true }); return; }
    task = snapshot();
    lastPoll = Date.now();
    if (scenario === 'lost-ack') { disconnect(); return; }
    if (scenario === 'bad-initial') task.status = 'completed';
    if (scenario === 'negative-ttl') task.ttl = -1;
    if (scenario === 'fractional-ttl') task.ttl = 0.5;
    if (scenario === 'negative-poll') task.pollInterval = -1;
    if (scenario === 'missing-ttl') delete task.ttl;
    if (scenario === 'bad-date') task.createdAt = '2026-02-30T00:00:00Z';
    if (scenario === 'backward-date') task.lastUpdatedAt = '2000-01-01T00:00:00Z';
    if (scenario === 'expired') {
      task.createdAt = task.lastUpdatedAt = new Date(Date.now() - 100).toISOString();
      task.ttl = 1;
    }
    if (scenario === 'null-ttl') task.ttl = null;
    reply(r, send, { task });
  } else if (['tasks/get', 'tasks/result', 'tasks/cancel'].includes(r.method)) {
    assert(!r.params._meta?.[related], 'task control requests use taskId, not related metadata');
    assert(r.params.taskId === task.taskId, 'exact target task ID');
    if (r.method === 'tasks/get') {
      counters.gets++;
      if (Date.now() - lastPoll < 20) counters.earlyPoll++;
      lastPoll = Date.now();
      if (scenario === 'identity-drift') { reply(r, send, { ...task, taskId: 'different' }); return; }
      if (scenario === 'ttl-drift') { reply(r, send, { ...task, ttl: 50000 }); return; }
      if (scenario === 'time-regression') { reply(r, send, { ...task, lastUpdatedAt: new Date(Date.parse(task.createdAt) - 1).toISOString() }); return; }
      if (scenario === 'terminal-regression') {
        reply(r, send, changed(counters.gets === 1 ? 'completed' : 'working'));
        return;
      }
      if (['cancel', 'infinite', 'cancel-in-result'].includes(scenario)) {
        reply(r, send, changed(scenario === 'cancel-in-result' ? 'input_required' : 'working'));
      } else if (scenario === 'input-required' && counters.gets === 1) {
        reply(r, send, changed('input_required'));
      } else {
        reply(r, send, changed(counters.gets < 2 ? 'working' : ['tool-error', 'remote-error'].includes(scenario) ? 'failed' : 'completed'));
      }
    } else if (r.method === 'tasks/result') {
      counters.results++;
      if (scenario === 'cancel-in-result') { waitingResult = { r, send }; return; }
      // On input_required the result stream is the rendezvous that unblocks work.
      if (!terminal) changed('completed');
      result(r, send);
    } else {
      counters.cancels++;
      if (terminal) { error(r, send, -32602, 'Cannot cancel terminal task'); return; }
      reply(r, send, changed('cancelled'));
    }
  } else if (r.method === 'tasks/list') {
    counters.lists++;
    const first = snapshot();
    const second = { ...first, taskId: 'second/private/task' };
    if (scenario === 'list-repeat') {
      reply(r, send, { tasks: [{ ...first, taskId: `page-${counters.lists}` }], nextCursor: '' });
    } else if (scenario === 'list-duplicate') {
      reply(r, send, { tasks: [first, first] });
    } else if (r.params.cursor === undefined) {
      reply(r, send, { tasks: [first], nextCursor: '' });
    } else {
      assert(r.params.cursor === '', 'empty opaque cursor preserved');
      reply(r, send, { tasks: [second] });
    }
  } else if (r.method === 'notifications/cancelled') {
    counters.rpcCancels++;
    if (waitingResult?.r.id === r.params.requestId) {
      error(waitingResult.r, waitingResult.send, -32800, 'RPC wait cancelled; task still working');
      waitingResult = undefined;
    }
  } else {
    throw new Error(`unexpected request ${JSON.stringify(r)}`);
  }
}

if (transport === 'stdio') {
  const send = value => process.stdout.write(`${JSON.stringify(value)}\n`);
  for await (const line of createInterface({ input: process.stdin })) {
    await receive(JSON.parse(line), send, () => process.exit(0));
  }
} else {
  const server = createServer(async (req, res) => {
    if (req.method === 'GET') { res.writeHead(405).end(); return; }
    if (req.method === 'DELETE') { res.writeHead(200).end(); return; }
    let body = '';
    for await (const chunk of req) body += chunk;
    const r = JSON.parse(body);
    if (r.id === undefined) { await receive(r, () => {}, () => req.socket.destroy()); res.writeHead(202).end(); return; }
    const send = value => {
      if (transport === 'http-sse') {
        res.writeHead(200, { 'Content-Type': 'text/event-stream' });
        res.end(`data: ${JSON.stringify(value)}\n\n`);
      } else {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify(value));
      }
    };
    await receive(r, send, () => req.socket.destroy());
  });
  server.listen(0, '127.0.0.1', () => process.stdout.write(`${server.address().port}\n`));
}
