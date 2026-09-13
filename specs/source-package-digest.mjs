// Fixture/document-check helper, not an HTLK compiler or general CBOR encoder.
// It supports exactly the text, array, and map values in source-package digests.
import { createHash } from "node:crypto";
import assert from "node:assert/strict";

function head(major, length) {
  if (length < 24) return Buffer.from([(major << 5) | length]);
  const width = length <= 0xff ? 1 : length <= 0xffff ? 2 : length <= 0xffffffff ? 4 : 8;
  const result = Buffer.alloc(1 + width);
  result[0] = (major << 5) | ({ 1: 24, 2: 25, 4: 26, 8: 27 })[width];
  if (width === 8) result.writeBigUInt64BE(BigInt(length), 1);
  else result.writeUIntBE(length, 1, width);
  return result;
}

export function encodeSourceValue(value) {
  if (typeof value === "string") {
    for (const scalar of value) {
      const point = scalar.codePointAt(0);
      assert(point < 0xd800 || point > 0xdfff, "Invalid Unicode scalar");
    }
    const bytes = Buffer.from(value, "utf8");
    return Buffer.concat([head(3, bytes.length), bytes]);
  }
  if (Array.isArray(value)) return Buffer.concat([head(4, value.length), ...value.map(encodeSourceValue)]);
  assert(value && Object.getPrototypeOf(value) === Object.prototype, "Expected a source-package map");
  const entries = Object.entries(value).map(([key, child]) => [encodeSourceValue(key), encodeSourceValue(child)]);
  entries.sort((a, b) => Buffer.compare(a[0], b[0]));
  return Buffer.concat([head(5, entries.length), ...entries.flat()]);
}

export function sourcePackageDigest(value) {
  const encoded = encodeSourceValue(["htlk.source_package", "0.1", value.manifest, value.sources]);
  return "sha256:" + createHash("sha256").update(encoded).digest("hex");
}
