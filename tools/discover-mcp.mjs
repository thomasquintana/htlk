// Bounded host-side stdio discovery. No tools/call, schema invention or compiler I/O.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { createInterface } from "node:readline";

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
const outputPath = resolve(process.argv[3]);
const child = spawn(config.program, config.arguments, {
  env: { ...process.env, ...config.environment }, stdio: ["pipe", "pipe", "pipe"],
});
const closed = new Promise(resolve => child.once("close", resolve));
let stderrBytes = 0;
child.stderr.on("data", data => { stderrBytes += data.length; if (stderrBytes > 1024 * 1024) child.kill("SIGKILL"); });
const pending = new Map();
let sequence = 0;
let inspected = 0;
child.stdout.on("data", data => { inspected += data.length; if (inspected > 16 * 1024 * 1024) { child.stdout.destroy(); child.kill("SIGKILL"); } });
const send = value => child.stdin.write(JSON.stringify(value) + "\n");
const reader = createInterface({ input: child.stdout, crlfDelay: Infinity });
reader.on("line", line => {
  if (inspected > 16 * 1024 * 1024 || Buffer.byteLength(line) > 2 * 1024 * 1024) {
    child.kill("SIGKILL"); return;
  }
  try {
    const message = JSON.parse(line);
    if (message.method && message.id !== undefined) {
      send(message.method === "ping" ? { jsonrpc: "2.0", id: message.id, result: {} }
        : { jsonrpc: "2.0", id: message.id, error: { code: -32601, message: "Unsupported discovery-client operation" } });
    } else if (pending.has(message.id)) {
      const handler = pending.get(message.id); pending.delete(message.id);
      message.error ? handler.reject(new Error("MCP discovery RPC failed")) : handler.resolve(message.result);
    }
  } catch { child.kill(); }
});
child.on("error", () => { for (const handler of pending.values()) handler.reject(new Error("MCP process unavailable")); });
child.on("exit", () => { for (const handler of pending.values()) handler.reject(new Error("MCP process exited")); });
child.stdin.on("error", () => { for (const handler of pending.values()) handler.reject(new Error("MCP input unavailable")); });
const request = (method, params) => new Promise((resolve, reject) => {
  const id = ++sequence;
  const timeout = setTimeout(() => { pending.delete(id); reject(new Error("MCP discovery timeout")); child.kill(); }, 30000);
  pending.set(id, { resolve: value => { clearTimeout(timeout); resolve(value); }, reject: error => { clearTimeout(timeout); reject(error); } });
  send({ jsonrpc: "2.0", id, method, params });
});
try {
  const initialized = await request("initialize", { protocolVersion: "2025-11-25", capabilities: {},
    clientInfo: { name: "htlk-live-discovery", version: "1" } });
  assert.equal(initialized.protocolVersion, "2025-11-25", "Pinned protocol not supported");
  send({ jsonrpc: "2.0", method: "notifications/initialized" });
  const tools = [];
  const cursors = new Set();
  let cursor;
  for (let page = 0; page < 128; page++) {
    const result = await request("tools/list", cursor === undefined ? {} : { cursor });
    assert(Array.isArray(result.tools));
    tools.push(...result.tools);
    assert(tools.length <= 4096, "Tool inventory exceeded limit");
    if (result.nextCursor === undefined) { cursor = undefined; break; }
    assert.equal(typeof result.nextCursor, "string");
    assert(!cursors.has(result.nextCursor), "Repeated discovery cursor");
    cursors.add(result.nextCursor); cursor = result.nextCursor;
  }
  assert.equal(cursor, undefined, "Discovery page limit exhausted");
  const servers = { [config.alias]: { deployment_id: config.deployment_id, transport: "stdio",
    implementation_name: initialized.serverInfo.name, implementation_version: initialized.serverInfo.version } };
  const catalog = () => ({ catalog_version: "0.1", mcp_protocol_version: "2025-11-25", servers, schema_documents: {} });
  const catalogs = { tools: { ...catalog(), tools: tools.map(descriptor => ({ server: config.alias, descriptor })) },
    resources: { ...catalog(), resources: [], templates: [] }, prompts: { ...catalog(), prompts: [] } };
  const encoded = JSON.stringify(catalogs, null, 2) + "\n";
  assert(Buffer.byteLength(encoded) <= 16 * 1024 * 1024, "Catalog representation too large");
  mkdirSync(dirname(outputPath), { recursive: true });
   writeFileSync(outputPath, encoded, { flag: "wx" });
   writeFileSync(outputPath + ".discovery.json", JSON.stringify({ protocol: initialized.protocolVersion,
    serverInfo: initialized.serverInfo, capabilities: initialized.capabilities,
    tools: tools.length, tools_without_output_schema: tools.filter(tool => tool.outputSchema === undefined).length,
     tools_call_count: 0, inspected_stdout_bytes: inspected }, null, 2) + "\n", { flag: "wx" });
  console.log(JSON.stringify({ server: initialized.serverInfo, tools: tools.length,
    without_output_schema: tools.filter(tool => tool.outputSchema === undefined).length, tools_call_count: 0 }, null, 2));
} finally {
  reader.close(); child.stdin.end();
  const timer = setTimeout(() => child.kill("SIGKILL"), 2000); timer.unref();
   await closed;
  clearTimeout(timer);
}
