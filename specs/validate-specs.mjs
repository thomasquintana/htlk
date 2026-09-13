#!/usr/bin/env node
// Documentation checks only: this is not the HTLK semantic compiler.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { sourcePackageDigest, encodeSourceValue } from "./source-package-digest.mjs";

const root = path.dirname(fileURLToPath(import.meta.url));
const files = ["htlk-grammar-spec.md", "htlk-ir-syntax-reference.md",
  "compiler-spec.md", "runtime-spec.md", "CHANGELOG.md",
  "htlk-ir-user-guide.md", "examples/README.md", "htlk-modules-spec.md",
  "examples/modules/README.md", "README.md"];
const docs = files.map(name => ({ name, text: fs.readFileSync(path.join(root, name), "utf8") }));
const grammarText = fs.readFileSync(path.join(root, "htlk-ir.ebnf"), "utf8");
const grammarBlock = docs[1].text.match(/^```ebnf\n([\s\S]*?)^```/m)?.[1];
assert.equal(grammarBlock, grammarText, "EBNF file differs from the syntax reference");

function grammarTokens(text) {
  const result = [];
  let pos = 0;
  while (pos < text.length) {
    const rest = text.slice(pos);
    const space = rest.match(/^\s+/);
    if (space) { pos += space[0].length; continue; }
    const quoted = rest.match(/^"(?:[^"\\]|\\.)*"/);
    if (quoted) {
      result.push({ kind: "literal", value: JSON.parse(quoted[0]) });
      pos += quoted[0].length;
      continue;
    }
    const word = rest.match(/^[A-Za-z_][A-Za-z0-9_]*/);
    if (word) { result.push({ kind: "name", value: word[0] }); pos += word[0].length; continue; }
    assert.match(rest[0], /[=;,|()[\]{}]/, "Unknown EBNF token");
    result.push({ kind: "punct", value: rest[0] });
    pos++;
  }
  return result;
}

const gt = grammarTokens(grammarText);
let gp = 0;
function take(value) {
  assert.equal(gt[gp]?.value, value, "Expected EBNF " + value);
  gp++;
}
function alternative(stops) {
  const items = [sequence(new Set([...stops, "|"]))];
  while (gt[gp]?.kind === "punct" && gt[gp]?.value === "|") {
    gp++;
    items.push(sequence(new Set([...stops, "|"])));
  }
  return items.length === 1 ? items[0] : { kind: "alt", items };
}
function sequence(stops) {
  const items = [];
  while (gp < gt.length && !(gt[gp].kind === "punct" && stops.has(gt[gp].value))) {
    if (gt[gp].kind === "punct" && gt[gp].value === ",") { gp++; continue; }
    const tok = gt[gp++];
    if (tok.kind === "literal") items.push({ kind: "literal", value: tok.value });
    else if (tok.kind === "name") items.push({ kind: "ref", value: tok.value });
    else if (tok.value === "(" || tok.value === "[" || tok.value === "{") {
      const close = { "(": ")", "[": "]", "{": "}" }[tok.value];
      const child = alternative(new Set([close]));
      take(close);
      items.push(tok.value === "(" ? child :
        { kind: tok.value === "[" ? "optional" : "repeat", child });
    } else throw Error("Unexpected EBNF token " + tok.value);
  }
  return { kind: "seq", items };
}
const rules = new Map();
while (gp < gt.length) {
  const name = gt[gp++];
  assert.equal(name.kind, "name");
  assert(!rules.has(name.value), "Duplicate grammar production " + name.value);
  take("=");
  rules.set(name.value, alternative(new Set([";"])));
  take(";");
}
const lexical = new Set(["identifier", "type_identifier", "integer", "float",
  "string", "triple_string", "regex", "EOF"]);
function checkReferences(ast) {
  if (ast.kind === "ref") assert(rules.has(ast.value) || lexical.has(ast.value),
    "Undefined EBNF nonterminal " + ast.value);
  ast.items?.forEach(checkReferences);
  if (ast.child) checkReferences(ast.child);
}
for (const ast of rules.values()) checkReferences(ast);

function sourceTokens(text) {
  assert(!text.startsWith("\ufeff"), "Source BOM is forbidden");
  for (const scalar of text) {
    const point = scalar.codePointAt(0);
    assert(point < 0xd800 || point > 0xdfff, "Invalid source Unicode scalar");
  }
  const tokens = [];
  let pos = 0;
  function emit(kind, value) { tokens.push({ kind, value, offset: pos }); pos += value.length; }
  while (pos < text.length) {
    const rest = text.slice(pos);
    const ws = rest.match(/^[ \t\r\n]+/);
    if (ws) { pos += ws[0].length; continue; }
    if (rest.startsWith("--[[")) {
      const end = rest.indexOf("]]", 4);
      assert(end >= 0, "Unterminated block comment");
      pos += end + 2; continue;
    }
    if (rest.startsWith("--")) {
      const end = rest.search(/[\r\n]/);
      pos += end < 0 ? rest.length : end; continue;
    }
    if (rest.startsWith('"""') || rest.startsWith('"')) {
      const triple = rest.startsWith('"""');
      const delimiter = triple ? '"""' : '"';
      let n = delimiter.length;
      while (n < rest.length && !rest.startsWith(delimiter, n)) {
        if (rest[n] === "\\") { n += 2; continue; }
        assert(triple || (rest[n] !== "\n" && rest[n] !== "\r"), "Line ending in string");
        n++;
      }
      assert(n < rest.length, "Unterminated string");
      const raw = rest.slice(0, n + delimiter.length);
      const body = raw.slice(delimiter.length, -delimiter.length);
      const jsonText = triple ? '"' + body.replace(/\\[\s\S]|["\n\r]/g,
        token => token.startsWith("\\") ? token : JSON.stringify(token).slice(1, -1)) + '"' : raw;
      const decoded = JSON.parse(jsonText);
      for (const scalar of decoded) {
        const point = scalar.codePointAt(0);
        assert(point < 0xd800 || point > 0xdfff, "Unpaired Unicode surrogate");
      }
      emit(triple ? "triple_string" : "string", raw);
      continue;
    }
    if (rest[0] === "/") {
      let n = 1;
      while (n < rest.length && rest[n] !== "/") {
        assert(rest[n] !== "\n" && rest[n] !== "\r", "Line ending in regex");
        if (rest[n] === "\\") {
          assert(rest[n + 1] !== "\n" && rest[n + 1] !== "\r", "Escaped line ending in regex");
        }
        n += rest[n] === "\\" ? 2 : 1;
      }
      assert(n < rest.length, "Unterminated regex");
      n++;
      const startFlags = n;
      while (/[A-Za-z]/.test(rest[n] ?? "") && n < rest.length) n++;
      const flags = rest.slice(startFlags, n);
      assert(/^[ims]*$/.test(flags) && new Set(flags).size === flags.length, "Invalid regex flags");
      emit("regex", rest.slice(0, n)); continue;
    }
    const compound = rest.match(/^(mcp\.(tool|resource|template|prompt)|predicates\.library)\b/);
    if (compound) { emit("word", compound[0]); continue; }
    const number = rest.match(/^-?(?:0|[1-9][0-9]*)(?:\.[0-9]+(?:[eE][+-]?[0-9]+)?|[eE][+-]?[0-9]+)/);
    if (number) { assert(Number.isFinite(Number(number[0])), "Nonfinite float"); emit("float", number[0]); continue; }
    const integer = rest.match(/^-?(?:0|[1-9][0-9]*)/);
    if (integer) {
      const value = BigInt(integer[0]);
      assert(value >= -(2n ** 63n) && value < 2n ** 63n, "Integer overflow");
      emit("integer", integer[0]); continue;
    }
    const word = rest.match(/^[A-Za-z_][A-Za-z0-9_]*/);
    if (word) {
      assert(/^[A-Z][A-Za-z0-9]*$/.test(word[0]) ||
        /^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$/.test(word[0]), "Invalid identifier spelling");
      emit(/^[A-Z]/.test(word[0]) ? "type_identifier" : "identifier", word[0]);
      continue;
    }
    const operator = rest.match(/^(==|!=|<=|>=|[=<>.,()[\]{}@&])/);
    assert(operator, "Unknown source token at " + pos + ": " + rest.slice(0, 30));
    emit("punct", operator[0]);
  }
  return tokens;
}

function makeParser(tokens) {
  const memo = new Map();
  let furthest = 0;
  function rule(name, pos) {
    furthest = Math.max(furthest, pos);
    if (lexical.has(name)) {
      if (name === "EOF") return new Set(pos === tokens.length ? [pos] : []);
      return new Set(tokens[pos]?.kind === name ? [pos + 1] : []);
    }
    const key = name + ":" + pos;
    if (memo.has(key)) return memo.get(key);
    memo.set(key, new Set());
    const result = match(rules.get(name), pos);
    memo.set(key, result);
    return result;
  }
  function match(ast, pos) {
    switch (ast.kind) {
      case "literal":
        return new Set(tokens[pos]?.value === ast.value ? [pos + 1] : []);
      case "ref": return rule(ast.value, pos);
      case "alt":
        return new Set(ast.items.flatMap(item => [...match(item, pos)]));
      case "seq": {
        let positions = new Set([pos]);
        for (const item of ast.items) {
          positions = new Set([...positions].flatMap(p => [...match(item, p)]));
          if (!positions.size) break;
        }
        return positions;
      }
      case "optional": return new Set([pos, ...match(ast.child, pos)]);
      case "repeat": {
        const reached = new Set([pos]);
        const work = [pos];
        while (work.length) {
          for (const next of match(ast.child, work.pop())) {
            if (!reached.has(next)) { reached.add(next); work.push(next); }
          }
        }
        return reached;
      }
      default: throw Error("Unknown grammar AST " + ast.kind);
    }
  }
  return { rule, get furthest() { return furthest; } };
}

function countEntryGraphs(parser, tokens) {
  let pos = [...parser.rule("version_decl", 0)][0];
  while (tokens[pos]?.value === "import" && parser.rule("import_decl", pos).size) {
    pos = Math.max(...parser.rule("import_decl", pos));
  }
  let count = 0;
  while (pos < tokens.length) {
    const ends = [...parser.rule("declaration", pos)].filter(end => end > pos);
    assert(ends.length, "Unparsed top-level declaration");
    if (tokens[pos].value === "graph") count++;
    pos = Math.max(...ends);
  }
  return count;
}

let examples = 0;
let completePrograms = 0;
let diagrams = 0;
let guideDiagrams = 0;
let moduleDiagrams = 0;
let moduleListings = 0;
let jsonExamples = 0;
for (const doc of docs) {
  const fenceLines = doc.text.match(/^```[^\n]*$/gm) ?? [];
  assert.equal(fenceLines.length % 2, 0, doc.name + ": unbalanced fences");
  if (files.slice(0, 4).includes(doc.name)) assert(doc.text.includes("**Status:** Draft 0.1"));
  for (const link of doc.text.matchAll(/\[[^\]]*\]\(([^)]+)\)/g)) {
    const target = link[1].split("#")[0];
    if (!target || /^[a-z]+:\/\//i.test(target)) continue;
    assert(fs.existsSync(path.resolve(root, path.dirname(doc.name), target)), doc.name + ": broken link " + target);
  }
  for (const block of doc.text.matchAll(/^```([^\n]*)\n([\s\S]*?)^```[ \t]*$/gm)) {
    const language = block[1].trim();
    if (language === "mermaid") {
      const guide = doc.name === "htlk-ir-user-guide.md";
      const moduleDoc = doc.name === "htlk-modules-spec.md";
      if (guide) guideDiagrams++; else if (moduleDoc) moduleDiagrams++; else diagrams++;
      const after = doc.text.slice(block.index + block[0].length);
      const caption = after.match(moduleDoc ?
        /^\s*\*\*Module diagram (\d+) — ([^\n]+)\*\* ([^\n]+)/ : guide ?
        /^\s*\*\*Guide diagram (\d+) — ([^\n]+)\*\* ([^\n]+)/ :
        /^\s*\*\*Diagram (\d+) — ([^\n]+)\*\* ([^\n]+)/);
      assert(caption, doc.name + ": missing diagram title/description");
      assert.equal(Number(caption[1]), moduleDoc ? moduleDiagrams : guide ? guideDiagrams : diagrams, doc.name + ": diagram numbering");
    }
    if (language === "json") {
      JSON.parse(block[2]);
      jsonExamples++;
    }
    if (language !== "htlk") continue;
    const tokens = sourceTokens(block[2]);
    const parser = makeParser(tokens);
    examples++;
    const complete = tokens[0]?.value === "ir_version";
    let accepted;
    if (complete) {
      const isModuleListing = doc.name === "htlk-modules-spec.md";
      if (isModuleListing) moduleListings++; else completePrograms++;
      assert.equal(JSON.parse(tokens[2].value), "0.1", "Wrong example IR version");
      accepted = parser.rule("document", 0).has(tokens.length);
      if (accepted && !isModuleListing) assert.equal(countEntryGraphs(parser, tokens), 1, "Complete example needs one graph");
      if (accepted && isModuleListing) assert(countEntryGraphs(parser, tokens) <= 1);
    } else {
      const reached = new Set([0]);
      const work = [0];
      while (work.length) {
        const p = work.pop();
        for (const rootRule of ["import_decl", "declaration", "scope_member", "loop_member", "edge"]) {
          for (const next of parser.rule(rootRule, p)) {
            if (!reached.has(next)) { reached.add(next); work.push(next); }
          }
        }
      }
      accepted = reached.has(tokens.length);
    }
    assert(accepted, doc.name + ": invalid HTLK example " + examples +
      " near token " + parser.furthest + " " +
      JSON.stringify(tokens[parser.furthest]?.value ?? "<eof>"));
  }
}

// The downloadable program must remain a complete, syntax-valid copy of the
// guide's worked-example declarations, not an independently drifting sample.
const guideText = docs.find(doc => doc.name === "htlk-ir-user-guide.md").text;
const guideBlocks = [...guideText.matchAll(/^```htlk\n([\s\S]*?)^```/gm)].map(m => m[1].trim());
const partPrefixes = ["type BriefOutcome", "prompt opportunities_query",
  "task research.find_evidence", "task brief.refine", "graph decision_brief"];
const exampleParts = partPrefixes.map(prefix => {
  const matches = guideBlocks.filter(block => block.startsWith(prefix));
  assert.equal(matches.length, 1, "Missing or duplicated worked-example part " + prefix);
  return matches[0];
});
const programText = fs.readFileSync(path.join(root, "examples/decision_brief.htlk"), "utf8");
const programTokens = sourceTokens(programText);
const programParser = makeParser(programTokens);
assert(programParser.rule("document", 0).has(programTokens.length), "Invalid complete example source");
assert.equal(countEntryGraphs(programParser, programTokens), 1);
const comparableTokens = text => sourceTokens(text).map(({ kind, value }) => ({ kind, value }));
assert.deepEqual(comparableTokens(programText),
  comparableTokens('ir_version = "0.1"\n' + exampleParts.join("\n")), "Guide and complete example differ");

const catalogFixture = JSON.parse(fs.readFileSync(path.join(root, "examples/decision_brief.catalogs.json"), "utf8"));
const inputFixture = JSON.parse(fs.readFileSync(path.join(root, "examples/decision_brief.inputs.json"), "utf8"));
for (const catalog of Object.values(catalogFixture)) {
  assert.equal(catalog.catalog_version, "0.1");
  assert.equal(catalog.mcp_protocol_version, "2025-11-25");
}
const fixtureTools = new Map(catalogFixture.tools.tools.map(entry => {
  assert(catalogFixture.tools.servers[entry.server], "Unknown fixture server");
  for (const key of ["inputSchema", "outputSchema"]) {
    const schema = entry.descriptor[key];
    assert.equal(schema.type, "object");
    assert.equal(schema.$schema, "https://json-schema.org/draft/2020-12/schema");
    assert.deepEqual([...schema.required].sort(), Object.keys(schema.properties).sort());
  }
  return [entry.server + "/" + entry.descriptor.name, entry];
}));
const calledTools = new Set();
for (const match of programText.matchAll(/mcp\.tool\("([^"]+)",\s*"([^"]+)"\)/g)) {
  const key = match[1] + "/" + match[2];
  assert(fixtureTools.has(key), "Missing fixture tool " + key);
  calledTools.add(key);
}
assert.equal(calledTools.size, 3);
assert.deepEqual(Object.keys(inputFixture), ["question"]);
assert.equal(inputFixture.question.kind, "inline");
assert.equal(typeof inputFixture.question.value, "string");
assert(inputFixture.question.value.length > 0);
const guideInputs = [...guideText.matchAll(/^```json\n([\s\S]*?)^```/gm)]
  .map(m => JSON.parse(m[1])).find(value => value.question?.kind === "inline");
assert.deepEqual(guideInputs, inputFixture, "Guide input example differs from fixture");

// Negative syntax checks ensure the grammar does not accidentally accept old forms.
for (const invalid of [
  'ir_version = "0.1" graph g { exports { x = n.outputs.value } }',
  'ir_version = "0.1" graph g { nodes { x = value(1) } }',
  'ir_version = "0.1" graph g { nodes { x = call(mcp.tool(inputs.server, "x")) } }',
  'ir_version = "0.1" graph g { preconditions = 1 < 2 < 3 }',
  'ir_version = "0.1" export graph g { inputs = {} outputs = {} nodes {} edges {} }',
  'ir_version = "0.1" export import other from "self/other"',
  'ir_version = "0.1" import other from inputs.path',
  'ir_version = "0.1" type T = string import other from "self/other"'
]) {
  const tokens = sourceTokens(invalid);
  assert(!makeParser(tokens).rule("document", 0).has(tokens.length), "Unexpected old/invalid syntax acceptance");
}

// Contract fields keep one expression grammar in every allowed location.
// Boolean types and at-most-once cardinality are semantic checks, not parser tests.
let contractSyntaxRegressions = 0;
for (const [rule, prefix] of [["scope", ""], ["node_options", ""], ["loop", "loop "]]) {
  const source = prefix + '{ preconditions = true and (false or true) postconditions = true or false and false }';
  const tokens = sourceTokens(source);
  assert(makeParser(tokens).rule(rule, 0).has(tokens.length), "Contract syntax rejected at " + rule);
  contractSyntaxRegressions++;
  for (const field of ["requires", "ensures", "pre_conditions", "post_conditions", "pre-conditions", "post-conditions"]) {
    let accepted = false;
    try {
      const invalid = sourceTokens(prefix + '{ ' + field + ' = true }');
      accepted = makeParser(invalid).rule(rule, 0).has(invalid.length);
    } catch { /* A lexical rejection also rejects the old/alternate spelling. */ }
    assert(!accepted, "Alternate contract field accepted: " + field + " at " + rule);
    contractSyntaxRegressions++;
  }
  for (const field of ["preconditions", "postconditions"]) {
    const invalid = sourceTokens(prefix + '{ ' + field + ' { true } }');
    assert(!makeParser(invalid).rule(rule, 0).has(invalid.length), "Contract block unexpectedly accepted");
    contractSyntaxRegressions++;
  }
}
assert(rules.has("preconditions") && rules.has("postconditions"));
assert(!rules.has("requires") && !rules.has("ensures"));

// Lexical regressions found during the second review. These are independent
// of semantic checks such as duplicate members, type checking and reachability.
const invalidLexemes = [
  "bad__name", "bad_name_", "_bad_name", "badName", "Bad_Type",
  "\ufeffir_version", "\u00a0graph", String.raw`"\uD800"`, String.raw`"\q"`,
  "9223372036854775808", "-9223372036854775809", "1e999",
  "/x/ii", "/x/g", "/line\nend/", "/line" + "\\" + "\nend/"
];
for (const input of invalidLexemes) assert.throws(() => sourceTokens(input),
  undefined, "Unexpected invalid lexeme acceptance: " + JSON.stringify(input));
const validLexemes = [
  "valid_name", "ValidType", "-9223372036854775808", "9223372036854775807",
  String.raw`"\uD83D\uDE00"`, String.raw`/a\/b/mi`, '"""line\nend"""'
];
for (const input of validLexemes) assert.equal(sourceTokens(input).length, 1);
assert.equal(sourceTokens("-- comment\rgraph")[0].value, "graph");
const contextual = sourceTokens('ir_version = "0.1" graph graph { inputs = {} outputs = {} nodes { graph = eval(string, "x") } edges {} }');
const contextualParser = makeParser(contextual);
assert(contextualParser.rule("document", 0).has(contextual.length));
assert.equal(countEntryGraphs(contextualParser, contextual), 1);

// Module fixture consistency. This checks this fixture, not general HTLK semantics.
const moduleRoot = path.join(root, "examples/modules");
const bundle = JSON.parse(fs.readFileSync(path.join(moduleRoot, "source_bundle.json"), "utf8"));
assert.equal(bundle.format, "htlk_source_bundle");
assert.equal(bundle.format_version, "0.1");
const packageDirectories = new Map([
  ["htlk_examples.decision_brief", "app"],
  ["htlk_examples.research", "research"],
  ["htlk_examples.writing", "writing"]
]);
const moduleRecords = new Map();
const id = "[a-z][a-z0-9]*(?:_[a-z0-9]+)*";
const modulePathPattern = new RegExp("^" + id + "(?:/" + id + ")*$");
const sourcePathPattern = new RegExp("^(?:" + id + "/)*" + id + "\\.htlk$");
function exactKeys(value, names) {
  assert.deepEqual(Object.keys(value).sort(), [...names].sort());
}
exactKeys(bundle, ["format", "format_version", "entry", "packages"]);
exactKeys(bundle.entry, ["package_digest", "module_path"]);
for (const [digest, pkg] of Object.entries(bundle.packages)) {
  assert.equal(sourcePackageDigest(pkg), digest, "Source snapshot digest mismatch");
  exactKeys(pkg, ["manifest", "sources"]);
  const m = pkg.manifest;
  exactKeys(m, ["manifest_version", "package_name", "package_version", "modules", "dependencies"]);
  assert.equal(m.manifest_version, "0.1");
  assert.equal(m.package_version, "0.1");
  const directory = packageDirectories.get(m.package_name);
  assert(directory, "Unexpected fixture package");
  assert.deepEqual(m, JSON.parse(fs.readFileSync(path.join(moduleRoot, directory, "htlk_package.json"), "utf8")));
  assert.deepEqual(Object.values(m.modules).sort(), Object.keys(pkg.sources).sort());
  assert.equal(new Set(Object.values(m.modules)).size, Object.keys(m.modules).length);
  for (const [alias, target] of Object.entries(m.dependencies)) {
    assert(new RegExp("^" + id + "$").test(alias) && alias !== "self");
    assert(bundle.packages[target], "Missing dependency snapshot");
  }
  for (const [name, sourcePath] of Object.entries(m.modules)) {
    assert(modulePathPattern.test(name) && sourcePathPattern.test(sourcePath));
    const text = pkg.sources[sourcePath];
    assert.equal(text, fs.readFileSync(path.join(moduleRoot, directory, sourcePath), "utf8"), "Snapshot source differs from disk");
    const tokens = sourceTokens(text), parser = makeParser(tokens);
    assert.equal(JSON.parse(tokens[2].value), "0.1");
    assert(parser.rule("document", 0).has(tokens.length), "Invalid module " + sourcePath);
    const entry = digest === bundle.entry.package_digest && name === bundle.entry.module_path;
    assert.equal(countEntryGraphs(parser, tokens), entry ? 1 : 0);
    const exports = new Set([...text.matchAll(/^export (?:type|task|prompt) ([A-Za-z0-9_.]+)/gm)].map(m => m[1]));
    moduleRecords.set(digest + "/" + name, { text, exports, imports: new Map(), directory, sourcePath });
  }
}
function visitDag(start, dependencies, visited = new Set(), active = new Set()) {
  assert(!active.has(start), "Fixture dependency cycle");
  if (visited.has(start)) return visited;
  active.add(start);
  for (const child of dependencies(start)) visitDag(child, dependencies, visited, active);
  active.delete(start); visited.add(start); return visited;
}
const reachedPackages = visitDag(bundle.entry.package_digest, key => Object.values(bundle.packages[key].manifest.dependencies));
assert.equal(reachedPackages.size, Object.keys(bundle.packages).length);
for (const [key, record] of moduleRecords) {
  const own = key.slice(0, 71);
  for (const match of record.text.matchAll(/^import ([a-z_]+) from "([^"]+)"/gm)) {
    const [, alias, specifier] = match;
    const slash = specifier.indexOf("/");
    const packageAlias = specifier.slice(0, slash), moduleName = specifier.slice(slash + 1);
    const targetPackage = packageAlias === "self" ? own : bundle.packages[own].manifest.dependencies[packageAlias];
    const target = targetPackage + "/" + moduleName;
    assert(moduleRecords.has(target), "Missing imported module");
    assert(!record.imports.has(alias), "Repeated alias");
    record.imports.set(alias, target);
    // Actual static imported references in this fixture all use alias.symbol.
    const refs = new RegExp("\\b" + alias + "\\.([A-Za-z][A-Za-z0-9_]*)", "g");
    for (const ref of record.text.matchAll(refs)) assert(moduleRecords.get(target).exports.has(ref[1]), "Private/missing fixture export");
  }
}
const moduleEntry = bundle.entry.package_digest + "/" + bundle.entry.module_path;
const reachedModules = visitDag(moduleEntry, key => [...moduleRecords.get(key).imports.values()]);
assert.equal(reachedModules.size, 5);
const moduleFile = relative => fs.readFileSync(path.join(moduleRoot, relative), "utf8");
const declarationsOnly = text => text.replace(/^ir_version = "0.1"\s*/, "").replace(/^import .*\n/gm, "").replace(/^export /gm, "");
const flattened = 'ir_version = "0.1"\n' + [
  declarationsOnly(moduleFile("app/src/types.htlk")),
  declarationsOnly(moduleFile("app/src/prompts.htlk")),
  declarationsOnly(moduleFile("research/src/search.htlk")).replace("task find_evidence", "task research.find_evidence"),
  declarationsOnly(moduleFile("writing/src/refine.htlk")).replace("task refine", "task brief.refine"),
  declarationsOnly(moduleFile("app/src/main.htlk")).replaceAll("shared.BriefOutcome", "BriefOutcome").replaceAll("&queries.", "&")
].join("\n");
assert.deepEqual(comparableTokens(flattened), comparableTokens(programText), "Modular example changed the worked program");
assert.equal(encodeSourceValue(["a", {}]).toString("hex"), "826161a0");
assert.equal(encodeSourceValue({ b: "two", a: "one" }).toString("hex"), "a26161636f6e6561626374776f");
const snapshot = bundle.packages[bundle.entry.package_digest];
const reordered = { sources: Object.fromEntries(Object.entries(snapshot.sources).reverse()), manifest: Object.fromEntries(Object.entries(snapshot.manifest).reverse()) };
assert.equal(sourcePackageDigest(snapshot), sourcePackageDigest(reordered));
const changedSnapshot = structuredClone(snapshot);
changedSnapshot.sources["src/main.htlk"] += "-- changed comment\n";
assert.notEqual(sourcePackageDigest(snapshot), sourcePackageDigest(changedSnapshot));
assert.throws(() => encodeSourceValue("\ud800"));

// Check CDDL reference closure. This is not a full CDDL parser or instance validator.
const cddl = fs.readFileSync(path.join(root, "htlk-executable.cddl"), "utf8")
  .replace(/;[^\n]*/g, "");
const declared = new Set([...cddl.matchAll(/^([a-z_][a-z0-9_]*)\s*=/gm)].map(m => m[1]));
const executableShape = cddl.match(/^executable = \{([^}]+)\}/m)?.[1];
assert(executableShape, "Missing executable envelope shape");
assert.match(executableShape, /format:\s*"htlk\.executable\.graph"/);
assert.match(executableShape, /\bversion:\s*"0\.1"/);
assert.deepEqual([...executableShape.matchAll(/^\s*([a-z_]+):/gm)].map(m => m[1]).sort(),
  ["fingerprint", "format", "payload", "version"], "Unexpected executable envelope fields");
assert.match(executableShape, /fingerprint:\s*digest/);
assert.match(executableShape, /payload:\s*bstr/);
assert(cddl.includes('ir_version: "0.1"'), "Canonical document version mismatch");
const compilerText = docs.find(doc => doc.name === "compiler-spec.md").text;
assert(compilerText.includes('format: "htlk.executable.graph",\n    version: "0.1"'),
  "Compiler envelope prose differs from CDDL");
assert(compilerText.includes('SHA256(UTF8("htlk.executable.graph/0.1\\n") || payload)'),
  "Compiler executable fingerprint formula mismatch");
for (const [payloadHex, expected] of [
  ["", "sha256:ebbad418b6ddd9ead246e32dc337b19276b2c709701d2ad69a9992098f9fa14c"],
  ["f6", "sha256:9b039893d4db25c42f0765d6a31877987d90ff86f07ca593af128ca23500120d"],
]) {
  const actual = "sha256:" + createHash("sha256")
    .update("htlk.executable.graph/0.1\n", "utf8")
    .update(Buffer.from(payloadHex, "hex")).digest("hex");
  assert.equal(actual, expected, "Executable fingerprint primitive vector mismatch");
  assert(compilerText.includes(expected), "Missing compiler fingerprint vector");
}
assert(cddl.includes('core_version: "0.1"'), "Evaluator core version mismatch");
for (const field of ["preconditions", "postconditions"]) {
  assert.equal([...cddl.matchAll(new RegExp("\\b" + field + ": expression", "g"))].length, 2,
    "Scope and node must both encode " + field);
}
assert(!/\b(?:requires|ensures)\s*:/.test(cddl), "Old canonical contract field remains");
for (const doc of docs) {
  for (const match of doc.text.matchAll(/\b(?:ir_version|format_version|manifest_version|interface_version|catalog_version|core_version|record_version)\s*(?:=|:)\s*"([^"]+)"/g)) {
    assert.equal(match[1], "0.1", doc.name + ": inconsistent HTLK format version");
  }
}
const words = [...cddl.matchAll(/"(?:[^"\\]|\\.)*"|[A-Za-z_][A-Za-z0-9_]*|=>|[.:=]/g)];
const cddlBuiltins = new Set(["tstr", "bstr", "bool", "null", "float"]);
for (let i = 0; i < words.length; i++) {
  const value = words[i][0];
  if (!/^[A-Za-z_]/.test(value) || words[i + 1]?.[0] === ":" || words[i - 1]?.[0] === ".") continue;
  assert(declared.has(value) || cddlBuiltins.has(value), "Undefined CDDL name " + value);
}
console.log(JSON.stringify({
  markdown_documents: docs.length,
  grammar_productions: rules.size,
  htlk_examples_parsed: examples,
  complete_programs_parsed: completePrograms,
  standalone_programs_parsed: 1,
  specification_diagrams: diagrams,
  user_guide_diagrams: guideDiagrams,
  module_diagrams: moduleDiagrams,
  module_listings_parsed: moduleListings,
  module_files_parsed: moduleRecords.size,
  source_package_digests_checked: Object.keys(bundle.packages).length,
  json_examples_parsed: jsonExamples,
  fixture_tool_references_checked: calledTools.size,
  cddl_rules_reference_checked: declared.size,
  executable_envelope_contract_checked: true,
  executable_fingerprint_vectors_checked: 2,
  contract_syntax_regressions: contractSyntaxRegressions,
  lexical_regressions: invalidLexemes.length + validLexemes.length + 2,
  note: "Syntax and document checks passed; runtime behavior and full CDDL instance validation are not performed."
}, null, 2));
