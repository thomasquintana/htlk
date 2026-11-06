// Bounded host-side stdio discovery. No tools/call, schema invention or compiler I/O.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";

assert.equal(process.argv.length, 4, "Usage: node tools/discover-mcp.mjs HOST.json CATALOGS.json");
const configPath = resolve(process.argv[2]);
assert(statSync(configPath).size <= 65536, "Host configuration too large");
const config = JSON.parse(readFileSync(configPath, "utf8"));
assert.equal(config.transport, "stdio");
assert.equal(typeof config.program, "string");
assert(Array.isArray(config.arguments));
assert(config.arguments.every(argument => typeof argument === "string"));
assert.equal(typeof config.alias, "string");
assert.equal(typeof config.deployment_id, "string");
const defaults = { pages: 128, items: 4096, frame_bytes: 2 * 1024 * 1024,
  total_bytes: 16 * 1024 * 1024, timeout_ms: 30000 };
const configuredLimits = config.discovery_limits ?? {};
assert(configuredLimits && typeof configuredLimits === "object" && !Array.isArray(configuredLimits), "Invalid discovery limits");
assert(Object.keys(configuredLimits).every(key => Object.hasOwn(defaults, key)), "Unknown discovery limit");
const limits = { ...defaults, ...configuredLimits };
for (const key of Object.keys(defaults)) {
  assert(Number.isSafeInteger(limits[key]) && limits[key] > 0 && limits[key] <= defaults[key], "Invalid discovery limit");
}
const outputPath = resolve(process.argv[3]);
const child = spawn(config.program, config.arguments, {
  env: { ...process.env, ...config.environment }, stdio: ["pipe", "pipe", "pipe"],
});
const closed = new Promise(resolve => child.once("close", resolve));
let stderrBytes = 0;
const pending = new Map();
let sequence = 0;
let inspected = 0;
let terminalError;
const stop = error => {
  if (terminalError) return;
  terminalError = error;
  for (const handler of pending.values()) handler.reject(error);
  pending.clear();
  child.stdout.destroy();
  child.kill("SIGKILL");
};
child.stderr.on("data", data => { stderrBytes += data.length; if (stderrBytes > 1024 * 1024) stop(new Error("MCP stderr limit exceeded")); });
const send = value => child.stdin.write(JSON.stringify(value) + "\n");
const object = value => value !== null && typeof value === "object" && !Array.isArray(value);
const frame = Buffer.allocUnsafe(limits.frame_bytes);
const decoder = new TextDecoder("utf-8", { fatal: true });
let frameBytes = 0;
const receive = bytes => {
  try {
    const message = JSON.parse(decoder.decode(bytes));
    assert(object(message) && message.jsonrpc === "2.0");
    if (typeof message.method === "string" && message.id !== undefined) {
      assert(typeof message.id === "string" || Number.isSafeInteger(message.id));
      send(message.method === "ping" ? { jsonrpc: "2.0", id: message.id, result: {} }
        : { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Unsupported discovery-client operation" } });
    } else if (pending.has(message.id)) {
      assert(Object.hasOwn(message, "result") !== Object.hasOwn(message, "error"));
      const handler = pending.get(message.id); pending.delete(message.id);
      Object.hasOwn(message, "error") ? handler.reject(new Error("MCP discovery RPC failed")) : handler.resolve(message.result);
    }
  } catch { stop(new Error("Invalid MCP discovery frame")); }
};
child.stdout.on("data", data => {
  inspected += data.length;
  if (inspected > limits.total_bytes) { stop(new Error("MCP discovery total byte limit exceeded")); return; }
  let offset = 0;
  while (offset < data.length && !terminalError) {
    const newline = data.indexOf(10, offset);
    const end = newline === -1 ? data.length : newline;
    const size = end - offset;
    if (frameBytes + size > limits.frame_bytes) { stop(new Error("MCP discovery frame limit exceeded")); return; }
    data.copy(frame, frameBytes, offset, end); frameBytes += size;
    if (newline === -1) break;
    receive(frame.subarray(0, frameBytes)); frameBytes = 0; offset = newline + 1;
  }
});
child.on("error", () => stop(new Error("MCP process unavailable")));
child.on("exit", () => stop(new Error("MCP process exited")));
child.stdin.on("error", () => stop(new Error("MCP input unavailable")));
const ownerTimeout = setTimeout(() => stop(new Error("MCP discovery timeout")), limits.timeout_ms);
ownerTimeout.unref();
const request = (method, params) => new Promise((resolve, reject) => {
  if (terminalError) { reject(terminalError); return; }
  const id = ++sequence;
  pending.set(id, { resolve, reject });
  send({ jsonrpc: "2.0", id, method, params });
});
try {
  const initialized = await request("initialize", { protocolVersion: "2025-11-25", capabilities: {},
    clientInfo: { name: "htlk-live-discovery", version: "1" } });
  assert.equal(initialized.protocolVersion, "2025-11-25", "Pinned protocol not supported");
  assert(object(initialized.serverInfo) && typeof initialized.serverInfo.name === "string"
    && typeof initialized.serverInfo.version === "string", "Invalid server identity");
  assert(object(initialized.capabilities), "Invalid server capabilities");
  send({ jsonrpc: "2.0", method: "notifications/initialized" });
  let totalItems = 0;
  const inventories = {};
  const enumerate = async (name, capability, method, field) => {
    const supported = initialized.capabilities[capability];
    inventories[name] = { status: "unsupported", pages: 0, items: 0 };
    if (supported === undefined) return [];
    assert(object(supported), "Invalid server capability");
    const items = [];
    const cursors = new Set();
    let cursor;
    for (let page = 0; page < limits.pages; page++) {
      const result = await request(method, cursor === undefined ? {} : { cursor });
      assert(object(result) && Array.isArray(result[field]), "Invalid discovery page");
      assert(result[field].every(object), "Invalid discovery descriptor");
      totalItems += result[field].length;
      assert(totalItems <= limits.items, "Catalog item limit exceeded");
      for (const item of result[field]) items.push(item);
      inventories[name] = { status: "complete", pages: page + 1, items: items.length };
      if (result.nextCursor === undefined) return items;
      assert.equal(typeof result.nextCursor, "string");
      assert(!cursors.has(result.nextCursor), "Repeated discovery cursor");
      cursors.add(result.nextCursor); cursor = result.nextCursor;
    }
    throw new Error("Discovery page limit exhausted");
  };
  const tools = await enumerate("tools", "tools", "tools/list", "tools");
  const prompts = await enumerate("prompts", "prompts", "prompts/list", "prompts");
  const resources = await enumerate("resources", "resources", "resources/list", "resources");
  const templates = await enumerate("templates", "resources", "resources/templates/list", "resourceTemplates");
  if (terminalError) throw terminalError;
  const servers = { [config.alias]: { deployment_id: config.deployment_id, transport: "stdio",
    implementation_name: initialized.serverInfo.name, implementation_version: initialized.serverInfo.version } };
  const catalog = () => ({ catalog_version: "0.1", mcp_protocol_version: "2025-11-25", servers, schema_documents: {} });
  const catalogs = { tools: { ...catalog(), tools: tools.map(descriptor => ({ server: config.alias, descriptor })) },
    resources: { ...catalog(), resources: resources.map(descriptor => ({ server: config.alias, descriptor })),
      templates: templates.map(descriptor => ({ server: config.alias, descriptor })) },
    prompts: { ...catalog(), prompts: prompts.map(descriptor => ({ server: config.alias, descriptor })) } };
  const encoded = JSON.stringify(catalogs, null, 2) + "\n";
  assert(Buffer.byteLength(encoded) <= 16 * 1024 * 1024, "Catalog representation too large");
  mkdirSync(dirname(outputPath), { recursive: true });
  writeFileSync(outputPath, encoded, { flag: "wx" });
  writeFileSync(outputPath + ".discovery.json", JSON.stringify({ protocol: initialized.protocolVersion,
    serverInfo: initialized.serverInfo, capabilities: initialized.capabilities,
    tools: tools.length, tools_without_output_schema: tools.filter(tool => tool.outputSchema === undefined).length,
     prompts: prompts.length, resources: resources.length, templates: templates.length, inventories,
     tools_call_count: 0, content_read_count: 0, inspected_stdout_bytes: inspected }, null, 2) + "\n", { flag: "wx" });
  console.log(JSON.stringify({ server: initialized.serverInfo, tools: tools.length,
    prompts: prompts.length, resources: resources.length, templates: templates.length,
    without_output_schema: tools.filter(tool => tool.outputSchema === undefined).length,
    tools_call_count: 0, content_read_count: 0 }, null, 2));
} finally {
  clearTimeout(ownerTimeout); child.stdin.end();
  const timer = setTimeout(() => child.kill("SIGKILL"), 2000); timer.unref();
  await closed;
  clearTimeout(timer);
}
