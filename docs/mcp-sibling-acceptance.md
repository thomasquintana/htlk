# Five-server MCP live acceptance

## Result

On 2026-10-29, **all 75 advertised tools were exercised through the actual HTLK
compiler, `NativeMcpAdapter` and durable runtime**. **73 tools have successful
normal-operation cases**. Brave Place Search failed its advertised output contract;
a normal Brave summary remained blocked because actual searches returned no
summary key. Its deliberate invalid-key call exercised the tool's error path.

| Server | Discovered tools | Successful normal tools | Primary transport | Additional transport |
| --- | ---: | ---: | --- | --- |
| Chrome DevTools | 32 | 32 | stdio | — |
| Brave Search | 8 | 6 | stdio | HTTP image/video and errors |
| llm-mcp | 4 | 4 | stdio | HTTP generation and error |
| context-mcp | 23 | 23 | HTTP | stdio identity, ingest, job, query and purge |
| MCP Sandbox | 8 | 8 | stdio | — |
| **Total** | **75** | **73** | | |

This is a measured integration sample. A successful operation means an accepted
normal result, with the assertions described below. It does not establish every
optional argument, exhaustive provider behavior or browser visual correctness.
Async context receipts alone do not establish successful processing. Failed and
interrupted attempts remain separate evidence. The earlier Chrome-specific
[extraction/inference campaign](mcp-live-acceptance.md) remains a distinct result.

## Source and build identities

HTLK started at clean `1a5403c`, including the user's `eb6ea55` execution-progress
fix. This campaign adds example/controller/test/report files; the production
compiler, analyzer, runtime, descriptors and executable fixtures were not edited.
All five sibling worktrees were clean again at final audit.
The verified harness/controllers and offline regressions were committed as
`9ba3880` (this results report is recorded separately).

| Server | Exact sibling revision | Observed package/server |
| --- | --- | --- |
| Chrome | `ada7963246c0c91eae185125a57aecd13cbe0376` | `1.12.0` |
| Brave | `af08e0d48e812a3966ea2211854db5f0e8eee32d` | `2.1.4` |
| llm | `a9b6d2fe3fed686dd2ca2fb67591613f0cf44cb8` | `0.1.0` |
| context | `7ecad1fe51de6d57a35f1c3792fa16e2b96ddcb3` | `0.1.0` |
| sandbox | `8c649a415ba7be6aa6e15a4dc51e56dc5990eae0` | `0.2.0-SNAPSHOT-8c649a4` |

Supported builds used Brave `npm ci --ignore-scripts` then `npm run build`, llm
`cargo +1.98.0 build --release --locked`, context `cargo build --workspace
--all-features --locked`, and sandbox `make build`/GoReleaser snapshot. Chrome's
supported sync/build is recorded in the earlier report. Node was `26.13.0` for
live server launches; local independent tests also ran with Node 24. Rust was
1.98.0, Go 1.27.3, GoReleaser 2.19.0 and Docker 29.7.2. Installed Chrome was
151.0.7922.72; the recording phase used ffmpeg 9.0.2_1.

SHA-256 of measured executables/entry files (JS entry hashes are not whole-build
hashes):

| Build output | SHA-256 |
| --- | --- |
| llm release binary | `798bef8f256bedc4adcdbe22830553c10d0291c0389cb805f26eb2d66e926e72` |
| context debug binary | `db3c28ba2e092eb0d6d30b88874f13d0c1d3a9359f048b5f7ec056b5da3e9bd1` |
| sandbox Darwin ARM64 binary | `e2a52d825b1a0c1c2bc20100a1d0c2e613076ebb0c71f2f9e5e2aaa12be024eb` |
| Brave `dist/index.js` | `fcf201a52a9989ee228f0461dc1f5cf8f27c9e398c56cb75e2c26b6c0a261f50` |
| Chrome built entry | `e8b0290a8fe9a1edc31d595ccc870df926bfaaafe19fb14cd4d8d47bf6194979` |

Context used isolated localhost PostgreSQL 16 and Typesense 29.1, MinIO image
`sha256:7f65b3f969303cb44164d557e947b88fe554d76394cbc935b3021cfe52a2b626`,
and a newly built media image from the current sibling Dockerfile:
`sha256:f23a08b8b85f8f40f9408eb5a6ec84ffb251ffa729f559d1420d648d36fc388d`.
Compose pins for PostgreSQL/Typesense are in `tools/live_context_env.py`. The
sandbox fixture pins Alpine in `tools/fixtures/live_sandbox.yml`.

## Execution and evidence boundary

`examples/live_session/` compiles an ordinary bounded loop from the complete actual
catalog. Host-selected phase subsets limit graph size without modifying any tool
descriptor. Every command grants exactly one tool name and argument JSON digest
for the pinned deployment. The graph has fixed tool nodes, authored failure
branches, finite call/time/evaluation limits and no automatic operation retries.

Discovery initializes and lists tools, making zero `tools/call` requests. The
negotiated protocol was `2025-11-25`. Chrome's 32 tools and five Brave tools lack
`outputSchema`; they still return the uniform envelope. Declared contracts retain
default Enforce. No upstream schema was relaxed to make this campaign pass.

The private campaign is `.opencode/.tmp/live/siblings-20261029` (mode 0700).
Each session preserves the exact catalog, authored graph, executable CBOR, SQLite
history, session manifest, per-step metadata, normalized result and protected
raw/error evidence. Environment/header secrets are host-only references. A second
principal's inspection is denied. Trusted wait-metadata lookup locates one exact
wait before authorized node inspection, avoiding repeated whole-history polling.

`mcp_live_session --inspect OUTPUT_DIR` has no MCP adapter. It authorizes durable
artifact/evidence reads, compares exported results and raw bytes with the durable
records, and rejects tampering without overwriting the evidence. Completed
sessions were reopened offline with zero provider calls. Interrupted sessions are
not represented as successful offline completions.

## Per-tool normal-operation matrix

### MCP Sandbox — 8/8

Evidence: `sandbox-one` (15 calls). The fixture is non-root, read-only cwd,
network-disabled, bounded to one CPU/1 GiB/64 PIDs, with three owned containers.

| Tool | Observed case |
| --- | --- |
| `profile_list` | Operator's bounded acceptance profile present |
| `sandbox_list` | Fresh provider empty; empty again after destruction |
| `sandbox_create` | Three actual `docker:` IDs; unique owner labels independently inspected |
| `sandbox_info` | Returned ID and working directory match creation |
| `sandbox_stats` | Stats identify the owned container |
| `sandbox_exec` | Actual stdout/stderr markers, retained `/tmp` state, and exit code 7 |
| `sandbox_destroy` | Physical container absent; repeated destruction accepted |
| `sandbox_destroy_all` | Exactly two owned survivors destroyed; physical absence checked |

Root execution and a nonexistent profile returned tool errors. The first operator
fixture used fractional `memory_gb`, which the profile builder rounded to zero;
the private fixture was corrected to an integer and the failed attempt retained.

**Restart finding:** `sandbox-loss-three` created an owned container, then shut
down the actual native provider session. A fresh provider listed no sessions and
could not look up the ID, but `sandbox_destroy` claimed `destroyed:1` while the
physical container survived. The harness verified the exact container's unique
label and removed it. This measures controlled shutdown/restart, not an in-flight
SIGKILL. Earlier PID-identification attempts were abandoned and cleaned separately.

### llm-mcp — 4/4

Evidence: `llm-stdio`, with representative `llm-http-two` SSE/HTTP generation.
These were actual OpenAI API-key endpoints, using small synthetic inputs.

| Tool | Model | Independent assertion |
| --- | --- | --- |
| `generate` | `gpt-4.1-mini` | Returned controlled ORCHID marker; `store:false`, max output 32 |
| `classify` | `gpt-6-luna` | Named red-square predicate ≥0.8 and left-side choice |
| `embed` | `text-embedding-3-small` | Two correctly indexed, finite, nonzero 64-dimensional vectors |
| `transcribe` | `gpt-4o-mini-transcribe` | Synthetic sibling WAV recognized red and blue |

A remote-image URL returned local `MCP_TOOL_ERROR`; it did not become a successful
generation. HTTP repeated generation and the local error with actual discovery,
session teardown and offline reopening. Empty SSE data/heartbeat events in the
first discovery attempt caused a helper parse failure before any paid call; the
helper now ignores empty data events and has an offline regression.

### Brave Search — 6/8; two open findings

Evidence: `brave-stdio`, `brave-http`, `brave-summary`.

| Tool | Normal result | Observed case |
| --- | --- | --- |
| `brave_web_search` | Passed | Public Rust query includes result URL/display text; literal candidates ambiguous |
| `brave_local_search` | Passed | Coffee-shop search returns display content; extraction exhausted its allowance |
| `brave_video_search` | Passed | NASA query returns content; selected literal JSON; also HTTP |
| `brave_news_search` | Passed | Bounded NASA query returns content and selected literal JSON |
| `brave_image_search` | Passed | Saturn result has structured content satisfying declared schema; also HTTP |
| `brave_place_search` | **Failed** | `MCP_DECLARED_CONTRACT` on the real output |
| `brave_llm_context` | Passed | Mars question returns structured content satisfying declared schema |
| `brave_summarizer` | **Blocked** | Two real summary-enabled searches returned no key; invalid-key error exercised over HTTP |

Ambiguous/exhausted optional extraction does not invalidate a successful display
envelope. It does not promise a selected `data` object either.

**Place Search contract finding:** the response adds root keys `countries`,
`mixed`, `neighborhoods` and `regions`, forbidden by the advertised
`additionalProperties:false`. Independent Python jsonschema 4.26.0 confirmed the
violation against the untouched captured descriptor. The sibling's Zod parser
accepts this response, which does not establish compliance with its advertised
JSON Schema. The raw response remains protected evidence, not a successful output.

**Summary prerequisite finding:** the initial web query and a factual “What causes
ocean tides?” query with summary enabled returned no real summary key. No key was
invented, and no entitlement cause is inferred. A deliberate invalid key produced
`MCP_TOOL_ERROR`, with a reservation covering the server's maximum 20 polls. A
51-word query was also rejected locally. Neither negative case is normal summary
acceptance.

### Chrome DevTools — 32/32 across supported configurations

Evidence abbreviations: **C**=`chrome-four` (40 calls), **F**=`chrome-files-one`
(12 calls), **R**=`chrome-recording` (9 calls). All use an isolated browser and a
controlled localhost fixture, with usage/update/CrUX reporting disabled. F/R grant
only their private output directory with `--workspace`.

| Tool | Phase | Observed case |
| --- | --- | --- |
| `new_page` | C | New controlled page and returned selected page ID |
| `list_pages` | C | Actual page list, including final absence of owned marker |
| `select_page` | C | Select the returned owned page ID |
| `close_page` | C/F/R | Close test-owned pages |
| `navigate_page` | C | Navigate to the owned fixture's second URL |
| `take_snapshot` | C | Accessibility snapshot contains unique fixture marker |
| `wait_for` | C | Wait for the controlled marker |
| `evaluate_script` | C | Exact marker JSON extraction and later form-value assertions |
| `click` | C | Click returned accessible targets |
| `hover` | C | Hover returned accessible target |
| `fill` | C | Fill returned textbox target |
| `fill_form` | C | Fill both identified textboxes; later DOM read verifies second value |
| `press_key` | C | End key on focused textbox |
| `type_text` | C | Type/Tab; later DOM read verifies typed text |
| `drag` | C | Accepted action between returned source/drop targets |
| `upload_file` | F | Actual file input; DOM read confirms synthetic filename |
| `get_css_styles` | C | Style request for the identified controlled heading |
| `emulate` | C | Accepted dark color-scheme/CPU setting |
| `resize_page` | C | Accepted 820×620 viewport resize |
| `handle_dialog` | C | Accept the fixture's scheduled alert |
| `list_console_messages` | C | Locate the actual fixture console marker and message ID |
| `get_console_message` | C | Fetch the returned marker message ID |
| `list_network_requests` | C | Locate the controlled payload request and ID |
| `get_network_request` | C | Fetch the returned fixture request ID |
| `take_screenshot` | C | Accepted envelope preserves actual image block |
| `take_heapsnapshot` | F | Actual 3,389,431-byte heap file |
| `performance_start_trace` | C/F/R | Start actual reload trace |
| `performance_stop_trace` | F/R | Stop trace; F writes 88,838-byte gzip file |
| `performance_analyze_insight` | R | Use actual returned `NAVIGATION_0`/`LCPBreakdown` insight identifiers |
| `lighthouse_audit` | F | Actual JSON and HTML audit files |
| `screencast_start` | R | Start documented experimental ffmpeg recording |
| `screencast_stop` | R | Stop/save actual MP4, independently probed below |

Default filesystem restrictions produced protected upload/heap/trace/audit tool
errors in C. Narrow explicit workspace configuration enabled the successful F
cases. The installed Chrome's default built-in recorder reported unsupported
screen recording; an inactive stop and empty file were not counted as recording
success. R used documented `--experimentalScreencast`, explicit ffmpeg path and
`--screencastFps=5`. ffprobe independently confirmed VP9, 800×600, 10 frames,
2 seconds, **319,083 bytes**. Trace identifiers were parsed from the actual
`## insight set id:` and `insight name:` fields, correcting earlier harness guesses.

Chrome attempts one/two/three retained assertion/deadline failures. The harness
then reduced whole-history inspection overhead; the successful C/F/R results
above are distinct runs. Their explicit tab closure and native shutdown passed.

### context-mcp — 23/23 across isolated attempts

Evidence: **A**=`context-acquisition`, **M**=`context-media` (both deployment
`context-three`); **B**=`context-five` phases; **P**=`context-six-http-purge`;
**T**=`context-seven-retry-factual`. All normal cases below have HTTP evidence;
B also has a separate stdio journey after stopping its owned HTTP instance.

| Tool | Phase | Observed case |
| --- | --- | --- |
| `context_identity` | A/B | Returned principal equals bootstrapped test identity; also stdio |
| `context_create_collection` | A | Create extra collection in owned workspace |
| `context_resource` | A/B | Inspect owned resources; guest read works after grant and fails after revoke |
| `context_ingest_text` | A/B/T | Actual synthetic source, completed processing; also stdio |
| `context_ingest_url` | A | Exact localhost data-plane URL, completed processing |
| `context_create_upload` | M | Presigned upload receipt for synthetic sibling PNG |
| `context_finalize_upload` | M | Actual HTTP PUT succeeds, finalize, processing completes |
| `context_job_status` | A/M/B/T | Observe succeeded/cancelled jobs rather than counting receipts alone |
| `context_query` | A/B | Actual evidence present with blue assertion; also stdio |
| `context_expand` | A/M/B | Expand owned asset/content, actual cropped bytes retrieved |
| `context_asset_history` | M | Read uploaded asset history |
| `context_reprocess` | M | Expected-revision reprocessing completes |
| `context_extract_media` | M | Materialized PNG crop independently checked as 16×16 |
| `context_cancel_job` | T | Cancel controlled slow job and wait for settled cancellation |
| `context_retry_job` | T | Explicit retry of cancelled factual source succeeds and publishes |
| `context_write_memory` | B | Create evidence-derived blue assertion; use returned memory UUID |
| `context_reconcile_memory` | B | Contradictory red assertion produces conflicted view |
| `context_read_memory` | B | Read actual memory UUID with history |
| `context_synthesize` | B | Saved answer includes blue and citations |
| `context_rebuild_index` | B | Rebuild owned workspace index |
| `context_set_access` | B | Grant guest read, verify read, revoke, verify denial |
| `context_purge` | P/B | Purge owned workspace over HTTP and separately stdio |
| `context_purge_status` | P/B | Poll actual purge to completed |

The crop SHA-256 is
`48c26138886877e18bf07a7a00376c70d6f6de9225b9fef367c4e1ffdca6bd6b`.
Context's private operator graph copies bound generation outputs to 512 and use a
unique sandbox session marker. The slow profile adds an explicit synthetic sleep.
Application upload/segment/job limits and concurrency are bounded. Real nested
llm/sandbox providers and the newly built media image perform processing.

Earlier setup attempts exposed operator errors: private Docker config needed its
Compose plugin path; discovery needed the actual sandbox snapshot version;
`read_memory` needed the returned memory UUID. Those attempts were preserved.
The original vague slow-source retry returned an accepted receipt but later failed
application contract validation. Read-only history audit found model-produced
relationships with undeclared entity references; the underlying extraction commands
exited zero. The later factual-input T case completed and published successfully.
This does not erase the earlier model/application-contract failure.

## Provider accounting

The user authorized **$25** total. The private locked ledger holds **$17.00** of
conservative reservations, including failures and uncertain charges. The ledger's
`provider_requests:186` is a reserved request allowance, not observed usage. One
unused $15 setup reservation was released before any provider dispatch; the held
$15 context reservation was shared across later attempts, not renewed per attempt.

Auditing all stopped context histories, including failed attempts, plus direct
llm results found **38 usage-bearing tool responses: 5 direct and 33 nested**.
These are received usage records, not an exact count of upstream HTTP attempts.

| Model | Responses with usage | Input tokens | Output tokens | Cached input included |
| --- | ---: | ---: | ---: | ---: |
| `gpt-4.1-2025-04-14` | 15 | 3,831 | 1,532 | 1,024 |
| `gpt-4.1-mini-2025-04-14` | 2 | 29 | 8 | 0 |
| `gpt-6-luna` | 1 | 277 | 0 | 0 |
| `gpt-4o-mini-transcribe` | 1 | 29 | 12 | 0 |
| `text-embedding-3-small` | 19 | 477 | 0 | 0 |

The published standard token rates give approximately **$0.02 of observed OpenAI
usage**, before Brave and billing adjustments; this is not an invoice. Public
pricing checked on the campaign date:
[OpenAI](https://developers.openai.com/api/docs/pricing) and
[Brave](https://brave.com/search/api/). Per-million input/output rates used are
GPT-4.1 $2/$8 (cached $0.50), mini $0.40/$1.60, Luna $0.10/$0.50,
mini-transcribe $1.25/$5, embedding $0.02 input. Brave reservations cover 31
potential upstream requests, including local-search expansion and 20 summary polls;
the advertised Search rate was $5/1,000. Actual Brave billing/poll count was not
independently metered. Held reservations were not released on this estimate.

The ledger gates new reservations; it is not a provider-side billing meter for
nested context activity. The observed 33 nested usage responses fit the held
150-request allowance. Final validation/auditing added no charged calls.

## Cleanup audit

Finally blocks stopped owned fixture servers and native sessions; successful
Chrome phases closed their owned tabs. Context teardown stopped the owned app,
removed exact-label sandbox orphans, and ran isolated Compose down with volumes.
Private durable HTLK histories and synthetic outputs remain for inspection.

`tools/live_cleanup_audit.py` independently checked every recorded ownership label:
six context Compose projects have no containers, volumes or networks; four sandbox
campaign markers have no remaining containers. No campaign-identified process
remained, and the Chrome main-process count was zero, including after interrupted
Chrome attempts. This final audit does not rewrite their original incomplete
cleanup reports. The unrelated preexisting context process was not targeted.
No global browser, Docker, database or credential reset was used.

## Reproduction and offline checks

Build the pinned siblings with their own instructions. `tools/live_setup.py` is an
operator-owned macOS/Apple Silicon convenience configuration for adjacent sibling
checkouts and installed Chrome/ffmpeg paths. Review those secret-free host files
for the actual machine. It creates a fresh private campaign, bounded sandbox
profile and separate anonymous Docker config. Context additionally requires the
pinned storage images and its current media Dockerfile build, tagged
`htlk-context-media:siblings-20261029` as referenced by the controller.

Example explicit setup/discovery/launch (choose a new directory under an existing
private parent; each controller refuses to reuse its session output):

```console
cargo build --example mcp_live_session --locked
python3 -B tools/live_setup.py PRIVATE_CAMPAIGN
python3 -B tools/live_env.py --env-file ../llm-mcp/.env --allow OPENAI_API_KEY -- node tools/discover-mcp.mjs PRIVATE_CAMPAIGN/llm.host.json PRIVATE_CAMPAIGN/llm.catalogs.json
python3 -B tools/live_env.py --env-file ../llm-mcp/.env --allow OPENAI_API_KEY -- python3 -B tools/live_llm.py PRIVATE_CAMPAIGN
```

`live_env.py` loads only named literal values into the child environment, without
shell evaluation/interpolation. Brave uses its own dotenv with `BRAVE_API_KEY`;
context uses the selected OpenAI key and its own isolated issued credentials.
Sandbox/Chrome discovery uses the same Node discovery helper without credentials.
The other opt-in entry points are `live_sandbox.py`, `live_brave.py`,
`live_brave_summary.py`, `live_http.py`, `live_chrome.py`, `live_chrome_files.py`,
`live_chrome_recording.py` and `live_context.py`; their `--help` identifies phase
arguments. Paid live controllers are intentionally separate from ordinary tests.

Offline verification requires no provider credentials:

```console
cargo test --example mcp_live_session --locked
python3 -B -m unittest discover -s tools -p live_tools_test.py -v
target/debug/examples/mcp_live_session --inspect SAVED_SESSION_DIR
python3 -B tools/live_cleanup_audit.py PRIVATE_CAMPAIGN
```

For independent saved-schema and nested-history auditing, use a disposable Python
environment with `jsonschema==4.26.0` and `cbor2==6.1.5`:

```console
python3 -B tools/live_schema_diagnose.py PRIVATE_CAMPAIGN/brave.catalogs.json brave_place_search PRIVATE_CAMPAIGN/brave-stdio/raw-006.json
python3 -B tools/live_campaign_audit.py PRIVATE_CAMPAIGN --sessions --output NEW_PRIVATE_AUDIT.json
```

Stop all writers and checkpoint SQLite WAL before immutable history inspection;
the audit rejects a nonempty WAL. The metadata-only audit preserves run/executable,
catalog-file/program-file SHA-256, invocation/attempt/binding/result-artifact/raw
file/envelope identities without publishing response bodies or credentials.

## Durable run anchors

The complete private index is `audit.json`; named phases above resolve its entries.
Selected primary executable/run anchors are:

| Session | Run ID | Executable fingerprint |
| --- | --- | --- |
| sandbox-one | `run:568ce050e99bab0e1cb0032af06809c5:1` | `sha256:9807025ddcfe460e63d85bd11c3268f7a0817569557fb3f2222983be32e97d02` |
| llm-stdio | `run:3674efe495b35c04af995545f03906bf:1` | `sha256:f06d6ddbaed987ee11274c87e75aef873a84bb28a18c39bfb54f5a4ef9b58611` |
| brave-stdio | `run:a8f1aa8a0102c87bc9bbcc1515d3221d:1` | `sha256:621609ebd0f2be6f94bdfa40f6f63dd8bed84242e397a277ab9ff8799727e941` |
| chrome-four | `run:e65ccbf576cae349ef69365d918dd60c:1` | `sha256:691ec9530e1a11382b07e7f2039f83332f684900e6361030460624a0e26e146b` |
| chrome-files-one | `run:ed06407d74be260dc1e6f3f80b8d2bc4:1` | `sha256:587ddf8890d43ff21d6cc3c01184b7ea647771f301b724e966b063ac8bf5ca40` |
| chrome-recording | `run:b38a77b757396fbef59ffc20e61a3e61:1` | `sha256:8496b6e836877708857662b6da2d39d8727136cc02f3599d3317fc3645d140e0` |
| context-acquisition | `run:82e4c9dacffc694d02727f430383a236:1` | `sha256:839922de8829d4e8ce9ff997be3db91362d68cdea4dc3998cc85be2829a93459` |
| context-media | `run:45af7c51ab1000b618d15c97ce7c7c88:1` | `sha256:715cd4d945cb55b4e89535746bb19a0350f7e187b2b731d420e790f51ed35a64` |
| context-five-memory | `run:a3d0448e37115eb0d7a4fc5141c5a855:1` | `sha256:b26dfbf24f54ed6b2274fd69d4f8c5f9472ae784e837bf2e5918b1f151452b5f` |
| context-five-stdio | `run:df881db435c45f89e5d6ff464947a178:1` | `sha256:2b6179ce137c93c7ee37276e995378bded8b6110f44dea34d73d995cddcdde83` |
| context-six-http-purge | `run:d8dcac3f8b24003cc1bbc5b53bc2dd7d:1` | `sha256:9f797454655ceadb54eb607270f72384428c480d0e2621160873b46a1669dd37` |
| context-seven-retry-factual | `run:0b0593a24ade35eb24576c4ae0d477ef:1` | `sha256:eb50938aa4fb93a75ec036798dde17ba466594139a2c0ee70614d763aaf03055` |

For example, context identity step A/1 retained binding
`sha256:283903d7be7934c4cbd4ccd5dccd0d2621dd2295cc56ed6d2011b51d72ade686`,
attempt `sha256:6b678e57440a796aafbfb4fb03e11bc10e321db459b3a89283243111def1301c`
and normalized envelope
`sha256:2f2b0e7d6dbbfd0587e48f5f7689f02dd18bcd513cdc21495a09bbd95df990be`.
The exact descriptor travels in the preserved catalog and binding; catalog-file
SHA-256 is a separate file identity, not a claim about canonical descriptor hashing.

## Validation record

Passed: `cargo fmt --all` and check, strict workspace/all-target/all-feature Clippy,
complete workspace/all-feature tests and doctests, the live-session independent
stateful peer regression (including one provider initialization, error evidence,
tampering refusal and zero-call offline verification), four Python credential/
ledger/SSE tests, Ruff formatting/lint, specification and independent executable
fixture validation, Node discovery/coverage-policy tests, warning-denied workspace
rustdoc, dependency/license policy and exact third-party-notice regeneration.
All five workspace crates also packaged and verified successfully with
`--workspace --locked --allow-dirty` in a fresh temporary target directory;
workflow syntax passed actionlint. The published matrix's 75 tool names were
independently compared with the captured inventories, with no missing/duplicate rows.

The first full-suite wrapper was too short; a subsequent complete attempt stalled
in the existing crash-recovery test. That test passed on focused rerun, and the
next full workspace run passed. No timing workaround or production change was
introduced. Existing dependency-duplication warnings remain permitted. This
campaign changes no coverage thresholds and does not claim new line/branch metrics.
