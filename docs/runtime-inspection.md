# Protected inspection and detached source diagnostics

Authority: runtime §§8 and 15. Every logical view receives an authenticated
`HostContext`; returned identities never grant artifact access. Current policy
authorizes complete value/control/evidence protection before a view is returned.

## Logical state and history

`inspect_run` returns active fingerprint/root/ownership, status and fixed root
failure, complete current protection, root manifests, runtime scope hierarchy,
open requests, usage, extension history and persisted guard/binding decisions.
Run and node views also expose protected `diagnostics`: canonical contract sites
and all destination-validation failures, with deterministic primary error selection.
Public output and loop-next decisions use private `$outputs`/`$next` markers,
separate from source node names. Bindings retain selected edge, source error and
protected causal links; guard decisions retain actual evaluated outcome causes.

`inspect_node` identifies its original executable/definition/canonical location,
runtime scope/local name, status/reason, complete decoded frozen input manifest and
hash, limits/deadline, accepted outputs, failure, child/iteration/feedback state,
guard/binding evidence, request history and delivery-aware attempt metadata.
Attempt views include count, ownership, policy and replay snapshots, delivery and
fixed failure classification. Raw arguments, unvalidated result bytes and raw-value
digests are excluded; artifact reads still require their independent authorization.

Dependency causes identify original invocation, failure code and canonical source
location. They are not interpolated into the graph-visible `{code,message}` error.
Viewer-specific source visibility cannot rewrite an already committed error value.

`inspect_history(context, run_id, after_sequence, page_size)` returns bounded journal
summaries: sequence, event, writer, time, causation and changed record keys. It never
returns private journal payloads or raw MCP proposals. Continue from `next` until an
empty terminal page. Run/node/history readers verify stable committed sequences;
continuous concurrent changes can return retryable `E_FENCED` instead of a torn view.
Metadata projection and aggregate byte allowances bound observations independently
of stored raw response size.

`blocked` uses the scheduler's completed eligibility scan at the exact observed
journal sequence, rather than interpreting pending-node reason strings. Any new
commit or due timer invalidates that rebuildable observation. It is false while
internal scanning/work remains outstanding and is recomputed after recovery.
Mounted preserved scopes and retry timers follow the same rule. This index is
bounded, process-local and cannot authorize or suppress graph execution.

`inspect_graph` displays the immutable portable document and use hierarchy even
when its original native implementation is unavailable. `native_available` reports
current admission separately. The graph includes a bounded authorized source page;
`inspect_sources` continues its opaque provenance cursor.

## Source metadata outside executable identity

`SourceSidecar` contains an independent package/report provenance digest and
`SourceEntry` values. Each entry identifies an RFC 6901 canonical target, inline or
exact package/module/file origin, exclusive authored UTF-8 byte span, optional
declaration and description. Several origins may refer to one deduplicated target.
Generated joins use `SourceOrigin::Synthetic` with no invented authored span.

`attach_source` requires a registered graph, validates complete bounds and every
canonical pointer, obtains trusted `source_labels`, authorizes `AttachSource`, and
saves an immutable publisher-attributed attachment in detached graph provenance.
Identical reports return their digest; another description/origin adds a new report.
Neither operation changes executable bytes, fingerprint, definition IDs, producer
trust, active work or error records.

`inspect_sources` checks graph and per-report `ReadSource` grants. Denied reports
are omitted while authorized origins remain intact. Historical metadata needs only
the registered portable document, not old predicate/library execution code.

`inherit_sources` copies authorized entries only where old/new canonical target
values are byte-equivalent under the model, retaining old labels even if current
classification becomes weaker. Operand scope-definition paths survive compiler
joins unchanged; changed public roots/ports are excluded. Hosts explicitly attach
synthetic markers for generated wrappers rather than attributing them to old text.

## Host workflow example

Run `cargo run --example runtime_workflow --locked` for source compilation,
registration, detached source maps, a typed planning window, an actual compiler
join, atomic extension/window closure, inspection and artifact reading. Its second
workflow uses a scripted pinned tool and a separately authenticated exact-action
approver. It needs no provider credentials or live model service.

Tests cover report identity/privacy/multiple origins/inherited protection, unchanged
envelopes, synthetic joins, causal dependency inspection, public-boundary evidence,
complete input checkpoints and bounded contiguous history summaries.
