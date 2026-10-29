#!/usr/bin/env python3
"""Actual HTTP discovery and representative calls using the same HTLK runner."""

import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request
from live_support import ROOT, Session, require, reserve, write_json


def rpc_message(body, mime, sequence):
    require(len(body) <= 4 * 1024 * 1024, "discovery response exceeded bound")
    if "text/event-stream" in mime:
        events = body.decode().replace("\r\n", "\n").replace("\r", "\n").split("\n\n")
        data = [
            "\n".join(
                line[5:].lstrip(" ")
                for line in event.splitlines()
                if line.startswith("data:")
            )
            for event in events
        ]
        messages = [json.loads(value) for value in data if value.strip()]
        packet = next(message for message in messages if message.get("id") == sequence)
    else:
        packet = json.loads(body)
    require(
        packet.get("jsonrpc") == "2.0"
        and packet.get("id") == sequence
        and "error" not in packet,
        "invalid discovery RPC",
    )
    return packet["result"]


def free_port():
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return server.getsockname()[1]


def discover(host, output, headers=None):
    """Bounded initialize/list capture; the native adapter rechecks raw wire/pins."""
    session_id = None
    sequence = 0

    def exchange(method, params=None, notification=False):
        nonlocal session_id, sequence
        sequence += 1
        packet = {"jsonrpc": "2.0", "method": method}
        if not notification:
            packet["id"] = sequence
        if params is not None:
            packet["params"] = params
        request_headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "MCP-Protocol-Version": "2025-11-25",
            **(headers or {}),
        }
        if session_id:
            request_headers["Mcp-Session-Id"] = session_id
        request = urllib.request.Request(
            host["url"], data=json.dumps(packet).encode(), headers=request_headers
        )
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read(4 * 1024 * 1024 + 1)
            require(len(body) <= 4 * 1024 * 1024, "discovery response exceeded bound")
            session_id = response.headers.get("Mcp-Session-Id", session_id)
            if notification:
                return None
            return rpc_message(body, response.headers.get("Content-Type", ""), sequence)

    try:
        initialized = exchange(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "htlk-live-http-discovery", "version": "1"},
            },
        )
        require(
            initialized["protocolVersion"] == "2025-11-25",
            "HTTP protocol negotiation mismatch",
        )
        exchange("notifications/initialized", notification=True)
        tools, cursors = [], set()
        cursor = None
        for _ in range(128):
            page = exchange("tools/list", {} if cursor is None else {"cursor": cursor})
            tools.extend(page["tools"])
            require(len(tools) <= 4096, "HTTP tool inventory exceeded bound")
            cursor = page.get("nextCursor")
            if cursor is None:
                break
            require(cursor not in cursors, "HTTP repeated discovery cursor")
            cursors.add(cursor)
        else:
            raise AssertionError("HTTP discovery page allowance exhausted")
        servers = {
            host["alias"]: {
                "deployment_id": host["deployment_id"],
                "transport": "streamable_http",
                "implementation_name": initialized["serverInfo"]["name"],
                "implementation_version": initialized["serverInfo"]["version"],
            }
        }
        common = {
            "catalog_version": "0.1",
            "mcp_protocol_version": "2025-11-25",
            "servers": servers,
            "schema_documents": {},
        }
        write_json(
            output,
            {
                "tools": {
                    **common,
                    "tools": [
                        {"server": host["alias"], "descriptor": tool} for tool in tools
                    ],
                },
                "resources": {**common, "resources": [], "templates": []},
                "prompts": {**common, "prompts": []},
            },
        )
        write_json(
            output.with_suffix(".discovery.json"),
            {
                "serverInfo": initialized["serverInfo"],
                "protocol": initialized["protocolVersion"],
                "tools": len(tools),
                "tools_call_count": 0,
            },
        )
    finally:
        if session_id:
            request = urllib.request.Request(
                host["url"],
                method="DELETE",
                headers={
                    "Mcp-Session-Id": session_id,
                    "MCP-Protocol-Version": "2025-11-25",
                    **(headers or {}),
                },
            )
            try:
                urllib.request.urlopen(request, timeout=5).close()
            except urllib.error.HTTPError:
                pass


def smoke(campaign, server, alias):
    session = Session(campaign, alias, alias)
    report = {"successful_tools": []}
    try:
        if server == "llm":
            reserve(campaign, "openai", "llm-http/generate", 0.20)
            result = session.data(
                "generate",
                {
                    "model": "gpt-4.1-mini",
                    "input": [{"role": "user", "content": "Reply only ORCHID."}],
                    "store": False,
                    "max_output_tokens": 32,
                },
            )
            require("ORCHID" in json.dumps(result), "HTTP generation marker absent")
            report["successful_tools"].append("generate")
            error = session.call(
                "generate",
                {
                    "model": "gpt-4.1-mini",
                    "input": [
                        {
                            "role": "user",
                            "content": [
                                {
                                    "type": "input_image",
                                    "image_url": "https://example.invalid/blocked.png",
                                }
                            ],
                        }
                    ],
                    "store": False,
                },
                success=False,
            )
        else:
            for tool, arguments in [
                ("brave_image_search", {"query": "Saturn planet", "count": 1}),
                ("brave_video_search", {"query": "NASA moon landing", "count": 1}),
            ]:
                reserve(campaign, "brave", f"brave-http/{tool}", 0.05)
                result = session.call(tool, arguments)
                require(
                    bool(result["protocol_result"]["content"]),
                    "HTTP search content absent",
                )
                report["successful_tools"].append(tool)
            reserve(
                campaign, "brave", "brave-http/explicit-invalid-summary-key", 0.50, 20
            )
            rejected = session.call(
                "brave_summarizer", {"key": "HTLK_EXPLICIT_INVALID_KEY"}, success=False
            )
            report["summary_negative_case"] = rejected["failure_code"]
            error = session.call(
                "brave_web_search", {"query": " ".join(["x"] * 51)}, success=False
            )
        report["local_error"] = error["failure_code"]
        session.close()
    finally:
        if not session.closed:
            session.kill()
        write_json(campaign / f"{alias}.report.json", report)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("server", choices=["llm", "brave"])
    parser.add_argument("--attempt", default="one")
    args = parser.parse_args()
    campaign, server = args.campaign.resolve(), args.server
    original = json.loads((campaign / f"{server}.host.json").read_text())
    port = free_port()
    alias = server + "-http"
    if args.attempt != "one":
        alias += "-" + args.attempt
    host = {
        "alias": alias,
        "deployment_id": "live-" + alias,
        "transport": "streamable_http",
        "url": f"http://127.0.0.1:{port}/mcp",
        "headers": {},
        "max_calls": 12,
    }
    write_json(campaign / f"{alias}.host.json", host)
    environment = {**os.environ, **original.get("environment", {})}
    if server == "llm":
        command = [original["program"], "serve"]
        environment.update(LLM_MCP_TRANSPORT="http", LLM_MCP_BIND=f"127.0.0.1:{port}")
        health = f"http://127.0.0.1:{port}/health/ready"
    else:
        command = [
            original["program"],
            original["arguments"][0],
            "--transport",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--stateless",
            "false",
        ]
        health = f"http://127.0.0.1:{port}/ping"
    log = os.fdopen(
        os.open(
            campaign / f"{alias}.server.stderr",
            os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            0o600,
        ),
        "w",
    )
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        env=environment,
        stdout=log,
        stderr=log,
        start_new_session=True,
    )
    try:
        for _ in range(100):
            try:
                with urllib.request.urlopen(health, timeout=1) as response:
                    if response.status == 200:
                        break
            except (OSError, urllib.error.HTTPError):
                time.sleep(0.1)
        else:
            raise AssertionError("owned HTTP server did not become ready")
        discover(host, campaign / f"{alias}.catalogs.json")
        smoke(campaign, server, alias)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait(timeout=5)
        log.close()
    print(json.dumps({"server": server, "transport": "HTTP", "cleanup": True}))


if __name__ == "__main__":
    main()
