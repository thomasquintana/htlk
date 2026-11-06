# Complete context MCP tool campaign — 2026-11-06

**All 23 advertised context tools have a verified normal functional outcome in this
fresh campaign.** This closes the nine-tool gap in the earlier
[sibling interoperability campaign](mcp-interoperability-2026-11-06.md).

Tests used HTLK `a8f02ef6ff67c3c5128acd906dc7d89c6c957b9a`, context-mcp
`7ecad1fe51de6d57a35f1c3792fa16e2b96ddcb3`, and llm-mcp
`667cc010bc247bb337b6160dc1e958c2b77886ed` with the refreshed OpenAI credential.
The existing context binary, real model/sandbox providers, PostgreSQL, Typesense,
and S3-compatible storage were used. No sibling source or custom MCP adapter was
changed. Operator graph/profile changes used documented configuration boundaries.

## Scope and results

| Transport | Graph commands | Successful results | Expected errors | Tools with a normal case |
| --- | ---: | ---: | ---: | ---: |
| Stdio | 6 | 6 | 0 | 5 |
| Authenticated Streamable HTTP | 50 | 43 | 7 | **23** |
| Total / union | **56** | **49** | **7** | **23/23** |

All seven errors were deliberate post-purge unavailable reads. There were no
unexpected graph failures or blocked normal cases. This is complete tool coverage
over HTTP plus representative stdio coverage, **not** all 23 tools on both transports
or exhaustive coverage of every argument, file format, or authorization variant.

Eight asynchronous jobs were followed through settled outcomes: six succeeded,
one was deliberately cancelled, and one deliberately failed before an explicit
successful retry. Admission receipts alone were not counted as publication.

Both transports also passed fresh native catalog-browser discovery using
`--connect-timeout-ms 30000`: 23 tools, three resource templates, zero enumerable
resources, and no prompts capability. All 26 descriptors agreed exactly with the
catalogs used for compiler/runtime pins. Native stdio resource reads verified the
exact published revision and original content.

## Tool-by-tool observations

Every row below passed an actual normal case through the native durable graph path.

| Tool | Independently checked outcome |
| --- | --- |
| `context_identity` | Authenticated principal matched the bootstrapped test owner and returned scope ceiling. |
| `context_resource` | New hierarchy metadata matched parent/owner; cancelled and failed assets had no current revision; purged assets became unavailable. |
| `context_create_collection` | New collections belonged to the requested test workspace and owner hierarchy. |
| `context_create_upload` | Actual signed conditional PUT transferred the independent UTF-8 fixture successfully. |
| `context_finalize_upload` | Job reached published success; authorized source download matched the uploaded SHA-256. |
| `context_ingest_text` | Text published and query returned exact unique original text with the matching revision citation. |
| `context_job_status` | Actual success, cancellation, failure, and retry transitions reached terminal states with cleanup settled. |
| `context_cancel_job` | A running extraction was cancelled; cleanup settled and no current revision was published. |
| `context_retry_job` | A known failed job retried after profile repair; a new job published the same immutable source revision. |
| `context_ingest_url` | One controlled allowlisted HTTP fetch ingested a unique PNG; downloaded source bytes matched the fixture hash. |
| `context_query` | Returned exact source text and available asset/revision citations within finite evidence budgets. |
| `context_expand` | Authorized original bytes, exact media source/location, and saved generated-answer dependencies matched their inputs. |
| `context_extract_media` | Published a real 16×16 image crop; a separate decoder verified all 256 RGB pixels against the independent source marker. |
| `context_asset_history` | Uploaded and reprocessed revisions remained immutable, with the old revision historical and new revision current. |
| `context_reprocess` | Same asset gained a distinct published revision; original source bytes remained identical. |
| `context_set_access` | Owner ACL replacement returned version 2 and the requested inheritance/resource identity. |
| `context_write_memory` | Initial source-backed assertion retained version 1, authenticated authorship, and the exact citation. |
| `context_reconcile_memory` | Real model classified a paraphrase as equivalent; version 2 and both cited/authored history records were verified. |
| `context_read_memory` | ID history returned logical version 2, two revisions, authorship, citation, and reconciliation rationale. |
| `context_synthesize` | Real saved answer contained the known source fact and exact evidence citation; expansion retained generated origin and source dependency. |
| `context_purge` | Purged only the newly bootstrapped hierarchy, including source, media, memory, and generated-answer content. |
| `context_purge_status` | Receipt progressed from pending to completed; staging emptied and post-purge asset reads were denied. |
| `context_rebuild_index` | Explicit workspace rebuild indexed the published test evidence without requesting source regeneration. |

## Controlled lifecycle cases

### Upload, URL acquisition, and media

Upload reservation was followed by the actual signed PUT with conditional and
SHA-256 headers. Finalization and reprocessing were followed to publication.
Independent downloads matched the original fixture bytes; reprocessing created a
distinct revision while preserving the original source.

The URL source was a campaign-only numeric-loopback fixture, explicitly allowed
by the operator configuration with development HTTP enabled. It observed exactly
one fetch. No unrestricted egress, redirect proxy, or fabricated response was used.
The 32×32 source PNG contained an independently generated unique marker region.
The real extraction pipeline produced a 16×16 crop whose entire pixel matrix
matched that region, rather than merely returning a successful processing receipt.

### Cancellation and explicit retry

For cancellation, an operator graph used the real native extraction command with
an explicit delay. Polling observed a running extraction before cancellation.
Terminal state was cancelled, cleanup was settled, and the asset never acquired a
published current revision.

For retry, a test-only graph requested an intentionally absent sandbox profile.
The actual provider returned the missing-profile failure; the job settled failed
with no current revision and no pending cleanup. After stopping the single application
instance, the operator installed the known pinned reference profile and restarted
against the same isolated durable state. An explicit retry created a new job that
published the same source revision. Failed and successful extraction runs retained
the same role executable fingerprint. No unknown effect was automatically replayed.

### Memory and synthesis

A source-backed assertion and paraphrase were reconciled by the real model.
History retained both revisions, their author/citations, and the equivalence rationale.
Synthesis used an explicit evidence ID and finite evidence budget; the saved answer
contained the known source fact, cited the supplied evidence, and expanded to a
generated record with the input source-revision dependency.

## Evidence, usage, and cleanup

Three durable sessions reopened offline with **zero provider dispatches**. All
**56 raw-result digests** and **56 structured/text copies** independently agreed.
Observed embedding vectors were finite, nonzero, and 256-dimensional.

The actual providers received eight generation and ten embedding calls. OpenAI
reported 1,944 input and 871 output generation tokens, plus 247 embedding tokens.
It supplied no dollar-cost field; no billed dollar total is asserted.

Application-managed purge completed, seven post-purge reads for five distinct
assets were denied, and private staging was empty. Owned client/application
processes, fixture listeners, and isolated backing-service containers/volumes were
released. Existing user application state was not modified. HTLK execution history
remains retained outside the application's purge boundary.

**Physical sandbox-container absence is not claimed.** Normal provider destroy
receipts were accepted under the previously acknowledged upstream sandbox lifetime
limitation. The final names/status inventory showed an additional container; no
further sandbox investigation or direct cleanup bypass was performed.

Credentials were loaded privately. A silent equality scan of retained configurations,
reports, binaries, and SQLite evidence found zero source-credential matches. Signed
upload/download URLs remain private short-lived capabilities, not committed evidence.

Full request/result files, compiled graphs, identities, provider usage, SQLite stores,
and the independent verifier are retained under the ignored local campaign directory
`.opencode/.tmp/live/context-all-tools-20261106/`. Its audit checks all 23 discovered
tool names against independently verified outcomes, source hashes, terminal job
states, offline evidence, and scoped teardown. It passed again during report review.
The live campaign requires fresh state; the audit itself dispatches no provider work.

No HTLK protocol/runtime fix or sibling source modification was needed. These
observations establish normal functional interoperability for the listed tools at
the identified revisions, not exhaustive application correctness.
