import assert from "node:assert/strict";
import test from "node:test";
import { inspectMarkdown, validateLinks } from "./validate-docs.mjs";

test("GitHub-style anchors preserve Unicode and disambiguate collisions", () => {
  const { anchors } = inspectMarkdown("# Café: `inputs.value`\n## Repeated\n## Repeated\n## Repeated-1\n## Repeated");
  assert.deepEqual([...anchors], ["café-inputsvalue", "repeated", "repeated-1", "repeated-1-1", "repeated-2"]);
});

test("fenced examples and inline code cannot invent documentation links or headings", () => {
  const parsed = inspectMarkdown("# Real\n````md\n# Fake\n```\n[bad](missing.md)\n````\n`[bad](inline.md)`\n[good](other.md#ok)");
  assert.deepEqual([...parsed.anchors], ["real"]);
  assert.deepEqual(parsed.links, ["other.md#ok"]);
  assert.throws(() => inspectMarkdown("~~~\nnot closed"), /Unclosed/);
});

test("relative paths, encoded filenames, references, and same-page anchors resolve", () => {
  const docs = new Map([
    ["/repo/docs/a.md", inspectMarkdown("# Here\n[self](#here)\n[next](../a%20b.md#target)\n[ref]: ../a%20b.md#target")],
    ["/repo/a b.md", inspectMarkdown("# Target")],
  ]);
  assert.equal(validateLinks(docs, file => docs.has(file)), 3);
});

test("missing files and stale anchors fail independently", () => {
  const destination = ["/repo/b.md", inspectMarkdown("# Correct")];
  assert.throws(() => validateLinks(new Map([
    ["/repo/a.md", inspectMarkdown("[bad](absent.md)")], destination,
  ]), file => file === "/repo/b.md"), /missing local target/);
  assert.throws(() => validateLinks(new Map([
    ["/repo/a.md", inspectMarkdown("[bad](b.md#stale)")], destination,
  ]), () => true), /missing heading anchor/);
});

test("external schemes are skipped, assets need existence, and explicit anchors work", () => {
  const docs = new Map([["/repo/a.md", inspectMarkdown(
    '<a id="manual"></a>\n[local](#manual)\n[asset](image.svg)\n[mail](mailto:a@example.test)\n[web](https://example.test/x#y)',
  )]]);
  assert.equal(validateLinks(docs, () => true), 2);
});
