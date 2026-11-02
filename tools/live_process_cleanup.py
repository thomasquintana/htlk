#!/usr/bin/env python3
"""Reap only live-session process trees whose argv identifies this campaign."""

import argparse
import os
from pathlib import Path
import signal
import subprocess
import time


def process_table():
    output = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,comm="], text=True, capture_output=True, check=True
    ).stdout
    rows = {}
    for line in output.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) == 3:
            rows[int(parts[0])] = (int(parts[1]), parts[2])
    return rows


def descendants(root):
    table = process_table()
    owned = []
    pending = [root]
    while pending:
        parent = pending.pop(0)
        children = [pid for pid, (owner, _) in table.items() if owner == parent]
        pending.extend(children)
        owned.extend((pid, table[pid][1]) for pid in children)
    return owned


def terminate_known(owned):
    table = process_table()
    signaled = []
    for pid, command in reversed(owned):
        if pid in table and table[pid][1] == command:
            try:
                os.kill(pid, signal.SIGTERM)
                signaled.append(pid)
            except ProcessLookupError:
                pass
    return signaled


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    args = parser.parse_args()
    campaign = str(args.campaign.resolve()) + "/"
    selected = []
    for pid, (_, command) in process_table().items():
        if command.endswith("/htlk/target/debug/examples/mcp_live_session"):
            actual = subprocess.run(
                ["ps", "-p", str(pid), "-o", "args="],
                text=True,
                capture_output=True,
                check=False,
            ).stdout.strip()
            if campaign in actual:
                owned = descendants(pid)
                selected.extend(terminate_known(owned))
                selected.extend(terminate_known([(pid, command)]))
    time.sleep(0.2)
    print({"campaign_owned_processes_signaled": selected})


if __name__ == "__main__":
    main()
