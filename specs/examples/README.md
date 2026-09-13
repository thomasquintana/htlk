# Decision-brief example

This directory accompanies the [HTLK IR user guide](../htlk-ir-user-guide.md) and targets **HTLK IR Draft 0.1**.

## What this example package is

A **fixture** is prepared example data used to explain or test a program; it is not a report from a live service. The IR file describes the work. The catalogs describe which external operations the compiler is allowed to resolve. The input file supplies the initial question for one run.

**JSON** is a text format for named fields and lists. A **schema** describes which JSON values an operation accepts or returns. **MCP** means Model Context Protocol, the interface used to request work from connected services. A **descriptor** is a catalog entry describing one such operation. The **host** is the application that submits inputs to the runtime.

The [user guide](../htlk-ir-user-guide.md) introduces nodes, tasks, ports, and conditions before walking through this example.

## Files

| File | Purpose |
|---|---|
| [decision_brief.htlk](decision_brief.htlk) | Complete source document with all type, prompt, task, and graph declarations. |
| [decision_brief.catalogs.json](decision_brief.catalogs.json) | Illustrative three-catalog compiler input with exact tool schemas. |
| [decision_brief.inputs.json](decision_brief.inputs.json) | Tagged host input map for `start_run`. |

The catalogs are fixtures, not discovery results from live servers. They contain no endpoints, credentials, model configuration, or implementation of the tools. To execute the graph, provide a conforming compiler/runtime and services matching the descriptors, or replace the fixture catalogs with actual discovered descriptors and adapt/recompile the IR.

There is no assumed `htlk run` command or released SDK. Use the logical host interfaces in the [runtime specification](../runtime-spec.md).

## Multi-file version

The [modular example](modules/README.md) divides this same workflow into five source files and three source packages. It uses these same tool catalogs and run inputs. All HTLK-owned formats in both examples use the unified 0.1 baseline.

## Tool behavior required by the example

The research tool receives a query and returns nonempty evidence text. Its implementation should identify sources and uncertainty. The schemas check representation, not the existence or reliability of those sources.

The revision tool receives question, evidence, prior draft, and feedback. An empty prior draft means create the first draft. Subsequent calls should use the previous draft and review feedback.

The review tool receives question, evidence, and draft. It should assess the rubric described in the user guide. A valid response with `accepted: false` is a successful tool response and permits another iteration. A response with `isError: true`, missing structured content, or invalid output data is an operational failure instead.

Every successful tool response supplies object-valued `structuredContent` matching its `outputSchema`. Protocol text blocks, if provided, do not replace that structured result.

## Bounds and assumptions

There are two research calls, then at most three pairs of revision and review calls. The source contains no automatic MCP retries.

| Case | MCP dispatches if all required operations complete normally |
|---|---|
| First review accepts | 4 |
| Second review accepts | 6 |
| Third review accepts or rejects | 8 |

The root permits at most eight calls; the refinement loop permits six. These are ceilings, not promises of available capacity. Tighter ancestor or deployment limits can stop work earlier. The selected policy must provide finite deadlines, adequate value/expression limits, and supported execution capabilities.

By source inspection, the graph has a maximum nesting depth of three scopes and an upper bound of 34 invocation occurrences, counting all guarded branches and three loop iterations under the compiler specification's counting rules. The configured structural limits must accommodate that bound. These counts have not been confirmed by an implemented HTLK compiler.

Model selection and sampling settings are deployment details of these illustrative tools. They are not special IR node options in this example.

## Expected behavior tests

These are integration-test requirements, not claims of executed runtime tests. Use controlled tool responses before testing with a live model.

| Scenario | Expected behavior |
|---|---|
| Research succeeds; first review accepts | Root succeeds with `result.status = "accepted"` and the reviewed draft. |
| First review rejects; second accepts | The second revision receives the first draft and feedback; only the accepted final draft becomes the loop's public output. |
| All three reviews reject | Loop fails with `E_LOOP_LIMIT`; the root normally succeeds with `result.status = "needs_attention"` and `brief = null`. |
| One retrieval fails | No valid combined evidence is produced; refinement fails its dependency path; the attention branch can complete. The other research branch is still settled. |
| Revision returns malformed structured output | Tool output validation fails; the ordinary failure path reaches the attention branch. |
| Review returns malformed structured output | It is not treated as a negative editorial review; the refinement path fails. |
| Review returns valid `accepted: false` with feedback | The loop advances if another iteration is allowed. |
| Review repeatedly rejects with unhelpful or empty feedback | The schema alone cannot guarantee improvement; the explicit loop bound still prevents unlimited attempts. |
| Root question is empty | Root precondition fails before child execution; no attention result is promised. |
| Host cancels during a model call | Cancellation follows runtime fencing rules; no successful fallback completion is promised. |
| Enclosing deadline or hard budget is exhausted | Runtime stop rules apply; a needs-attention result is not guaranteed after the enclosing scope has stopped. |
| Crash after a response is durably recorded | Recovery resumes local validation/acceptance without redispatching that recorded response. |
| Remote delivery is uncertain and replay is unsafe | Preserve uncertainty; do not blindly repeat the operation. |
| Repeated attempts to extend a terminal run | Reject live extension; start a new run with explicitly authorized artifact reuse if needed. |

Test the distinction between runtime success and application success: a successfully reported `needs_attention` outcome is not a successful recommendation.

## Validation provided here

The checker is a small JavaScript program. **Node.js** is the program used to execute it; the following command assumes Node.js is already installed. A **terminal** is an application where such commands are entered. Checking the documents does not run the HTLK workflow.

Run `node ../validate-specs.mjs` from this directory, or `node validate-specs.mjs` from the release directory.

The document checker parses the guide's HTLK blocks and this complete source against the EBNF, checks that the complete source matches the guide excerpts, verifies local links and diagram captions, parses JSON examples, and checks fixture tool references. It does not compile/type-check HTLK, contact MCP servers, execute the workflow, or prove behavioral conformance.
