# Sibling MCP interoperability — 2026-11-06

This dated campaign exercised **all five sibling MCP server projects** against
HTLK revision `e51732857e50673641eb71208c67ad337b7e80b7` on
`feat/complete-mcp-client` (PR #20). Tests used the real servers and providers,
fresh discovered descriptors, the native client, and ordinary compiled graphs.
These are measured samples for the identified revisions, not exhaustive server
conformance or a substitute for the [protocol feature inventory](mcp-conformance.md).

## Results

| Sibling / revision | Transport | Verified behavior | Remaining finding |
| --- | --- | --- | --- |
| Chrome DevTools `ada7963246c0c91eae185125a57aecd13cbe0376` | Stdio | **32/32 tools** had successful real graph calls; isolated page continuity, form interaction, screenshots, recording, network/performance/filesystem operations; actual roots inquiry and tools-list-change event | Early test-targeting/path-denial cases were corrected and passed. |
| Brave `af08e0d48e812a3966ea2211854db5f0e8eee32d` | Stdio, stateless HTTP, stateful HTTP | **6/8 tools** had positive cases on every transport, including actual web/news/video/image/context/local results; all eight had bounded negative coverage | Place Search violates its advertised output schema. Positive summarization was unavailable because searches returned no summary key. |
| LLM `667cc010bc247bb337b6160dc1e958c2b77886ed` | Stdio, sessionful HTTP | **4/4 tools** passed on both OpenAI and OpenRouter: generation, embedding, classification, transcription; additional vision, schema-output and tool-continuation cases | Initial OpenAI 401 responses were resolved by the operator's credential update; fresh retests passed. |
| Context `7ecad1fe51de6d57a35f1c3792fa16e2b96ddcb3` | Authenticated stdio and HTTP | **14/23 tools** exercised; real document publication, query/expansion and citation checks, three resource-template families, ACL/index/history/memory operations, purge and post-purge denial | Not all application tools/formats were attempted. Slow stdio startup needed an explicit larger client timeout. |
| Sandbox `8c649a415ba7be6aa6e15a4dc51e56dc5990eae0` | Stdio | **6 tools** succeeded, six guidance resources were read, and uninterrupted create/exec/info/list/destroy was independently verified | Statistics ended the original session uncertainly. Restarted-server destroy falsely reported removal; this is retained as an upstream cleanup defect. `destroy_all` was not invoked. |

`htlk-nlp` / `htlk-nlt` is a consuming host rather than an additional MCP server,
so it is not counted as a sixth server.

## Client paths and independent observations

Fresh discovery used `tools/discover-mcp.mjs` and the native `mcp_client` example.
The durable `mcp_live_session` runner compiled exact-pinned tool calls and retained
original responses, normalization, delivery, and policy evidence. Small additional
harnesses used public `NativeMcpClient` APIs for resource reads and utility requests;
they did not supply a custom MCP adapter. Initialization or a server health probe
alone was not treated as successful tool/provider execution.

### Chrome and sandbox

Chrome's isolated synthetic pages carried unique titles and markers. Script results
and accessibility snapshots independently agreed. Actual response page IDs and
fresh snapshot UIDs were used for later requests. File-producing operations created
nonempty campaign files with recorded digests; performance analysis used an actual
returned insight-set ID.

A dedicated native-client run answered Chrome's real `roots/list`, then changed
only the campaign-owned watched configuration. The host received one real
`notifications/tools/list_changed` while **tools/call remained zero and tools/list
remained one**. Receipt did not automatically refresh the catalog or acquire content.

Sandbox ran a digest-pinned narrow profile against disposable files. Execution
returned the expected unique marker, uid 65534, and a successful scratch-file check.
The uninterrupted cleanup control returned an empty session list, and Docker
independently confirmed the exact created container no longer existed.

The original statistics failure had no admitted raw response and retained
`E_EFFECT_UNCERTAIN`. Later operations were refused locally rather than silently
creating a new session or replaying work. Its root cause was not established in
this campaign. After session loss, a fresh sandbox server reported `destroyed:1`
although independent Docker inspection still showed the original container. The
operator explicitly approved removal of that exact campaign-owned container;
manual recovery was performed and absence verified. This is **not** counted as
successful MCP-server cleanup. No further sandbox bug investigation was performed.

### Brave and LLM

Brave returned actual source URLs or named/addressed local businesses. Independent
Draft 2020-12 AJV validation confirmed the Place Search contract failure: the
discovered `additionalProperties:false` schema omits top-level properties actually
returned by the server, including `countries`, `regions`, `neighborhoods`, and
`mixed`. HTLK's `MCP_DECLARED_CONTRACT` rejection was correct. The test did not loosen
the schema or count an invalid summary key as positive summarization.

LLM results demonstrated unique generated markers, nonzero finite 64-dimensional
embedding vectors, meaningful classification probabilities, and transcription of
actual synthetic speech. Additional calls exercised image understanding, constrained
JSON output, returned function calls and full-history continuation, and
image-description embeddings. Text JSON and `structuredContent` copies agreed.

After the credential update, OpenAI's focused retest completed **13/13 tool calls**:
nine over stdio and four over HTTP. All four tools passed on both transports, with
zero new provider errors. Historical 401 evidence was preserved rather than
relabeled. An outdated prebuilt LLM binary ignored the current OpenRouter selector;
the current locked sibling source was built into the ignored campaign directory,
without changing the sibling checkout, and rediscovered before positive tests.

Seven native utility probes verified real ping and supported logging-level requests,
plus local capability rejection for unsupported services. Brave and LLM emitted no
notifications during the bounded watches, so those runs establish no positive
notification-delivery claim.

### Context application and providers

Context used isolated PostgreSQL, Typesense, S3-compatible storage, private staging,
and HTLK state. The existing user's application and content were left intact.
Explicit operator graph copies selected supported provider/model parameters; no
output repair, invented descriptors, or sibling source adaptation was introduced.

OpenRouter-backed stdio and HTTP documents reached terminal publication with one
chunk and no pending cleanup. Queries returned exact unique source text with matching
asset/revision citations. Expansion verified a source relationship; native reads
covered revision, content, and memory templates. Post-purge reads were denied.

After the OpenAI key refresh, a narrow second run published one new tiny document
per transport. Queries and **four native resource reads** matched their original
text and citations. Both generation calls and all four embedding calls succeeded;
all eight application child workflow runs succeeded. Reported generation usage was
287 input and 257 output tokens; embeddings reported 80 tokens. OpenAI supplied no
dollar-cost field, so no actual dollar total is asserted. The earlier successful
OpenRouter campaign reported aggregate provider cost of $0.00324964.

Historical failed jobs remained failed in the evidence. Admission of an ingestion
job was never equated with publication. The follow-up used 16 graph commands:
14 successes and two intentional post-purge denials.

## Durable evidence and cleanup

- Chrome/sandbox: six offline durable reopens verified 100 saved steps and 97
  admitted raw-result digests, with zero provider calls.
- Brave/LLM: nine initial reopens verified 68 commands; the OpenAI refresh added
  two reopens verifying 13 commands. All inspections dispatched zero provider calls.
- Context: nine initial reopens verified 73 commands and 72 raw-result digests;
  the OpenAI follow-up added two reopens and 16 matching raw-result digests.
- Negative cases preserve local admission errors, explicit tool errors, declared
  contract violations, protocol rejections, and uncertain effects distinctly.
- Campaign-owned browser/server/client processes and HTTP listeners were stopped.
  Context purges completed and its isolated dependency containers and volumes were
  removed. The sandbox exception and explicitly approved manual recovery are
  described above.
- Credential files were loaded privately into child environments. Silent
  credential-value equality scans found no credential values in retained campaign
  artifacts, including databases. No credentials or private evidence are committed.

Full local raw traces, compiled graphs, SQLite stores, independent audits, and
controller scripts remain in ignored campaign directories. The committed record
contains redacted measured findings, not credential-bearing host configuration or
copies of changing upstream catalogs.

## Resulting HTLK change and reproduction

No HTLK protocol/runtime defect was demonstrated by these siblings. The native
browser example's fixed five-second startup timeout was too short for the context
stdio process. `--connect-timeout-ms` now permits an explicit **1000–120000 ms**
startup/default request allowance while catalog traversal and subscription RPCs keep
their own finite budgets. For a slower stdio deployment:

```console
cargo run -p htlk-runtime --example mcp_client --locked -- --connect-timeout-ms 30000 stdio SERVER_NAME SERVER_VERSION /absolute/path/to/server ARG
```

Focused independent-peer tests verify that a 6.5-second initialization fails under
the default and succeeds with a 10-second setting, while a slow catalog page still
hits its separate five-second bound. There is one initialization attempt and zero
content/effect calls. The actual context stdio path separately passed using the
public native client with an explicit 30-second bound.

Use the maintained [live-session runner](mcp-guide.md#run-a-durable-live-session),
[catalog browser](mcp-guide.md#run-a-host-owned-catalog-browser), and
[discovery helper](mcp-guide.md#discover-catalogs) with current server identities and
fresh output directories to repeat a campaign. Assert provider outputs and scoped
cleanup, not merely successful RPC envelopes. Stdio and HTTP deployment identities
and transport configuration must match the fresh catalog selection.

These siblings do not expose every new MCP service. Their measured behavior does
not establish real-server sampling, elicitation, subscriptions, or protocol-task
interoperability where absent. Independent protocol peers and durable-store tests
remain the supporting evidence for those features.
