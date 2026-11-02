// Local Markdown file/heading validation. External URLs are deliberately not fetched.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export function inspectMarkdown(text) {
  const visible = [];
  let fence;
  for (const line of text.split(/\r?\n/)) {
    const marker = line.match(/^\s{0,3}(`{3,}|~{3,})/);
    if (marker) {
      if (!fence) fence = marker[1];
      else if (marker[1][0] === fence[0] && marker[1].length >= fence.length
        && line.slice(marker[0].length).trim() === "") fence = undefined;
      visible.push("");
    } else visible.push(fence ? "" : line);
  }
  assert.equal(fence, undefined, "Unclosed Markdown fence");
  const prose = visible.join("\n").replace(/<!--[\s\S]*?-->/g, "");
  const anchors = new Set();
  const slugs = new Map();
  for (const match of prose.matchAll(/^ {0,3}#{1,6}\s+(.+?)(?:\s+#+)?\s*$/gm)) {
    const title = match[1].replace(/<[^>]*>/g, "")
      .replace(/!?\[([^\]]+)\]\([^)]+\)/g, "$1")
      .replace(/&amp;/g, "&").toLowerCase()
      .replace(/[^\p{L}\p{N}\p{M}_\- ]/gu, "").replace(/ /g, "-");
    let slug = title;
    let suffix = slugs.get(title) ?? 0;
    while (anchors.has(slug)) slug = title + "-" + ++suffix;
    slugs.set(title, suffix);
    anchors.add(slug);
  }
  for (const match of prose.matchAll(/<(?:a|span)\s+[^>]*(?:id|name)=["']([^"']+)["'][^>]*>/g)) {
    anchors.add(match[1]);
  }
  const links = [];
  const withoutCode = prose.replace(/`+[^`\n]*`+/g, "");
  for (const match of withoutCode.matchAll(/\[[^\]\n]*\]\(<?([^\s)>]+)>?(?:\s+["'][^\n]*?["'])?\)/g)) {
    links.push(match[1]);
  }
  for (const match of withoutCode.matchAll(/^ {0,3}\[[^\]]+\]:\s*<?([^\s>]+)>?/gm)) {
    links.push(match[1]);
  }
  return { anchors, links };
}

export function validateLinks(documents, exists) {
  let count = 0;
  for (const [file, document] of documents) {
    for (const link of document.links) {
      if (/^[a-z][a-z0-9+.-]*:|^\/\//i.test(link)) continue;
      const hash = link.indexOf("#");
      const target = hash < 0 ? link : link.slice(0, hash);
      const fragment = hash < 0 ? "" : decodeURIComponent(link.slice(hash + 1));
      const resolved = target ? path.resolve(path.dirname(file), decodeURIComponent(target)) : file;
      assert(exists(resolved), `${file}: missing local target ${link}`);
      if (fragment && path.extname(resolved) === ".md") {
        const destination = documents.get(resolved);
        assert(destination, `${file}: Markdown target outside documentation inventory: ${link}`);
        assert(destination.anchors.has(fragment), `${file}: missing heading anchor ${link}`);
      }
      count++;
    }
  }
  return count;
}

function main() {
  const root = fileURLToPath(new URL("../", import.meta.url));
  const files = execFileSync("git", ["ls-files", "-z", "--cached", "--others",
    "--exclude-standard", "--", "*.md"], { cwd: root, encoding: "utf8" })
    .split("\0").filter(Boolean).map(file => path.join(root, file)).filter(existsSync);
  const documents = new Map([...new Set(files)].map(file =>
    [file, inspectMarkdown(readFileSync(file, "utf8"))]));
  const links = validateLinks(documents, existsSync);
  console.log(JSON.stringify({ markdown_documents: documents.size, local_links_checked: links }, null, 2));
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
