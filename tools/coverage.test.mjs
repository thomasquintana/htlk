import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { summarize, check } from './coverage.mjs';

const names = ['htlk-analyzer', 'htlk-compiler', 'htlk-executable', 'htlk-runtime', 'htlk'];
function report() {
  return {
    type: 'llvm.coverage.json.export',
    data: [{ files: names.map(name => ({
      filename: name === 'htlk' ? 'src/lib.rs' : `crates/${name}/src/lib.rs`,
      summary: Object.fromEntries(['lines', 'functions', 'regions', 'branches'].map(metric =>
        [metric, { count: metric === 'branches' ? 0 : 100, covered: metric === 'branches' ? 0 : 99 }])),
    })) }],
  };
}
const thresholds = Object.fromEntries(names.map(name => [name, { lines: 99, max_uncovered_lines: 1 }]));

test('each crate must meet its own exact threshold and uncovered-line allowance', () => {
  const data = report();
  assert.deepEqual(check(summarize(data), thresholds), []);
  data.data[0].files[0].summary.lines.covered = 98;
  const failures = check(summarize(data), thresholds);
  assert.equal(failures.length, 2);
  assert.match(failures[0], /htlk-analyzer/);
  assert.match(failures[1], /2 uncovered lines/);
});

test('a large well-covered crate cannot mask another crate falling below its floor', () => {
  const data = report();
  data.data[0].files[0].summary.lines = { count: 1_000_000, covered: 1_000_000 };
  data.data[0].files[1].summary.lines = { count: 100, covered: 98 };
  assert.match(check(summarize(data), thresholds).join('\n'), /htlk-compiler/);
});

test('missing, empty, duplicated and malformed measurements fail closed', () => {
  assert.throws(() => summarize({}), /LLVM/);
  assert.throws(() => summarize({ type: 'llvm.coverage.json.export', data: [] }), /Missing coverage/);
  const missing = report();
  missing.data[0].files.splice(3, 1);
  assert.throws(() => summarize(missing), /htlk-runtime/);
  const duplicate = report();
  duplicate.data[0].files.push(duplicate.data[0].files[0]);
  assert.throws(() => summarize(duplicate), /Duplicate/);
  for (const invalid of [{ count: 0, covered: 0 }, { count: 10, covered: 11 }, { count: -1, covered: 0 }, { count: 10, covered: 1.5 }, { count: 10 }]) {
    const data = report();
    data.data[0].files[0].summary.lines = invalid;
    assert.throws(() => summarize(data), /Invalid|Missing/);
  }
});

test('test, example, dependency and unrelated source paths do not inflate production totals', () => {
  const data = report();
  for (const filename of ['tests/deep_e2e.rs', 'crates/htlk-runtime/tests/records.rs', 'examples/main.rs', '/registry/dependency/src/lib.rs', '../outside.rs']) {
    data.data[0].files.push({ filename, summary: { lines: { count: 10000, covered: 10000 } } });
  }
  assert.equal(summarize(data)['htlk-runtime'].lines.count, 100);
});

test('threshold configuration is complete and finite', () => {
  const totals = summarize(report());
  assert.throws(() => check(totals, {}), /threshold/);
  for (const lines of [-1, 101, NaN, Infinity, '99']) {
    assert.throws(() => check(totals, { ...thresholds, 'htlk-analyzer': { lines } }), /threshold/);
  }
  for (const allowance of [-1, 0.5, '1']) {
    assert.throws(() => check(totals, { ...thresholds, 'htlk-analyzer': { lines: 99, max_uncovered_lines: allowance } }), /allowance/);
  }
});

test('branch gates reject absent instrumentation and enforce each crate independently', () => {
  const data = report();
  const floors = Object.fromEntries(names.map(name => [name, { lines: 99, branches: 90 }]));
  assert.equal(check(summarize(data), floors).length, 5);
  for (const file of data.data[0].files) file.summary.branches = { count: 10, covered: 9 };
  assert.deepEqual(check(summarize(data), floors), []);
  data.data[0].files[2].summary.branches.covered = 8;
  assert.match(check(summarize(data), floors).join('\n'), /htlk-executable: 80.00% branches/);
  assert.throws(() => check(summarize(data), { ...floors, 'htlk-runtime': { lines: 99, branches: NaN } }), /branch threshold/);
});

test('rounded display percentages never weaken the actual coverage floor', () => {
  const data = report();
  data.data[0].files[0].summary.lines = { count: 100000, covered: 98999 };
  assert.match(check(summarize(data), thresholds).join('\n'), /lines < 99%/);
});

test('a new production crate requires explicit coverage policy', () => {
  for (const name of ['htlk-new', 'custom']) {
    const data = report();
    data.data[0].files.push({ filename: `crates/${name}/src/lib.rs` });
    assert.throws(() => summarize(data), /Unconfigured production crate/);
  }
});

test('the reexport-only facade is n/a until it contains executable code', () => {
  const data = report();
  data.data[0].files.pop();
  const totals = summarize(data);
  assert.deepEqual(totals.htlk.lines, { count: 0, covered: 0 });
  assert.deepEqual(check(totals, { ...thresholds, htlk: { lines: 100, max_uncovered_lines: 0 } }), []);
  data.data[0].files.push({ filename: 'src/lib.rs', summary: Object.fromEntries(['lines', 'functions', 'regions', 'branches'].map(metric => [metric, { count: 1, covered: 0 }])) });
  assert.match(check(summarize(data), { ...thresholds, htlk: { lines: 100, max_uncovered_lines: 0 } }).join('\n'), /htlk: 0.00%/);
});

test('the CLI returns failing status for broken reports, gates and arguments', () => {
  const directory = mkdtempSync(join(tmpdir(), 'htlk-coverage-'));
  try {
    const input = join(directory, 'coverage.json');
    const policy = join(directory, 'thresholds.json');
    writeFileSync(input, JSON.stringify(report()));
    writeFileSync(policy, JSON.stringify(thresholds));
    const command = fileURLToPath(new URL('./coverage.mjs', import.meta.url));
    const run = args => spawnSync(process.execPath, [command, ...args], { encoding: 'utf8' });
    const accepted = run([input, '--check', policy]);
    assert.equal(accepted.status, 0, accepted.stderr);
    assert.match(accepted.stdout, /\| Crate \| Lines/);
    for (const args of [[], [input, '--check'], [input, '--unknown', 'value']]) assert.notEqual(run(args).status, 0);
    writeFileSync(input, JSON.stringify({ type: 'llvm.coverage.json.export', data: [] }));
    assert.notEqual(run([input, '--check', policy]).status, 0);
    const low = report();
    low.data[0].files[0].summary.lines.covered = 1;
    writeFileSync(input, JSON.stringify(low));
    assert.notEqual(run([input, '--check', policy]).status, 0);
    writeFileSync(input, '{');
    assert.notEqual(run([input]).status, 0);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
