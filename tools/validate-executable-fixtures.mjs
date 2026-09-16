// Independent, deliberately small CBOR/JCS/hash checker for the fixed native fixture.
// This is not a replacement for the Rust verifier or a general CDDL implementation.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const root = new URL('../', import.meta.url);
const lines = readFileSync(new URL('crates/htlk-executable/tests/fixtures/native-empty.hex', root), 'utf8').trim().split(/\r?\n/);
const fingerprint = lines.shift().replace(/^fingerprint=/, '');
const scopeDigest = lines.shift().replace(/^root_scope=/, '');
const hex = lines.join('');
assert.match(hex, /^(?:[0-9a-f]{2})+$/);
const bytes = Buffer.from(hex, 'hex');
const sha = (...parts) => `sha256:${parts.reduce((hash, part) => hash.update(part), createHash('sha256')).digest('hex')}`;
const text = new TextDecoder('utf-8', { fatal: true });
function decode(bytes) {
  let offset = 0;
  function take(count) {
    assert(count <= bytes.length - offset, 'truncated CBOR');
    const value = bytes.subarray(offset, offset + count);
    offset += count;
    return value;
  }
  function item(depth = 0) {
    assert(depth <= 128, 'fixture nesting limit');
    const start = offset;
    const initial = take(1)[0], major = initial >> 5, info = initial & 31;
    let value;
    if (major === 7) {
      assert([20, 21, 22].includes(info), 'unsupported fixture simple value');
      value = info === 22 ? null : info === 21;
    } else {
      let argument = BigInt(info);
      if (info >= 24) {
        assert(info <= 27, 'indefinite/reserved CBOR');
        const width = 2 ** (info - 24);
        argument = 0n;
        for (const byte of take(width)) argument = (argument << 8n) | BigInt(byte);
        assert(argument >= [24n, 256n, 65536n, 4294967296n][info - 24], 'non-shortest CBOR argument');
      }
      if (major < 2) {
        assert(argument <= 9223372036854775807n, 'outside HTLK integer range');
        value = major === 0 ? argument : -1n - argument;
      } else {
        assert(argument <= BigInt(bytes.length), 'fixture length bound');
        const length = Number(argument);
        if (major === 2) value = take(length);
        else if (major === 3) value = text.decode(take(length));
        else if (major === 4) value = Array.from({ length }, () => item(depth + 1));
        else if (major === 5) {
          value = new Map(); let previous;
          for (let index = 0; index < length; index++) {
            const key = item(depth + 1);
            assert.equal(typeof key.value, 'string');
            if (previous) assert(Buffer.compare(previous, key.raw) < 0, 'map ordering/duplicate key');
            previous = key.raw;
            value.set(key.value, item(depth + 1));
          }
        } else assert.fail('tags are outside the fixture profile');
      }
    }
    return { value, raw: bytes.subarray(start, offset) };
  }
  const value = item(); assert.equal(offset, bytes.length, 'trailing CBOR'); return value;
}
const get = (node, key) => {
  assert(node.value instanceof Map); assert(node.value.has(key), `missing ${key}`); return node.value.get(key);
};
const envelope = decode(bytes);
assert.equal(envelope.value.size, 4);
assert.equal(get(envelope, 'format').value, 'htlk.executable.graph');
assert.equal(get(envelope, 'version').value, '0.1');
const payload = get(envelope, 'payload').value;
assert(Buffer.isBuffer(payload));
assert.equal(sha('htlk.executable.graph/0.1\n', payload), fingerprint);
assert.equal(get(envelope, 'fingerprint').value, fingerprint);
const document = decode(payload);
assert.equal(document.value.size, 10);
assert.equal(get(document, 'ir_version').value, '0.1');
assert.equal(get(document, 'graph_id').value, 'fixture.empty');
assert.equal(get(document, 'root_scope').value, scopeDigest);
const scopes = get(document, 'scopes'); assert.equal(scopes.value.size, 1);
const scope = get(scopes, scopeDigest);
assert.equal(sha('htlk.scope/0.1\n', scope.raw), scopeDigest);
assert.equal(scope.value.size, 8);
for (const key of ['inputs', 'outputs', 'carried', 'limits']) assert.equal(get(scope, key).value.size, 0);
for (const key of ['nodes', 'edges']) assert.equal(get(scope, key).value.length, 0);
for (const key of ['preconditions', 'postconditions']) assert.deepEqual(get(scope, key).value.map(node => node.value), ['literal', true]);
for (const key of ['bindings', 'libraries', 'templates', 'schema_uris']) assert.equal(get(document, key).value.size, 0);
const profile = get(document, 'profile');
assert.equal(get(profile, 'core_version').value, '0.1');
assert.equal(get(profile, 'mcp_protocol_version').value, '2025-11-25');
const documents = get(document, 'documents'); assert.equal(documents.value.size, 1);
const policyId = get(profile, 'policy_document').value;
const policyBytes = get(documents, policyId).value;
assert.equal(sha(policyBytes), policyId);
function jcs(value) {
  if (Array.isArray(value)) return `[${value.map(jcs).join(',')}]`;
  if (value !== null && typeof value === 'object') return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${jcs(value[key])}`).join(',')}}`;
  if (typeof value === 'number') assert(Number.isFinite(value) && (!Number.isInteger(value) || Number.isSafeInteger(value)));
  return JSON.stringify(value);
}
assert.equal(jcs(JSON.parse(text.decode(policyBytes))), text.decode(policyBytes));
function implementation(files) {
  const hash = createHash('sha256').update('htlk.native-implementation/0.1\n');
  for (const file of files) {
    const source = readFileSync(new URL(`crates/htlk-executable/src/${file}`, root));
    const length = Buffer.alloc(8); length.writeBigUInt64BE(BigInt(source.length));
    hash.update(length).update(source);
  }
  return `sha256:${hash.digest('hex')}`;
}
assert.equal(get(profile, 'core_digest').value, implementation([
  'native_profile.rs', 'evaluate.rs', 'checked_evaluate.rs', 'runtime_type.rs',
  'schema_projection.rs', 'schema_hints.rs', 'type_check.rs', 'native_registry.rs',
  'graph_verify.rs', 'verified.rs',
]));
assert.equal(get(get(profile, 'regex_engine'), 'implementation_digest').value, implementation(['native_profile.rs', 'evaluate.rs']));
assert.equal(get(get(profile, 'uri_template_engine'), 'implementation_digest').value, implementation(['native_profile.rs', 'uri_template.rs']));
assert.equal(get(get(profile, 'schema_validator'), 'implementation_digest').value, sha(readFileSync(new URL('crates/htlk-executable/src/native_schema.rs', root))));
console.log(JSON.stringify({ fixture: 'native-empty', bytes: bytes.length, payload_bytes: payload.length, fingerprint, scope: scopeDigest, independent_cbor_jcs_hash_checks: true }, null, 2));
