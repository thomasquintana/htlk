// Independent HTTP peer for persistent client tests. Never imports HTLK code.
import http from "node:http";
let sessionSequence = 0;
const sessions = new Map();
const send = (response, message) => response.write(`data: ${JSON.stringify(message)}\n\n`);
const server = http.createServer(async (request, response) => {
  if (request.headers.authorization !== "Bearer fixture-secret") {
    response.writeHead(401).end(); return;
  }
  if (request.headers["mcp-protocol-version"] !== "2025-11-25") {
    response.writeHead(400).end(); return;
  }
  let session = sessions.get(request.headers["mcp-session-id"]);
  if (request.method === "GET") {
    if (!session) { response.writeHead(404).end(); return; }
    if (process.env.GET_MODE === "405") { response.writeHead(405).end(); return; }
    response.writeHead(200, { "content-type": "text/event-stream" });
    response.flushHeaders();
    const cursor = request.headers["last-event-id"];
    if (cursor?.startsWith("resume-")) {
      const id = Number(cursor.slice(7));
      if (!session.resume.has(id)) { response.destroy(); return; }
      send(response, { jsonrpc: "2.0", id, result: { content: [], posts: session.posts } });
      response.end(); return;
    }
    session.streams.add(response);
    response.on("close", () => session.streams.delete(response));
    if (!cursor) {
      response.write('id: idle-1\r\n');
      send(response, { jsonrpc: "2.0", method: "notifications/resources/updated", params: { uri: "test://idle" } });
    }
    return;
  }
  if (request.method === "DELETE") {
    if (session) for (const stream of session.streams) stream.end();
    sessions.delete(request.headers["mcp-session-id"]);
    response.writeHead(204).end(); return;
  }
  let body = "";
  for await (const bytes of request) body += bytes;
  const rpc = JSON.parse(body);
  const result = value => {
    if (process.env.RESPONSE_MODE === "sse") {
      response.writeHead(200, { "content-type": "text/event-stream" });
      const message = `data: ${JSON.stringify({ jsonrpc: "2.0", id: rpc.id, result: value })}\n\n`;
      // Split UTF-8/framing at arbitrary bytes rather than whole JSON values.
      for (const byte of Buffer.from(message)) response.write(Buffer.from([byte]));
      response.end();
    } else response.writeHead(200, { "content-type": "application/json" })
      .end(JSON.stringify({ jsonrpc: "2.0", id: rpc.id, result: value }));
  };
  if (rpc.method === "initialize") {
    const id = `session-${++sessionSequence}`;
    session = { streams: new Set(), resume: new Set(), posts: 0, reverse: new Map(), waits: new Map(), sampling: new Map() };
    sessions.set(id, session);
    response.setHeader("mcp-session-id", id);
    result({ protocolVersion: "2025-11-25", serverInfo: { name: "peer", version: "1" }, capabilities: { tools: {}, resources: { subscribe: true } } });
    return;
  }
  if (!session) { response.writeHead(404).end(); return; }
  if (!rpc.method && rpc.id?.startsWith?.("sampling-")) {
    const pending = session.sampling.get(rpc.id);
    if (!pending || rpc.error) process.exit(27);
    const value = JSON.stringify(rpc.result);
    if (++pending.replies === 1) {
      pending.value = value;
      response.destroy(); // The client must retry the saved reply, not model work.
    } else {
      if (pending.value !== value) process.exit(28);
      response.writeHead(202).end();
      send(pending.response, { jsonrpc: "2.0", id: pending.parent, result: { content: [], structuredContent: { callbackReplies: pending.replies } } });
      pending.response.end();
    }
    return;
  }
  if (!rpc.method && rpc.id?.startsWith?.("ping-")) {
    response.writeHead(202).end();
    const pending = session.reverse.get(rpc.id);
    if (!pending || !rpc.result) process.exit(23);
    send(pending.response, { jsonrpc: "2.0", id: pending.id, result: { content: [], ping: true } });
    pending.response.end(); return;
  }
  if (rpc.method === "notifications/cancelled") {
    response.writeHead(202).end();
    const pending = session.waits.get(rpc.params.requestId);
    if (pending) {
      send(pending, { jsonrpc: "2.0", id: rpc.params.requestId, error: { code: -1, message: "cancelled" } });
      pending.end();
    }
    return;
  }
  if (rpc.method === "notifications/initialized") { response.writeHead(202).end(); return; }
  if (rpc.method === "tools/list") { result({ tools: [], marker: "café" }); return; }
  if (rpc.method === "tools/call") {
    session.posts++;
    if (rpc.params.name === "raw" || rpc.params.name === "raw-error") {
      const raw = rpc.params.name === "raw"
        ? '{ "content" : [], "value" : 1.0, "text" : "brace }" }'
        : '{ "code" : -32042, "message" : "private", "data" : {"value":1.0} }';
      const key = rpc.params.name === "raw" ? 're\\u0073ult' : 'err\\u006fr';
      const message = `{"jsonrpc":"2.0","id":${rpc.id},"extension":{"result":{"wrong":true}},"${key}":${raw}}`;
      if (process.env.RESPONSE_MODE === "sse") {
        response.writeHead(200, { "content-type": "text/event-stream" }).end(`data: ${message}\n\n`);
      } else response.writeHead(200, { "content-type": "application/json" }).end(message);
      return;
    }
    if (rpc.params.name === "expire") {
      sessions.delete(request.headers["mcp-session-id"]);
      for (const stream of session.streams) stream.end();
      response.writeHead(404).end(); return;
    }
    response.writeHead(200, { "content-type": "text/event-stream" });
    if (rpc.params.name === "sampling") {
      const id = `sampling-${rpc.id}`;
      session.sampling.set(id, { response, parent: rpc.id, replies: 0 });
      send(response, { jsonrpc: "2.0", id, method: "sampling/createMessage", params: { messages: [{ role: "user", content: { type: "text", text: "Question" } }], maxTokens: 10 } });
      return;
    }
    if (rpc.params.name === "resume") {
      session.resume.add(rpc.id);
      response.end(`id: resume-${rpc.id}\nretry: 5\ndata:\n\n`); return;
    }
    if (rpc.params.name === "ping") {
      session.reverse.set(`ping-${rpc.id}`, { id: rpc.id, response });
      send(response, { jsonrpc: "2.0", id: `ping-${rpc.id}`, method: "ping" }); return;
    }
    if (rpc.params.name === "wait") {
      session.waits.set(rpc.id, response);
      response.write(': waiting\n\n'); return;
    }
  }
  response.writeHead(400).end();
});
server.listen(0, "127.0.0.1", () => console.log(server.address().port));
