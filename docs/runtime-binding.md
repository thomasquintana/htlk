# Deterministic binding and input selection

Authority: [runtime §5](../specs/runtime-spec.md). `htlk_runtime::binding` supplies
the shared pure reduction rules. These results are proposals, not authorization,
type-validation, dispatch or persistence proof. Coordinator integration follows.

## Uniform destination reduction

`reduce_binding` accepts candidate edge IDs, a guard evaluator and a local source
resolver. It bounds/sorts candidates and rejects duplicate IDs. Every guard is
evaluated in canonical edge order, preserving the evaluator's own lazy Boolean
semantics within each guard. It never uses the first completed source as a winner.

The result follows the specification order:

1. Any pending guard keeps the destination pending, including when another guard
   errored or selected a value. Pending results retain settled guard failures.
2. Once all guards settle, guard errors fail in edge order and retain all causes.
3. Multiple true guards produce binding conflict without resolving a source.
4. No true guard produces absence without resolving false-edge sources.
5. One true guard resolves exactly one complete source: pending stays pending,
   accepted artifact becomes a value proposal, absence stays absent and failed or
   cancelled source becomes dependency failure with its original causal error.

The same function applies to input, public-output and loop-next destinations.
Analyzer syntactic exclusivity/warning results never disable actual uniqueness.
Resolver callbacks must read local state and must not send external requests.

## Protection and causes

Guard results retain evaluated influence and protected causal invocation/location
links. Every guard's evaluated labels contribute, including false decisions that
produce absence. Only a uniquely selected source contributes source labels; false
edges neither read source state nor inherit its unread data. Source boundary and
artifact protection must be represented by the coordinator's `LabeledSource`.

`BindingFailure` distinguishes the stable destination failure from an original
selected producer error. In particular, selected cancellation is dependency
failure, not optional absence. Ordered failure vectors make their first element
primary without discarding secondary evidence.

## Node guard and input presence gate

`reduce_admission` resolves the node guard first. Pending, false or error guards do
not inspect even an invalid input inventory. A false guard yields skipped; an error
stays an error. For a true guard it requires exact declared destination coverage,
waits for every binding, selects errors before required absence, and orders primary
input diagnostics by decoded UTF-8 port name. An optional absent port can be ready.

`InputsReady` is only the gate to value validation and preconditions. The coordinator
must authorize/validate selected value transfer, run the relevant contracts, freeze
the complete manifest once, and commit admission before starting work. Public and
next-boundary validation use their scope-specific type/missing/contract precedence.
No pending selection is a frozen manifest entry, and null remains an artifact value.

## Verification and delivery boundary

Eight reducer tests cover late second writers, canonical evaluation order, pending
vs error precedence, inactive source suppression, source states, protected absence,
guard-first admission and all-input error/absence precedence. An independent small
oracle exhausts 64 three-candidate pending/false/true/error combinations and checks
that a source is resolved exactly once only for a uniquely selected settled group.
Malformed inventory and byte limits fail structurally rather than becoming false.

Actual checked-expression contexts, durable guard/binding records, actor wake-ups,
destination value validation and atomic execution publication remain coordinator
integration tasks. Static DAGs will supply wake-up/cycle information rather than
forcing evaluation of lazy branches.
