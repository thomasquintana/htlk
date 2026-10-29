#!/usr/bin/env python3
"""Create a fresh private campaign directory and secret-free endpoint files."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OSS = ROOT.parent


def write_json(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
    path.chmod(0o600)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    directory = args.directory.resolve()
    directory.mkdir(mode=0o700)
    for child in ["sandbox-files", "profiles", "browser-files", "docker"]:
        (directory / child).mkdir(mode=0o700)
    (directory / "sandbox-files" / "fixture.txt").write_text("HTLK synthetic fixture\n")
    (directory / "sandbox-files").chmod(0o755)
    profile = directory / "profiles/acceptance-bounded.yml"
    profile.write_text((ROOT / "tools/fixtures/live_sandbox.yml").read_text())
    profile.chmod(0o444)
    docker_host = subprocess.run(
        ["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    ).stdout.strip()
    docker = Path(shutil.which("docker")).resolve()
    plugins = docker.parent.parent / "cli-plugins"
    write_json(
        directory / "docker/config.json",
        {
            "auths": {},
            "cliPluginsExtraDirs": [str(plugins)] if plugins.is_dir() else [],
        },
    )
    endpoints = {
        "sandbox": {
            "program": str(
                OSS / "mcp-sandbox/dist/mcp-sandbox_darwin_arm64_v8.0/mcp-sandbox"
            ),
            "arguments": [],
            "environment": {
                "ALLOW_DIRS": str(directory / "sandbox-files"),
                "SANDBOX_PROFILES_DIR": str(directory / "profiles"),
                "DOCKER_HOST": docker_host,
            },
        },
        "llm": {
            "program": str(OSS / "llm-mcp/target/release/llm-mcp"),
            "arguments": ["serve"],
            "environment": {
                "LLM_MCP_AUTH": "api_key",
                "LLM_MCP_TRANSPORT": "stdio",
                "LLM_MCP_MAX_CONCURRENCY": "1",
            },
            "inherit_environment": ["OPENAI_API_KEY"],
        },
        "brave": {
            "program": "/opt/homebrew/bin/node",
            "arguments": [
                str(OSS / "brave-search-mcp-server/dist/index.js"),
                "--transport",
                "stdio",
            ],
            "environment": {},
            "inherit_environment": ["BRAVE_API_KEY"],
        },
        "chrome": {
            "program": "/opt/homebrew/bin/node",
            "arguments": [
                str(OSS / "chrome-devtools-mcp/build/src/bin/chrome-devtools-mcp.js"),
                "--headless",
                "--isolated",
                "--no-page-id-routing",
                "--viewport=800x600",
                "--executablePath=/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "--no-usage-statistics",
                "--no-performance-crux",
            ],
            "environment": {
                "CHROME_DEVTOOLS_MCP_NO_UPDATE_CHECKS": "1",
                "CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS": "1",
            },
        },
    }
    for alias, endpoint in endpoints.items():
        write_json(
            directory / f"{alias}.host.json",
            {
                "alias": alias,
                "deployment_id": f"live-{alias}",
                "transport": "stdio",
                "max_calls": 150,
                "attempt_timeout_ms": 180000,
                **endpoint,
            },
        )
    write_json(
        directory / "budget.json",
        {
            "limit_usd": 25,
            "reserved_usd": 0,
            "provider_requests": 0,
            "entries": [],
            "pricing_sources": [
                "https://developers.openai.com/api/docs/pricing",
                "https://brave.com/search/api/",
            ],
        },
    )
    print(json.dumps({"campaign": str(directory), "paid_calls": 0}))


if __name__ == "__main__":
    main()
