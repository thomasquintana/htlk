# Interactive user guide

Learn HTLK by running small workflows and experimenting with their inputs. This
guide starts with the basics of Rust tooling, then walks through six terminal
lessons backed by HTLK's actual compiler and runtime.

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

- [HTLK IR user guide](../specs/htlk-ir-user-guide.md): deeper language examples,
  workflow patterns, and a larger worked example.
- [Compiler guide](../crates/htlk-compiler/README.md): standalone source,
  source bundles, diagnostics, module inspection, and graph joins.
- [Runtime guide](../crates/htlk-runtime/README.md): embedding the runtime and its
  public execution boundary.
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
