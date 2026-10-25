# Stateful MCP capability spike

## Outcome

The native adapter preserves one MCP session per run/server across graph nodes.
The fixture spike passes both required sequences through source compilation,
native admission, durable runtime execution, and independent MCP wire peers:

| Transport | Browser: open → inspect the same page | Sandbox: create → execute → inspect → destroy |
| --- | --- | --- |
| stdio | Pass | Pass |
| Streamable HTTP, JSON replies | Pass | Pass |
| Streamable HTTP, SSE replies | Pass | Pass |

The fixture advertises implementation `fixture`, version `1`, and protocol
`2025-11-25`. It simulates page and sandbox state in session-local maps. These
results establish HTLK session continuity; they do not establish compatibility
with a particular browser engine, sandbox product, or third-party MCP server.
No external server was selected for this fixture-first spike.

## Evidence

[`runtime_mcp_sessions.rs`](../tests/runtime_mcp_sessions.rs) compiles the explicit
[`browser.htlk`](../examples/mcp_capability_spike/browser.htlk) and
[`sandbox.htlk`](../examples/mcp_capability_spike/sandbox.htlk) graphs. Tool names,
argument construction, handle transfers, ordering and result assertions are visible
in those graphs. The independent Node
[`stateful_mcp_peer.mjs`](../tests/support/stateful_mcp_peer.mjs) stores objects only
inside the owning stdio process or HTTP MCP session; its audit log is not a state
lookup mechanism.

- Browser open stores a unique marker and returns an opaque page handle. Inspect
  receives that handle and reads the stored marker. A root postcondition checks
  equality with the original input.
- Sandbox create returns an opaque handle. Execute mutates its stored marker;
  inspect reads that mutation. Destroy receives the same handle after inspection,
  removes the object, and returns a checked destruction receipt.
- Each graph runs twice concurrently under the same principal with distinct
  markers. Each run initializes exactly once, uses its own session, and sends
  exactly two or four operations in dependency order.
- A fresh-session negative control cannot inspect another session's page. Unknown
  sandbox handles and inspection after destruction return tool errors. Reopening a
  lease in the original session still finds its page after an idle interval.
- HTTP teardown follows the final operation; sandbox teardown observes zero
  remaining sandboxes. On Unix, stdio teardown checks that the actual server PID
  has exited. Normal completion, terminal failure, cancellation and engine shutdown
  all have cleanup coverage.
- A held request does not block another run's session. Cancellation targets the
  actual RPC. Ownership takeover cancels old access, and stale cleanup leaves the
  new session usable. HTTP expiry and ambiguous timeouts never trigger renewal or
  operation replay.

Reproduce without external credentials (Node.js 24+ and Rust required):

```console
cargo test -p htlk --test runtime_mcp_sessions --locked
cargo test -p htlk-runtime --test mcp_transport --locked
cargo test --example mcp_capability_spike --locked
```

The example smoke test loads a real host configuration and executes the complete
sandbox graph through the runner against the independent stdio peer.

## Opt-in live-server runner

Run a configured sequence with:

```console
cargo run --example mcp_capability_spike -- /path/to/host-config.json
```

The host configuration supplies routing and inputs. Source/catalog paths resolve
relative to the configuration file. For example:

```json
{
  "source": "browser.htlk",
  "catalogs": "live.catalogs.json",
  "inputs": { "marker": "unique-live-page-marker" },
  "endpoints": {
    "browser-deployment": {
      "transport": "streamable_http",
      "url": "http://127.0.0.1:8931/mcp",
      "headers": {}
    }
  }
}
```

For a launched server, replace the endpoint with:

```json
{
  "transport": "stdio",
  "program": "/absolute/path/to/server",
  "arguments": [],
  "environment": {}
}
```

Deployment keys must match catalog server identities. Populate the three catalogs
with the live server's actual implementation/version, protocol transport,
descriptors and schemas. Adapt the example graph's tool selections, argument
records, result projections and postconditions to those exact contracts. For a
browser server, open a controlled page containing the unique marker and inspect
that page's returned handle or same-session active-page state. For a sandbox
server, execute a deterministic file/state mutation and inspect it before destroy.
Keep all sequence steps explicit graph nodes.

HTLK requires protocol `2025-11-25`, an `outputSchema` for every selected tool and
object-valued `structuredContent` for successful results. A live server's missing
schema, text-only result or protocol mismatch is a compatibility finding; the
runner does not synthesize typed output from display text.

The runner uses an in-memory runtime store, a 30-second graph deadline,
10-second attempt/discovery ceilings and a 60-second outer observation bound.
It prints the run ID, executable fingerprint, terminal status and accepted output
values, exits unsuccessfully on failure, then stops execution and awaits native
adapter shutdown. Its host policy allows the explicitly configured spike operations
and grants no replay-safety override. Configuration is literal JSON; credentials
belong in the host's local endpoint headers/environment.

For each live result, record the server product/version, launch command or deployment
reference, negotiated protocol, selected descriptors, executable fingerprint,
sequence outputs, handle continuity and cleanup observation. The fixture table
above should remain distinct from those product-specific results.

## Session lifetime and limits

Sessions are local to one adapter process and ownership generation. Waiting runs
retain their session while locally owned. A restart or ownership transfer creates
no guarantee that previous page/sandbox handles survive. Established-session expiry
or uncertain protocol state fails closed without silently replacing the session.
Only the coordinator's authored retry and trusted delivery/replay rules can permit
another operation attempt.

Requests serialize within a session, so the spike does not claim same-session RPC
multiplexing. Transport teardown is bounded and best-effort; it is separate from
destroying remote application objects. The sandbox graph's explicit destroy node
establishes successful-path cleanup. Failure compensation or recovery of external
objects requires explicit server-supported operations and authored control flow.

See [MCP execution](runtime-mcp.md) for authorization, delivery evidence,
cancellation and durable retry behavior.
