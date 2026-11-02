# HTLK user guide

Learn HTLK by running small workflows and experimenting with their inputs. This
guide starts with the basics of Rust tooling, then walks through six terminal
lessons backed by HTLK's actual compiler and runtime. After the lessons, continue
with [writing workflows](#writing-workflows) and the worked decision-brief example.

## 1. Install Rust

Install Rust using the instructions at [rustup.rs](https://rustup.rs/). Rustup
installs **Rust**, the programming language used by the host application, and
**Cargo**, its build tool and dependency manager. Restart your terminal after
installation so the commands are available.

Check the installation:

```console
rustc --version
cargo --version
```

The repository selects stable Rust through `rust-toolchain.toml`. If you already
have Rust installed, update it with:

```console
rustup update stable
```

## 2. Get the repository and start

If you already have this repository open, run the launch command from its root
directory (the directory containing `Cargo.toml`). Otherwise, install
[Git](https://git-scm.com/downloads), then run:

```console
git clone https://github.com/thomasquintana/htlk.git
cd htlk
cargo run --example interactive_guide --locked
```

The first build downloads dependencies and can take several minutes. Later runs
reuse the compiled files in `target/` and are much faster. The lessons themselves
need no network services, provider credentials, or separately installed database.

What the launch command means:

- `cargo run` builds a Rust program, then runs it.
- `--example interactive_guide` selects the tutorial program in this workspace.
- `--locked` uses the dependency versions recorded in `Cargo.lock`.

HTLK is a library toolkit. The tutorial is a small Rust application that uses it;
you do not need to install a separate `htlk` command.

## 3. Use the menu

```text
Harness Toolkit — Interactive Guide

Choose a lesson (you can start anywhere):
  1. Your first graph
  2. Run a workflow
  3. Add a calculation
  4. Understand errors
  5. Human input
  6. Tools and approvals
```

Start with **1**, then move through the lessons in order. Each lesson displays its
source, explains a concept, and lets you try it.

| Control | Action |
|---|---|
| Enter | Accept the value shown in `[brackets]`. |
| `n` | Go to the next lesson, at a lesson's closing prompt. |
| `r` | Retry the current lesson, at its closing prompt. |
| `m` or `menu` | Return to the menu from any prompt. |
| `q` or `quit` | Exit from any prompt. |
| `h` | Show menu help, or request a hint at lesson 1's question. |

Commands are entered on their own line. The words `m`, `menu`, `q`, and `quit`
are reserved for navigation, including at text-input prompts. Text is trimmed at
both ends. End-of-input also exits: Ctrl-D on macOS/Linux, or Ctrl-Z then Enter
in a Windows console.

Completed lessons show `[done]` in the menu. Progress lasts for the current
session. Every runtime exercise uses a fresh in-memory SQLite store. Returning
to the menu or quitting cancels an unfinished run and stops its supervision.

## 4. Work through the lessons

### Lesson 1: Your first graph

**Goal:** understand inputs, outputs, types, and edges.

The guide compiles [`message.htlk`](../examples/interactive_guide/graphs/message.htlk).
Its input and output both accept `string` (text). An edge forwards the input to
the output; no calculation node is required.

Answer the question about connections. Use `h` for a hint. A successful compile
prints an executable size and a fingerprint. The fingerprint identifies the
executable meaning; it is not a run ID.

**Checkpoint:** compiling checks the graph and emits executable bytes. It does
not execute the workflow.

### Lesson 2: Run a workflow

**Goal:** follow the host's compile/register/start/inspect/read sequence.

Enter a message. The guide compiles the graph, registers its executable, launches
execution supervision, and starts a run with your text as an explicitly typed
input. It waits for success, then reads the output artifact.

**Try:** enter `My first workflow`. You should see that same text after
`read_artifact:`. Use `r` and try another message.

**Checkpoint:** registration checks compatibility with the actual runtime. A run
is one execution of a registered graph. Accepted output values are read through
artifact references.

### Lesson 3: Add a calculation

**Goal:** understand nodes and pure `eval` expressions.

[`compare.htlk`](../examples/interactive_guide/graphs/compare.htlk) declares two
integer inputs and evaluates `inputs.left > inputs.right`. Edges supply the
node's inputs and export its Boolean result.

**Try:** enter `7` and `5`, then predict `true`. Retry with equal numbers: the
result should be `false`. Negative integers work too. The tutorial accepts
integers between -1000000 and 1000000.

HTLK has built-in comparisons and logical expressions. Arithmetic and other
transformations use explicitly linked library functions; `+` is not an infix
operator in HTLK source.

**Checkpoint:** the graph and node each have their own ports. Values reach a node
through explicit edges. `eval` publishes its result as `outputs.value`.

### Lesson 4: Understand errors

**Goal:** distinguish graph errors from invalid actual inputs.

The deliberately broken
[`type_error.htlk`](../examples/interactive_guide/graphs/type_error.htlk) forwards a
string into an integer output. The guide shows the compiler's real diagnostic
code and location, then asks you to choose a compatible output type. The repair
is made in memory for this exercise.

Next, the host attempts to send the integer `42` into the repaired graph's string
input. The runtime rejects it with `E_INPUT`. Enter replacement text to complete
a successful run.

**Checkpoint:** the compiler checks declared connections; the runtime also
validates the values actually supplied. Both errors in this lesson are expected.

### Lesson 5: Human input

**Goal:** pause a workflow and submit a typed reply.

[`question.htlk`](../examples/interactive_guide/graphs/question.htlk) contains a
`wait(string)` node. The guide reads its request artifact and shows its addressed
wait ID. Answer the question; the host calls `submit_reply`, and the graph
publishes your answer.

Actor supervision keeps running while you type. This lesson has a 30-minute graph
deadline; retry the lesson if it expires.

**Checkpoint:** a wait topic helps the host route a question. The authenticated
host context, policy, reply type, and wait ID govern whether a reply is accepted.
A reply supplies data; it does not authorize tool execution.

### Lesson 6: Tools and approvals

**Goal:** understand an MCP tool binding and an exact-action approval.

[`tool.htlk`](../examples/interactive_guide/graphs/tool.htlk) creates a tool argument
object and calls `guide.lookup`, whose descriptor is pinned by the supplied
[`catalogs.json`](../examples/interactive_guide/catalogs.json).

Enter a query and choose:

- **`allow`** — the local scripted adapter runs once and returns a fixed text result.
- **`deny`** — the run fails as expected and the adapter is never invoked.

Use `r` to try the other decision. The result is fixed tutorial data, not live
research. The tutorial supplies separate learner and reviewer identities locally;
an embedding application supplies its own authentication and trusted policy.

**Checkpoint:** approval applies to a specific requested action. It is separate
from a typed input reply and from ordinary graph data.

## 5. Explore the Rust host

The files are organized so you can read one concern at a time:

| File | Purpose |
|---|---|
| [`graphs/`](../examples/interactive_guide/graphs/) | The HTLK programs displayed in each lesson. |
| [`host.rs`](../examples/interactive_guide/host.rs) | Compiler configuration, native registry, host identities/policy, registration, run execution, and artifact reads. |
| [`lessons.rs`](../examples/interactive_guide/lessons.rs) | Explanations and exercise steps that call the host helpers. |
| [`terminal.rs`](../examples/interactive_guide/terminal.rs) | Prompts and input handling; reads terminal input off the asynchronous runtime thread. |
| [`main.rs`](../examples/interactive_guide/main.rs) | The menu, progress tracking, and command-line entry point. |

Some Rust notation you will encounter:

| Notation | Meaning here |
|---|---|
| `let message = ...;` | Bind a value to a name. |
| `let mut session = ...;` | Bind a value that can be updated. |
| `&value` | Borrow a value without transferring its ownership. |
| `&str` | A borrowed piece of text. |
| `Result<T>` | Either a successful value of type `T` or an error. |
| `operation()?` | Return an error to the caller if the operation fails. |
| `async fn` / `.await` | Run asynchronous operations while letting other work make progress. |
| `Arc<T>` | Share ownership of a value, such as the runtime or native registry. |

Start in lesson 2's `workflow` function, then follow the calls into `host.rs`:

```text
Host::compile          -> Compiler::compile_source
Host::session          -> Runtime::register_graph + ExecutionEngine::launch
Session::start         -> Runtime::start_run
Session::terminal      -> Runtime::inspect_run
Session::output        -> Runtime::read_artifact
Session::finish        -> cancel unfinished work + shut down supervision
```

The compiler and runtime share the host-selected policy and native profile. The
tutorial helpers package that configuration so early lessons can focus on graphs.
They are defined in this example; the underlying compiler/runtime methods are
HTLK's library APIs.

### Edit and rerun

Open a `.htlk` file in your editor, change it, then rerun:

```console
cargo run --example interactive_guide --locked
```

The source is included when Cargo builds the example, so restarting the existing
binary alone will not pick up edits. Lesson runners check their expected
semantics: if you change what a graph computes, update the corresponding exercise
in `lessons.rs` too. Keep `type_error.htlk` deliberately broken to use lesson 4.

## Troubleshooting

| Symptom | What to do |
|---|---|
| `cargo: command not found` | Install Rust and restart your terminal. |
| Cargo cannot find `Cargo.toml` | Run the command from the cloned `htlk` directory. |
| The first build appears busy | Wait for dependency compilation to finish; subsequent runs reuse the build. |
| Rust reports a missing linker/C compiler | Install your platform's build tools: Xcode Command Line Tools on macOS, a C/C++ build toolchain on Linux, or the Visual Studio C++ build tools offered by Rustup on Windows. |
| `C_TYPE` / `E_INPUT` in lesson 4 | These are intentional. Follow the repair prompt. |
| A wait or approval expired | Use `r` at the closing prompt to start a fresh run. |
| A lesson fails after source edits | Read the diagnostic and check the displayed source; restore the fixture or update the lesson's expected behavior. |
| You want a quick automated check | Run the scripted walkthrough below. |

Run every lesson, including both approval decisions, without entering answers:

```console
cargo run --example interactive_guide --locked -- --check
```

Cargo's `--` separates its arguments from the tutorial's arguments. This check
exits unsuccessfully if a lesson fails or a required outcome is missing. For usage:

```console
cargo run --example interactive_guide --locked -- --help
```

## Where to go next

- [Writing workflows](#writing-workflows): language patterns and a larger worked example.
- [Compiler guide](../crates/htlk-compiler/README.md): standalone source,
  source bundles, diagnostics, module inspection, and graph joins.
- [Runtime guide](runtime-guide.md): embedding, execution, and durable state.
- [IR syntax reference](../specs/htlk-ir-syntax-reference.md): exact syntax and
  available built-in expressions.
- [Detailed API examples](executable-api.md): executable, compiler, analyzer, and
  runtime APIs.
- [Advanced host example](../examples/runtime_workflow.rs): graph composition,
  live extension, source metadata, and exact-action approval. Run it with
  `cargo run --example runtime_workflow --locked`.

## Glossary

| Term | Meaning |
|---|---|
| Host | Your Rust application that configures and calls HTLK. |
| Graph | A workflow with typed inputs, outputs, nodes, and edges. |
| Port | A named, typed input or output on a graph or node. |
| Node | One unit of work, such as an evaluation, wait, or tool call. |
| Edge | An explicit connection from a source port to a destination port. |
| Executable | The self-contained canonical bytes emitted by the compiler. |
| Fingerprint | A digest identifying an executable's meaning. |
| Run | One execution of a registered graph with supplied inputs. |
| Artifact | An accepted typed value with immutable runtime metadata. |
| MCP catalog | A supplied snapshot of tool/resource/prompt descriptors used during compilation. |
| Host policy | Trusted application rules controlling admission, authorization, and protection. |
| Wait | An addressed request for typed input or an exact-action approval. |

## Writing workflows

Start with the result you need to deliver. Give it a type and acceptance checks,
then identify the inputs and visible operations needed to produce it. Use tasks
to group responsibilities with useful public interfaces. Descriptions help readers
and planning agents understand intent; types, edges, and contracts govern execution.

### A complete first program

This is the same credential-free passthrough graph used by the first lessons:

```htlk
ir_version = "0.1"
graph hello {
    inputs = { message = string }
    outputs = { message = string }
    nodes {}
    edges {
        edge message { from = inputs.message to = outputs.message }
    }
}
```

The graph declares two ports and connects them. It supplies no initial message:
your Rust host provides that when starting a run. A compiler checks the source
and emits executable bytes; registration checks those bytes against the runtime's
actual configuration; starting a run supplies values; execution produces artifacts.
The tutorial's [`host.rs`](../examples/interactive_guide/host.rs) implements that
sequence. For an embedding application, follow the [runtime guide](runtime-guide.md).

### Add a calculation

An edge moves one complete value. To select a field, build an argument object,
render text, or calculate a condition, add an `eval` node. The expression reads
the node's own inputs, which need explicit incoming edges.

```htlk
ir_version = "0.1"
graph compare {
    inputs = { left = integer, right = integer }
    outputs = { greater = boolean }
    nodes {
        compare = eval(boolean, inputs.left > inputs.right) {
            inputs = { left = integer, right = integer }
        }
    }
    edges {
        edge left { from = inputs.left to = compare.inputs.left }
        edge right { from = inputs.right to = compare.inputs.right }
        edge result { from = compare.outputs.value to = outputs.greater }
    }
}
```

`eval(T, expression)` checks that the result fits T; it does not convert it. Native
integers and floats are distinct. Arithmetic and richer transformations use linked
pure libraries. HTLK has no infix `+` and does not execute arbitrary scripts.

### Understand tool results before using them

MCP tools are selected from supplied catalogs. Each call receives one complete
argument object and returns an `McpToolResult`, even for schema-less, text-only,
or multimodal tools. Its important parts are:

| Part | Meaning |
|---|---|
| `protocol_result` | Original admitted protocol content. |
| `extraction` | Literal JSON candidates and the pinned selection decision. |
| Optional `data` | Selected application JSON, if selection succeeded. |
| `validation` | Separate declared, inferred, and application assessments. |

A tool envelope is not its application object. If a review returns an `accepted`
field, it belongs to selected `data`, not `outputs.value.accepted`. Check presence
and narrow the JSON to the application type before using it. A selected JSON null
is present data; missing data is absence. The [MCP guide](mcp-guide.md) shows the
shipped `htlk.mcp-content` helpers and their real manifest selection.

Tools may omit `outputSchema`. When one is advertised, default enforcement checks
original `structuredContent`; JSON recovered from prose cannot establish declared
compliance. `isError: true` fails the invocation even if its text contains usable
diagnostic JSON. Evidence remains available through authorized inspection.

### Put each check at the right boundary

| Mechanism | Use it for |
|---|---|
| Types and schemas | Required representation and fields. |
| `preconditions` | Whether supplied inputs permit a meaningful attempt. |
| `postconditions` | Whether proposed results may be accepted. |
| Node `when` | Whether an operation should run, including its effects. |
| Edge `when` | Whether one source should supply a destination. |
| Loop `until` | Whether the current iteration completes successfully. |

Each contract is one Boolean expression; combine checks with `and`, `or`, and
parentheses. An omitted contract means true. Putting a positive-review requirement
on the reviewer call would make a negative review fail the node. A revision loop
usually needs a successful negative review so it can read feedback and try again.
Put the quality decision in the loop's termination condition instead.

Schemas and persuasive prompts do not prove factual quality. Stronger verification
requires actual checks: resolve citations, compare evidence, execute generated-code
tests, or request an independent review through explicit nodes or linked functions.

### Branch, recover, and distinguish absence

Several conditional edges can target the same destination. All their guards settle;
exactly one true guard selects its source. Two true guards fail with
`E_BINDING_CONFLICT`. The analyzer recognizes simple complements and distinct
literal comparisons, but runtime uniqueness is always checked.

A common fallback uses `when = status(@primary) == "failed"`. Status and error
inspection belongs in allowed guard/contract contexts; an ordinary `eval` cannot
read arbitrary siblings. Route required data through its own ports. Guard an
unavailable output with a lazy status test before reading it.

Keep these states distinct:

- Optional absence: no value was supplied; `present` can test it.
- Present null: an actual value allowed by a nullable type.
- Pending: a producer has not settled yet.
- Failure: a selected producer has no ordinary successful output.

`present` does not hide failed producers. A false edge does not suppress its
producer's effects; use the producer's node guard for that. Scopes settle all
instantiated children. A failed child can lead to a successful fallback; a required
unexported effect needs an explicit completion postcondition.

### Study the decision-brief workflow

Open [`decision_brief.htlk`](../specs/examples/decision_brief.htlk), then its
[catalogs and behavior notes](../specs/examples/README.md). The tools in those
catalogs are illustrative contracts, with scripted implementations in the tests.
They are not bundled research/model services.

The program performs these steps:

1. Render two research queries from the input question.
2. Run two occurrences of the reusable research task.
3. Combine their checked evidence strings.
4. Enter a loop with an empty prior draft and feedback.
5. Draft and review; carry the new draft and feedback into the next iteration.
6. Publish an accepted brief, or an explicit `needs_attention` outcome after failure.

The loop runs at most three iterations. Acceptance on reviews one, two, and three
uses 4, 6, and 8 calls respectively. Three negative reviews reach `E_LOOP_LIMIT`
and the ordinary fallback. A successfully reported `needs_attention` result is a
runtime success, but it is not an accepted recommendation. Empty input,
cancellation, or an enclosing deadline can stop the run without fallback output.

Each iteration has fresh values and provenance. Only committed `next` values feed
the next iteration. Both next values and proposed outputs are checked even on the
final iteration; a true final `until` succeeds. Retrying an MCP request repeats frozen
arguments; revising a draft is a new iteration with new arguments. Neither retries
nor a large call budget replaces the explicit loop bound.

To run the controlled implementation scenarios from the repository root:

```console
cargo test --test runtime_mcp decision_brief_ --locked
cargo test --test compiler source_output_is_admitted_under_exact_native_configuration --locked
```

Install Node.js 24 to run the full workspace suite, which also starts independent
local MCP peers. See [testing](testing.md) for the complete check commands.

### Reuse tasks and split source files

A task exposes public ports and hides internal scope names. Each `use(task)` creates
an independent occurrence. Start with the [five-file example](../specs/examples/modules/README.md):
entry graph, shared type, prompt templates, research task, and refinement task.

Imports select explicitly exported task/type/prompt definitions from exact source
snapshots. They do not execute code, fetch dependencies, discover services, or share
mutable state. Your host collects the files into a source bundle. The compiler
verifies actual definitions, even if an authoring agent saw only generated interfaces.

`Compiler::inspect_module` returns typed Rust export summaries. A host can first
request an index and then selected summaries, render those for an author or agent,
and compile the proposed caller against the actual source. See the
[compiler API](../crates/htlk-compiler/README.md) for a concrete invocation. Reports
must match current source pins, catalogs, and profile; they are not executable stubs.

### Compose and extend deliberately

Compiler joins combine two checked graphs through public whole ports. Any required
field extraction or conversion belongs inside an operand. The joined result is a
new executable containing ordinary task scopes.

Live extension is stricter: existing work and inputs must remain unchanged. Keep
the run open with a typed planning wait, compile/register an additive composition,
then install it and close the wait atomically. See the runnable
[`runtime_workflow`](../examples/runtime_workflow.rs) example and
[extension guide](runtime-guide.md#extend-a-running-workflow).

For first-result racing, early quorums, or automatic competitor cancellation, use
an explicit arbiter service or host integration. Ordinary sibling nodes settle
together. Keep search bounded, isolate effects behind useful task interfaces, and
make repair and compensation visible in the graph.
