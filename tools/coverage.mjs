#!/usr/bin/env node
/** Workspace coverage summaries and fail-closed per-crate coverage checks. */
import { readFileSync } from 'node:fs';
import { resolve, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const crates = ['htlk-analyzer', 'htlk-compiler', 'htlk-executable', 'htlk-runtime', 'htlk'];

export function summarize(report) {
  if (report.type !== 'llvm.coverage.json.export' || !Array.isArray(report.data)) {
    throw new Error('Expected an LLVM coverage JSON export');
  }
  const totals = Object.fromEntries(crates.map(name => [name, {
    lines: { count: 0, covered: 0 },
    functions: { count: 0, covered: 0 },
    regions: { count: 0, covered: 0 },
    branches: { count: 0, covered: 0 },
  }]));
  const seen = new Set();
  for (const data of report.data) {
    for (const file of data.files ?? []) {
      const path = relative(root, resolve(root, file.filename)).split(sep).join('/');
      const match = /^crates\/([^/]+)\/src\//.exec(path);
      const name = match?.[1] ?? (path.startsWith('src/') ? 'htlk' : undefined);
      if (!name) continue;
      if (!crates.includes(name)) throw new Error(`Unconfigured production crate: ${name}`);
      if (seen.has(path)) throw new Error(`Duplicate coverage file: ${path}`);
      seen.add(path);
      for (const metric of Object.keys(totals[name])) {
        const value = file.summary?.[metric];
        if (metric === 'branches' && !value) continue;
        if (!value || !Number.isSafeInteger(value.count) || !Number.isSafeInteger(value.covered)
            || value.count < 0 || value.covered < 0 || value.covered > value.count) {
          throw new Error(`Invalid ${metric} counts for ${path}`);
        }
        totals[name][metric].count += value.count;
        totals[name][metric].covered += value.covered;
      }
    }
  }
  for (const name of crates) {
    if (name !== 'htlk' && totals[name].lines.count === 0) throw new Error(`Missing coverage for ${name}`);
  }
  return totals;
}

export function check(totals, thresholds) {
  const failures = [];
  for (const name of crates) {
    const minimum = thresholds[name];
    if (!minimum || !Number.isFinite(minimum.lines) || minimum.lines < 0 || minimum.lines > 100) {
      throw new Error(`Missing or invalid line threshold for ${name}`);
    }
    const lines = totals[name].lines;
    const percent = lines.count ? 100 * lines.covered / lines.count : 100;
    if (percent + Number.EPSILON < minimum.lines) {
      failures.push(`${name}: ${percent.toFixed(2)}% lines < ${minimum.lines}% required`);
    }
    if (minimum.branches !== undefined) {
      if (!Number.isFinite(minimum.branches) || minimum.branches < 0 || minimum.branches > 100) {
        throw new Error(`Invalid branch threshold for ${name}`);
      }
      const branches = totals[name].branches;
      if (branches.count === 0 && lines.count !== 0) failures.push(`${name}: missing branch instrumentation`);
      else if (100 * branches.covered / branches.count < minimum.branches) {
        failures.push(`${name}: ${(100 * branches.covered / branches.count).toFixed(2)}% branches < ${minimum.branches}% required`);
      }
    }
    if (minimum.max_uncovered_lines !== undefined) {
      if (!Number.isSafeInteger(minimum.max_uncovered_lines) || minimum.max_uncovered_lines < 0) {
        throw new Error(`Invalid uncovered-line allowance for ${name}`);
      }
      if (lines.count - lines.covered > minimum.max_uncovered_lines) {
        failures.push(`${name}: ${lines.count - lines.covered} uncovered lines > ${minimum.max_uncovered_lines} allowed`);
      }
    }
  }
  return failures;
}

function main(args) {
  if (!args.length) throw new Error('Usage: node tools/coverage.mjs REPORT.json [--check THRESHOLDS.json] [--missing PATH]');
  const report = JSON.parse(readFileSync(args.shift(), 'utf8'));
  const totals = summarize(report);
  console.log('| Crate | Lines | Functions | Regions | Branches | Uncovered lines |');
  console.log('| --- | --- | --- | --- | --- | --- |');
  for (const [name, metrics] of Object.entries(totals)) {
    console.log(`| ${name} | ${Object.values(metrics).map(({ count, covered }) =>
      `${count ? (100 * covered / count).toFixed(2) + '%' : 'n/a'} (${covered}/${count})`).join(' | ')} | ${metrics.lines.count - metrics.lines.covered} |`);
  }
  while (args.length) {
    const option = args.shift();
    if (!args.length) throw new Error(`Missing argument for ${option}`);
    const value = args.shift();
    if (option === '--check') {
      const failures = check(totals, JSON.parse(readFileSync(value, 'utf8')));
      if (failures.length) throw new Error(failures.join('\n'));
    } else if (option === '--missing') {
      for (const data of report.data) {
        for (const file of data.files ?? []) {
          if (!file.filename.includes(value)) continue;
          const lines = new Set();
          const segments = file.segments ?? [];
          for (let i = 0; i < segments.length; i++) {
            const [line, , count, hasCount, , gap] = segments[i];
            if (!hasCount || count || gap) continue;
            const end = segments[i + 1]?.[0] ?? line;
            for (let n = line; n <= end; n++) lines.add(n);
          }
          console.log(`${relative(root, file.filename)}: ${[...lines].join(',')}`);
        }
      }
    } else throw new Error(`Unknown option: ${option}`);
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { main(process.argv.slice(2)); }
  catch (error) { console.error(error.message); process.exitCode = 1; }
}
