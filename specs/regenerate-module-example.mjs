// Explicit fixture regeneration, not compiler filesystem discovery or a migration.
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { sourcePackageDigest } from "./source-package-digest.mjs";

assert(process.argv.length <= 3 && [undefined, "--write"].includes(process.argv[2]),
  "Usage: node specs/regenerate-module-example.mjs [--write]");
const root = new URL("./examples/modules/", import.meta.url);
function snapshot(directory, expectedPaths) {
  const manifest = JSON.parse(readFileSync(new URL(`${directory}/htlk_package.json`, root), "utf8"));
  assert.deepEqual(Object.values(manifest.modules).sort(), expectedPaths);
  const sources = Object.fromEntries(expectedPaths.map(path =>
    [path, readFileSync(new URL(`${directory}/${path}`, root), "utf8")]));
  return { manifest, sources };
}
const research = snapshot("research", ["src/search.htlk"]);
const writing = snapshot("writing", ["src/refine.htlk"]);
assert.deepEqual(research.manifest.dependencies, {});
assert.deepEqual(writing.manifest.dependencies, {});
const researchDigest = sourcePackageDigest(research);
const writingDigest = sourcePackageDigest(writing);
const app = snapshot("app", ["src/main.htlk", "src/prompts.htlk", "src/types.htlk"]);
assert.deepEqual(Object.keys(app.manifest.dependencies).sort(), ["research", "writing"]);
app.manifest.dependencies = { research: researchDigest, writing: writingDigest };
const appDigest = sourcePackageDigest(app);
const bundle = {
  format: "htlk_source_bundle",
  format_version: "0.1",
  entry: { package_digest: appDigest, module_path: "main" },
  packages: { [appDigest]: app, [researchDigest]: research, [writingDigest]: writing },
};
if (process.argv[2] === "--write") {
  writeFileSync(new URL("app/htlk_package.json", root), JSON.stringify(app.manifest, null, 2) + "\n");
  writeFileSync(new URL("source_bundle.json", root), JSON.stringify(bundle, null, 2) + "\n");
  console.log(`Regenerated the module example: ${appDigest}`);
} else {
  console.log(JSON.stringify(bundle, null, 2));
}
