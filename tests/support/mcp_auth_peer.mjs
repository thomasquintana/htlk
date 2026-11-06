// Independent OAuth resource/authorization server. Tests navigate explicitly;
// the protocol library never follows the authorization redirect itself.
import http from 'node:http';
import {createHash, timingSafeEqual} from 'node:crypto';

const mode = process.env.AUTH_MODE ?? 'oauth';
const authMethod = process.env.AUTH_METHOD ?? 'none';
const requests = [];
const codes = new Map();
const refreshes = new Map();
const issued = new Map();
const nativeRequests = [];
const nativeStreams = new Set();
const pending = new Map();
let nativeCalls = 0;
let serial = 0;
let base;
const resource = () => `${base}/public/mcp`;
const issuer = () => `${base}/tenant`;
const metadata = () => ({resource: resource(), authorization_servers: [issuer()], scopes_supported: ['read']});
const asMetadata = () => ({
  issuer: mode === 'issuer-mismatch' ? `${base}/other` : issuer(),
  authorization_endpoint: `${base}/authorize`, token_endpoint: `${base}/token`,
  response_types_supported: ['code'], grant_types_supported: ['authorization_code', 'refresh_token'],
  code_challenge_methods_supported: mode === 'no-pkce' ? ['plain'] : ['S256'],
  token_endpoint_auth_methods_supported: [authMethod],
  authorization_response_iss_parameter_supported: true,
});
const equal = (a, b) => a.length === b.length && timingSafeEqual(Buffer.from(a), Buffer.from(b));
const server = http.createServer(async (req, res) => {
  const url = new URL(req.url, base);
  let body = '';
  for await (const chunk of req) body += chunk;
  requests.push({path: url.pathname, method: req.method});
  const json = (status, value) => { res.writeHead(status, {'content-type': 'application/json'}); res.end(JSON.stringify(value)); };
  const reject = error => json(400, {error, error_description: 'private-remote-diagnostic'});
  if (url.pathname === '/stats') {
    json(200, {requests: requests.slice(0, -1), issued: issued.size, redeemed: serial, nativeRequests, nativeCalls}); return;
  }
  if (url.pathname === '/public/mcp') {
    const token = req.headers.authorization?.replace(/^Bearer /, '');
    const scope = issued.get(token);
    let rpc;
    if (mode.startsWith('native')) {
      rpc = body ? JSON.parse(body) : null;
      nativeRequests.push({method: req.method, rpc: rpc?.method ?? (rpc ? 'response' : null), cursor: req.headers['last-event-id'] ?? null,
        credential: token ? Number(token.split('-').at(-1)) : null, valid: !!scope});
    }
    if (!scope) {
      const headers = mode === 'fallback' || mode.startsWith('oidc') ? {} : {
        'www-authenticate': `Basic realm="other, space", Bearer resource_metadata="${base}/metadata", scope="read"`,
      };
      if (mode === 'native-invalid-challenge') headers['www-authenticate'] = 'Bearer scope=read, scope=write';
      res.writeHead(401, headers); res.end(); return;
    }
    if (mode.startsWith('native')) {
      const send = (response, message) => response.write(`data: ${JSON.stringify(message)}\n\n`);
      if (req.headers['mcp-protocol-version'] !== '2025-11-25') { json(400, {}); return; }
      const challenge = status => { res.writeHead(status, {'www-authenticate': `Bearer error="insufficient_scope", scope="read write", resource_metadata="${base}/metadata"`}); res.end(); };
      if (req.method === 'GET') {
        if (mode === 'native-get-challenge') { challenge(403); return; }
        res.writeHead(200, {'content-type': 'text/event-stream'}); res.flushHeaders();
        const cursor = req.headers['last-event-id'];
        if (cursor?.startsWith('resume-')) {
          send(res, {jsonrpc: '2.0', id: Number(cursor.slice(7)), result: {content: [], posts: nativeCalls}}); res.end(); return;
        }
        if (!cursor) {
          res.write('id: idle-1\nretry: 5\n');
          send(res, {jsonrpc: '2.0', method: 'notifications/resources/updated', params: {uri: 'test://idle'}}); res.end(); return;
        }
        nativeStreams.add(res); res.on('close', () => nativeStreams.delete(res)); return;
      }
      if (req.method === 'DELETE') {
        for (const stream of nativeStreams) stream.end();
        if (mode === 'native-delete-challenge') { challenge(401); return; }
        res.writeHead(204); res.end(); return;
      }
      const result = value => json(200, {jsonrpc: '2.0', id: rpc.id, result: value});
      if (rpc.method === 'initialize') {
        res.setHeader('mcp-session-id', 'native-auth-session');
        result({protocolVersion: '2025-11-25', serverInfo: {name: 'auth-peer', version: '1'}, capabilities: {tools: {}, resources: {}}}); return;
      }
      if (req.headers['mcp-session-id'] !== 'native-auth-session') { json(404, {}); return; }
      if (rpc.method === 'notifications/initialized') {
        if (mode === 'native-control-challenge') { challenge(401); return; }
        res.writeHead(202); res.end(); return;
      }
      if (rpc.method === 'tools/list') { result({tools: []}); return; }
      if (rpc.method === 'tools/call') {
        nativeCalls++;
        if (rpc.params.name === 'scope' && !scope.split(' ').includes('write')) { challenge(403); return; }
        if (rpc.params.name === 'scope') { result({content: []}); return; }
        res.writeHead(200, {'content-type': 'text/event-stream'});
        if (rpc.params.name === 'resume') { res.end(`id: resume-${rpc.id}\nretry: 1\ndata:\n\n`); return; }
        if (rpc.params.name === 'ping') {
          pending.set(`ping-${rpc.id}`, {res, id: rpc.id});
          send(res, {jsonrpc: '2.0', id: `ping-${rpc.id}`, method: 'ping'}); return;
        }
      }
      if (!rpc.method && pending.has(rpc.id)) {
        const held = pending.get(rpc.id); pending.delete(rpc.id);
        res.writeHead(202); res.end(); send(held.res, {jsonrpc: '2.0', id: held.id, result: {content: []}}); held.res.end(); return;
      }
      json(400, {}); return;
    }
    if (req.headers['x-require-write'] && !scope.split(' ').includes('write')) {
      res.writeHead(403, {'www-authenticate': `Bearer error="insufficient_scope", scope="read write", resource_metadata="${base}/metadata"`}); res.end(); return;
    }
    json(200, {authenticated: true, method: req.method}); return;
  }
  if (url.pathname === '/metadata' || url.pathname === '/.well-known/oauth-protected-resource/public/mcp' || url.pathname === '/.well-known/oauth-protected-resource') {
    if (mode === 'fallback' && url.pathname.endsWith('/public/mcp')) { json(404, {}); return; }
    if (mode === 'metadata-redirect') { res.writeHead(302, {location: `${base}/trap`}); res.end(); return; }
    if (mode === 'large') { json(200, {padding: 'x'.repeat(90000)}); return; }
    if (mode === 'slow') { setTimeout(() => json(200, metadata()), 500); return; }
    if (mode === 'slow-body') {
      res.writeHead(200, {'content-type': 'application/json'}); res.write('{');
      setTimeout(() => res.end(JSON.stringify(metadata()).slice(1)), 500); return;
    }
    if (mode === 'resource-mismatch') { json(200, {...metadata(), resource: `${base}/elsewhere`}); return; }
    if (mode === 'duplicate-json') { res.writeHead(200, {'content-type': 'application/json'}); res.end(`{"resource":"${resource()}","resource":"${resource()}","authorization_servers":["${issuer()}"]}`); return; }
    json(200, metadata()); return;
  }
  if (url.pathname.startsWith('/.well-known/') || url.pathname === '/tenant/.well-known/openid-configuration') {
    const expected = mode === 'oidc-insert' ? '/.well-known/openid-configuration/tenant'
      : mode === 'oidc-append' ? '/tenant/.well-known/openid-configuration'
      : '/.well-known/oauth-authorization-server/tenant';
    if (url.pathname !== expected) { json(404, {}); return; }
    json(200, asMetadata()); return;
  }
  if (url.pathname === '/authorize') {
    const p = url.searchParams;
    if (p.get('client_id') !== 'registered-client' || p.get('response_type') !== 'code' || p.get('resource') !== resource()
      || p.get('redirect_uri') !== 'http://127.0.0.1:32123/callback' || p.get('code_challenge_method') !== 'S256'
      || !/^[A-Za-z0-9_-]{43}$/.test(p.get('code_challenge')) || !p.get('state')) { reject('invalid_request'); return; }
    const code = `code-${codes.size}`;
    codes.set(code, {challenge: p.get('code_challenge'), redirect: p.get('redirect_uri'), scope: p.get('scope') ?? ''});
    const callback = new URL(p.get('redirect_uri'));
    callback.searchParams.set('code', code); callback.searchParams.set('state', p.get('state'));
    callback.searchParams.set('iss', issuer()); callback.searchParams.set('resource', resource());
    res.writeHead(302, {location: callback.href}); res.end(); return;
  }
  if (url.pathname === '/token') {
    if (req.method !== 'POST' || req.headers['content-type'] !== 'application/x-www-form-urlencoded') { reject('invalid_request'); return; }
    if (mode === 'token-redirect') { res.writeHead(307, {location: `${base}/trap`}); res.end(); return; }
    const p = new URLSearchParams(body);
    if (p.get('resource') !== resource()) { reject('invalid_target'); return; }
    if (authMethod === 'none' && p.get('client_id') !== 'registered-client') { reject('invalid_client'); return; }
    if (authMethod === 'client_secret_post' && (p.get('client_id') !== 'registered-client' || p.get('client_secret') !== 'client-secret: +')) { reject('invalid_client'); return; }
    if (authMethod === 'client_secret_basic' && Buffer.from(req.headers.authorization?.replace(/^Basic /, '') ?? '', 'base64').toString() !== 'registered-client:client-secret%3A+%2B') { reject('invalid_client'); return; }
    let scope;
    if (p.get('grant_type') === 'authorization_code') {
      const saved = codes.get(p.get('code'));
      if (!saved || p.get('redirect_uri') !== saved.redirect || !/^[A-Za-z0-9_-]{43}$/.test(p.get('code_verifier') ?? '')) { reject('invalid_grant'); return; }
      const computed = createHash('sha256').update(p.get('code_verifier')).digest('base64url');
      if (!equal(computed, saved.challenge)) { reject('invalid_grant'); return; }
      codes.delete(p.get('code')); scope = saved.scope;
    } else if (p.get('grant_type') === 'refresh_token') {
      if (mode === 'refresh-rejected' || !refreshes.has(p.get('refresh_token'))) { reject('invalid_grant'); return; }
      scope = refreshes.get(p.get('refresh_token')); refreshes.delete(p.get('refresh_token'));
    } else { reject('unsupported_grant_type'); return; }
    serial++;
    const access = `private-access-${serial}`; const refresh = `private-refresh-${serial}`;
    issued.set(access, scope); refreshes.set(refresh, scope);
    if (mode === 'token-disconnect' || (mode === 'refresh-disconnect' && p.get('grant_type') === 'refresh_token')) { req.socket.destroy(); return; }
    json(200, {access_token: access, token_type: 'Bearer', refresh_token: refresh,
      expires_in: mode === 'expired' ? 0 : 3600, scope}); return;
  }
  json(404, {});
});
server.listen(0, '127.0.0.1', () => {
  base = `http://127.0.0.1:${server.address().port}`;
  process.stdout.write(`${server.address().port}\n`);
});
