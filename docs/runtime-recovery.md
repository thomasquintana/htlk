# Fenced startup recovery and replay

Authority: runtime §14. The execution supervisor scans authoritative running runs,
acquires a higher ownership fence after expiry, and verifies durable state before
resuming graph work. Mutating commands return retryable `E_FENCED` while their new
local coordinator is still completing recovery.

## Checked recovery

The runtime re-admits the active executable and every scope's exact origin under
available native implementations. It preserves the original `ScopeUse`, definition,
scope and invocation IDs. Stored input checkpoints are decoded against authoritative
declarations, their complete logical manifests and input hashes are recomputed,
and referenced typed values/protection are checked. Invocation definition/origin
identities and ordinary/loop body use locations must agree with their executable.

Artifact verification is consumer-specific: final metadata must match producer
declarations, validation fingerprint, final producer/output slots, labels, raw bytes
and typed/provenance hashes. Accepted parent links remain immutable. Relevant global
producer-slot index rows are rebuilt transactionally from canonical metadata; an
index is never used as proof that an output was accepted. Missing/corrupt retained
bytes and invalid metadata fail recovery; explicit tombstones remain distinguishable.

Attempts retain frozen encoded parameters, one-based IDs and delivery state. Waits
retain invocation/generation identities and deadlines. Usage must account for every
outstanding reservation, including ancestry adopted by extension. Rebuilding derived
indexes does not replace accepted records or append fabricated execution decisions.

## Dispatch reconciliation and liveness

- `prepared`: no intent was committed; normal policy/approval/budget checks precede
  any first send.
- `possibly_sent`: delivery is ambiguous. Only trusted replay/deduplication policy
  and the authored retry intersection can permit another attempt; otherwise the
  invocation fails `E_EFFECT_UNCERTAIN`, retaining outstanding reservations.
- `response_recorded`: resume local protocol/schema/postcondition validation and
  acceptance. A successful recorded result needs no MCP adapter and sends nothing.

Pure work can recompute missing decisions under exact code and frozen inputs.
Committed loop feedback resumes the current iteration and the same open input wait.
Saved guards, dispositions, accepted results and prior iteration IDs are preserved.

Fatal implementation/checkpoint/invariant failures finalize the fenced run and stop
descendants. Transient database/ownership conflicts remain retryable. If a reducer
pass makes no progress and has no open external wait or future retry delay, it fails
`E_INVARIANT`; concurrent committed changes first trigger a fresh reducer pass.
Deadline and cancellation transitions remain responsive while workers calculate.

## Journal replay

`ReplayState::begin/apply` consumes bounded journal pages with no adapter, database
writer or execution capability. It checks sequence, time, ownership, revisions,
finality, command identities, initialization, extension root changes and terminal
receipt/tombstone restrictions. The caller selects finite reconstruction bounds.
Replay reconstructs decisions; it does not recall MCP or replace committed choices.

`tests/runtime_recovery.rs` reopens an actual SQLite response-recorded checkpoint
without an MCP adapter and compares replayed headers and record families. It also
reopens a loop at committed feedback/open wait, and checks missing implementation,
corrupt checkpoint and impossible-progress failures. Storage tests remove derived
producer indexes, verify reconstruction without journal changes, and reject damaged
bytes. MCP tests additionally exercise unsafe owner takeover and stale late results.
