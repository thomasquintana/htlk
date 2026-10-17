# Checked evaluation influence and boundary authorization

Authority: [runtime §§3, 6 and 8.1](../specs/runtime-spec.md), and expression contexts
in the [syntax reference](../specs/htlk-ir-syntax-reference.md).

`InfluenceContext` wraps an existing typed data context with trusted coordinator
protection metadata and a current read-authorization callback. It does not attach
authority to ordinary values or replace runtime admission. Use its `evaluate`
method with a `VerifiedExecutable`, `ScopeUse` and authored `ExpressionSite`.

## Actual reads and lazy behavior

The environment carries enclosing base protection plus named reference/outcome
protection and optional causal locations. Base labels cover enclosing admission
and control decisions even for constant expressions. Additional metadata is read
and authorized only when the evaluator actually requests that value or outcome.
An unevaluated lazy Boolean branch does not trigger authorization, data access or
additional influence. Repeated reads of the same source are traced once per
evaluation; every new evaluation rechecks current policy.

Missing required metadata fails closed with invariant failure. A denied read
propagates its exact stable failure rather than becoming `false`, null, absence or
a cached successful result. Both native errors and policy failures retain actual
influence/causes. Metadata exhaustion returns an error instead of an incompletely
labeled result.

## Projection, outcomes and native calls

The adapter forwards schema checking, projections, templates, native functions and
checked callback boundaries. Projection cannot remove labels on the source root.
Checked schema origins and lazy operand order remain owned by the existing engine.
Function references remain static capabilities, not application data.

Terminal `status`/`error` reads authorize outcome labels and retain causal links.
The original committed redacted error message is used for failed nodes; inspection
redaction cannot change it. Per the syntax specification, `error(@node)` returns
null for nonfailed terminal outcomes, including cancelled, while status still
reports the actual immutable outcome. Transient MCP attempts are not outcomes.

## Runtime integration

Root preconditions now use this adapter with the frozen root manifest's protection
and `Authorization::ReadBoundary`. Metadata/source artifact authorization remains
separate: reading through a boundary cannot strip control protection from an
otherwise unmodified artifact alias. The authenticated principal comes from host
context; no graph value supplies identity or grants.

Coordinator-created node/scope frames must populate environments from authoritative
manifests, outcomes and bindings, include enclosing influences, retain the returned
protection on failure/absence as well as value, and commit decisions under fences.
The adapter itself creates neither artifacts nor terminal outcomes.

## Verification

Four real-compiler tests cover lazy sensitive RHS suppression, denial-as-error,
protected status/error reads with stored messages/causes, projection protection,
current-policy rechecks and missing metadata. The root admission suite continues
to verify protected input aliases and contract evidence under this boundary hook.
