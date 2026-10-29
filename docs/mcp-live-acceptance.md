# MCP interoperability acceptance

## Actual Chrome DevTools campaign

The local campaign on 2026-10-29 passed against the unmodified sibling Chrome
DevTools MCP. HTLK compiled ordinary authored graphs from actual discovered
descriptors and executed through `NativeMcpAdapter`, with durable SQLite results.

| Component | Observed identity |
| --- | --- |
| HTLK implementation | `9989cad`, plus the acceptance example and discovery helper introduced with this report |
| Chrome DevTools MCP | clean `ada7963246c0c91eae185125a57aecd13cbe0376`, package/server `1.12.0` |
| DevTools submodule | `daedf816a02fb9def836e35037ea6a99ba5aad3f` |
| Built entry SHA-256 | `e8b0290a8fe9a1edc31d595ccc870df926bfaaafe19fb14cd4d8d47bf6194979` (`build/src/bin/chrome-devtools-mcp.js`; entry file, not a whole-build hash) |
| Server Node | `26.13.0` |
| Installed Google Chrome | `151.0.7922.72` |
| Negotiated protocol | `2025-11-25` |
| MCP server name | `chrome_devtools` |
| Discovery | 32 tools, all 32 without `outputSchema`; zero `tools/call` requests |

The sibling's supported `PUPPETEER_SKIP_DOWNLOAD=1 npm run sync` and `npm run
build` commands completed successfully. The server launch used `--headless`,
`--isolated`, the installed Chrome executable, `--no-page-id-routing`,
`--viewport=640x480`, `--no-usage-statistics`, and `--no-performance-crux`.
Update checks and usage statistics were disabled with the corresponding
`CHROME_DEVTOOLS_MCP_NO_*` environment switches. No credentials were needed.

### Measured results

The campaign made **21 tool calls**: 9 basic operations, then 4 each for Observe,
Enforce, and explicit revision adoption. Both inference operations made **zero
additional calls**. Each graph had a finite call budget and used a host policy
scoped to the controlled data URL, four exact authored script bodies, screenshot/
snapshot/list operations, test-owned page closure, and one deliberate invalid-page
closure. No retry was requested.

- Created a unique controlled page marker `HTLK_MCP_1793275318685610000`.
- Verified that marker through three `evaluate_script` results and an actual page
  snapshot in the same run-owned native browser session.
- All three samples arrived as **text-only protocol results without
  `structuredContent`**. Literal JSON was selected from a fence in content block
  0, UTF-8 byte span `[41,92)`. HTLK retained the original display text.
- The actual screenshot result retained both `text` and `image` blocks. It had no
  JSON candidate and remained a successful generic envelope.
- The deliberate invalid-page operation failed with `MCP_TOOL_ERROR`. Its received
  response remained protected inspection evidence, with conservative external-effect
  uncertainty and no successful tool-output artifact.
- Passive revision one inferred an object with string `marker` and integer `price`
  from the three identified accepted samples.
- A later actual script returned string `price` and added string `currency`.
  Observe published an envelope with an invalid inferred assessment. Enforce
  rejected the same shape with `MCP_INFERRED_SCHEMA`; an authored failure branch
  still performed cleanup.
- Explicitly sampling the accepted Observe result produced revision two: integer/
  string `price` and optional string `currency`. Explicit adoption and recompilation
  validated the changed value. The original descriptor was unchanged throughout.
- Every phase explicitly closed its test-owned tab and checked that the marker was
  absent from the remaining page list. The engine and native adapter shut down;
  a subsequent process check found no Google Chrome main process.
- Reopening the saved store with `--inspect` reconstructed sample origins and
  authorized error evidence **without an MCP adapter or provider calls**.

### Immutable evidence identities

| Phase | Run ID | Executable fingerprint |
| --- | --- | --- |
| Basic | `run:0167f5255cb46d69d26ed47bc3a6a090:1` | `sha256:021a9239e55ca0f38ef0bab18805f544e38da64bf214eecf767b2acce2cc81f7` |
| Observe drift | recorded in `observe-drift.run.json` | `sha256:22ae4d3c3bf9b72e9bf7edf8943ab1b55a9bdfaaf0e8bf2d950e407277eced1b` |
| Enforce drift | recorded in `enforce-drift.run.json` | `sha256:76025427141f9a8b246c508a68898414d7a3c3deeef872ff885f7be2130414fa` |
| Adopt revision | recorded in `adopt-revision.run.json` | `sha256:94716c85fba058d40d6110c8d7d2b11d73961f16e68c76c48529599351683071` |

Schema identities:

- Revision one: `sha256:25b39b481fac8536eca114a011a3cbf82e31abf0823cfbeec60df459f447a0ba`.
- Revision two: `sha256:d101c97153c8dfe79d69f06bd03412762002116570125b4faf7b25df943060aa`.

The actual server identity was
`sha256:3b01025a2789e43b76b05aec01d5703b48a2c7f7316affaa7b02e7a934d62f73`.
Basic result policy canonical-CBOR SHA-256:
`sha256:ecfe3b1f162949a11808f22c62046c655fdf5deb5e531b46f0a2a0ffe2c602b1`.
It used default extraction, declared Enforce, and no inferred/application schema.

| Actual tool | Unmodified descriptor digest |
| --- | --- |
| `new_page` | `sha256:debd9f89eb2a60c3599a626c76ab3117527a3737ca774853a89f3cf6d9acb6cd` |
| `evaluate_script` | `sha256:721fcc5c9dce3bc15ffdc845cd72a7482a117010dee4a5493e9cecceda7d587b` |
| `take_snapshot` | `sha256:4b42f537f63d01cd4dafd8144fe1c8ad47a1566977a1c1cef0144468cd645138` |
| `take_screenshot` | `sha256:69fa6f2d19d0ee5cb474bdbc940b5d43467ae8fbfc23123d0e8f7ca481ef8229` |
| `close_page` | `sha256:b043cf630cd768716ce1025573a406d71bc67c95421fb382332b4d8e790926bf` |
| `list_pages` | `sha256:5dcff5be9beee0def05f5dcd290d5e5d1038c23e5d9551b93f9799a56f66ad68` |

Sample artifact IDs:

1. `sha256:dd5239d0ec717dc81d42d96a8c055478fd8e8ec969b2c49b5a192b0264f05721`
2. `sha256:62185091b8df668e23eb9e86fc06c77a68f13be883ac6f99592030b24ec5339c`
3. `sha256:aa9845a668814211adfb2d66bee41dbe4402938e3b990d270197e6dfaf5ac50c`

Screenshot artifact:
`sha256:2f168c8b840072f732ac018daee35e16dc5e6db14a4585a81caab06f58004850`.
Error invocation:
`sha256:b85d8161c9aad865c2161d3461e16c0a79b2ae00d1ac947f6c48c6ff472b402e`;
attempt `sha256:6d9ac442857141a07a60d0c898d8d29d7ce9df6e50a8d5a49a76f8b2b05b1886`;
raw response `sha256:c302abf77e00003cb2b206fd4b5e6ba9ae2a1801da46b18be31a0d27eb11eaf5`.

Raw catalogs, rendered graphs, run manifests, inference evidence, schema diff,
screenshot and response data remain in the operator's ignored local campaign
directory. Public evidence here contains controlled markers and immutable IDs.

## Reproducing the opt-in campaign

Build the sibling at the recorded revision using its own instructions. Create a
private host JSON file with this shape, substituting actual absolute paths:

```json
{
  "transport": "stdio",
  "alias": "chrome",
  "deployment_id": "chrome-local-pinned",
  "program": "/absolute/path/to/node",
  "arguments": [
    "/absolute/path/to/chrome-devtools-mcp/build/src/bin/chrome-devtools-mcp.js",
    "--headless", "--isolated",
    "--executablePath=/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "--no-page-id-routing", "--viewport=640x480",
    "--no-usage-statistics", "--no-performance-crux"
  ],
  "environment": {
    "CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS": "1",
    "CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS": "1"
  }
}
```

Choose an existing private parent directory and new catalog/output paths:

```console
node tools/discover-mcp.mjs HOST.json CATALOGS.json
cargo run --example mcp_live_acceptance --locked -- HOST.json CATALOGS.json NEW_OUTPUT_DIR
cargo run --example mcp_live_acceptance --locked -- --inspect NEW_OUTPUT_DIR
```

Discovery initializes and lists only; it bounds pages, bytes and process lifetime.
The compiler strictly admits the captured catalog, and the native adapter independently
validates actual wire JSON and checks live identities/descriptors before dispatch.
Discovery refuses to overwrite its output. The runner requires a new output
directory, isolated/headless arguments and the actual Chrome catalog alias. Inspect
requires an existing store and prints only bounded evidence metadata.

The authored templates are `examples/live_chrome/basic.htlk` and `revision.htlk`;
the runner substitutes the linked content-library identity and exact script literals.
Graphs preserve normal authorization, labels, budgets, checks and failure handling.
They navigate only a test-owned data URL and do not inspect a personal profile.

## Independent adversarial wire acceptance

`tests/mcp_wire_results.rs` uses the independent Node peer in
`tests/support/mcp_peer.mjs` over stdio, Streamable HTTP JSON, and SSE. It verifies
schema-less literal prose JSON with escaped quotes/braces and UTF-8, fenced arrays,
scalars/null, ordinary prose, ambiguity, deduplication, duplicate-key rejection with
later valid JSON, structured-content preference, all supported media/resource kinds,
authoritative tool errors and malformed media rejection. Each transport makes exactly
12 calls with no retries/probes, checks immutable artifact/journal results, and retains
authorized failed-call evidence without success publication.

The existing deep E2E, extraction, protocol, inference, admission, process-kill and
recovery suites additionally exercise limits, descriptor drift, contract failures,
actual-value checks, protected sampling and restart without provider recall.
These independent fixtures passed separately from the actual Chrome campaign.

```console
node --test tools/discover-mcp.test.mjs
cargo test --locked --test mcp_wire_results --test deep_e2e
```

This report establishes the Chrome acceptance gate. Brave, llm-mcp, and a live
sandbox campaign were not needed or run for these results. Final workspace coverage
and release-quality gates are recorded separately in the validation handoff.
