#!/usr/bin/env python3
"""Verify exact campaign-owned resources are absent; never stop or remove anything."""

import argparse
import json
from pathlib import Path
import subprocess

from live_process_cleanup import process_table
from live_sandbox import LABEL, docker
from live_support import require, write_json


def audit(campaign):
    projects, markers = set(), set()
    for path in campaign.glob("context*/cleanup.json"):
        projects.add(json.loads(path.read_text())["project"])
    for path in campaign.glob("sandbox*.report.json"):
        report = json.loads(path.read_text())
        if "marker" in report:
            markers.add(report["marker"])
    for project in projects:
        label = "label=com.docker.compose.project=" + project
        require(
            not docker(["ps", "-aq", "--filter", label]).stdout.strip(),
            "owned compose container remains",
        )
        require(
            not docker(["volume", "ls", "-q", "--filter", label]).stdout.strip(),
            "owned compose volume remains",
        )
        require(
            not docker(["network", "ls", "-q", "--filter", label]).stdout.strip(),
            "owned compose network remains",
        )
    for marker in markers:
        require(
            not docker(
                ["ps", "-aq", "--filter", f"label={LABEL}={marker}"]
            ).stdout.strip(),
            "owned sandbox remains",
        )
    owned = []
    chrome_main = 0
    for pid, (_, command) in process_table().items():
        if command.endswith("/Google Chrome"):
            chrome_main += 1
        if any(
            name in command
            for name in [
                "mcp_live_session",
                "context-mcp",
                "llm-mcp",
                "mcp-sandbox",
                "node",
            ]
        ):
            arguments = subprocess.run(
                ["ps", "-p", str(pid), "-o", "args="],
                text=True,
                capture_output=True,
                check=False,
                timeout=5,
            ).stdout
            if str(campaign) + "/" in arguments:
                owned.append(pid)
    require(not owned, "campaign-owned process remains")
    return {
        "compose_projects_absent": sorted(projects),
        "sandbox_markers_absent": sorted(markers),
        "campaign_processes_remaining": owned,
        "chrome_main_process_count": chrome_main,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    result = audit(args.campaign.resolve())
    if args.output:
        require(not args.output.exists(), "refuse to overwrite cleanup audit")
        write_json(args.output, result)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
