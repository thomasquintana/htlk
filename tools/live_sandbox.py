#!/usr/bin/env python3
"""Exercise every actual sandbox tool through one ordinary HTLK run."""

import argparse
import json
from pathlib import Path
import subprocess
import uuid
from live_support import Session, require, write_json

LABEL = "dev.teran.sandbox.opencode_session"
PROFILE = "acceptance-bounded"


def docker(arguments, *, check=True):
    return subprocess.run(
        ["docker", *arguments], text=True, capture_output=True, timeout=30, check=check
    )


def owned_container(identifier, marker):
    physical = identifier.removeprefix("docker:")
    result = docker(
        ["container", "inspect", physical, "--format", "{{json .Config.Labels}}"],
        check=False,
    )
    if result.returncode:
        return False
    require(
        json.loads(result.stdout).get(LABEL) == marker, "container ownership mismatch"
    )
    return True


def cleanup(marker):
    ids = docker(["ps", "-aq", "--filter", f"label={LABEL}={marker}"]).stdout.split()
    for identifier in ids:
        require(
            owned_container(identifier, marker), "orphan ownership verification failed"
        )
        docker(["rm", "--force", "--volumes", identifier])
    require(
        not docker(["ps", "-aq", "--filter", f"label={LABEL}={marker}"]).stdout.strip(),
        "owned container survived cleanup",
    )
    return ids


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--attempt", default="one")
    parser.add_argument("--loss-only", action="store_true")
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    cwd = campaign / "sandbox-files"
    cwd.chmod(0o755)
    (campaign / f"profiles/{PROFILE}.yml").chmod(0o444)
    marker = "htlk-live-" + uuid.uuid4().hex
    session = None
    created = []
    report = {"marker": marker, "tools": [], "cleanup": False}
    try:
        if not args.loss_only:
            normal_campaign(campaign, cwd, marker, created, report, args.attempt)
        session = Session(campaign, "sandbox", f"sandbox-loss-{args.attempt}")
        lost = session.data(
            "sandbox_create",
            {"profile": PROFILE, "cwd": str(cwd), "session_id": marker},
        )["id"]
        report["provider_termination"] = "native adapter shutdown after create"
        session.close()
        session = None
        session = Session(campaign, "sandbox", f"sandbox-restarted-{args.attempt}")
        require(
            not session.data("sandbox_list", {})["sessions"],
            "restarted provider recovered unexpected in-memory state",
        )
        session.call("sandbox_info", {"id": lost}, success=False)
        receipt = session.data("sandbox_destroy", {"id": lost})
        report["restart_destroy_receipt"] = receipt
        report["container_survived_provider_restart"] = owned_container(lost, marker)
        session.close()
        session = None
    finally:
        if session:
            session.kill()
        report["harness_removed_owned_orphans"] = cleanup(marker)
        report["cleanup"] = True
        write_json(campaign / f"sandbox-{args.attempt}.report.json", report)
    print(
        json.dumps(
            {
                "server": "sandbox",
                "successful_tools": len(report["tools"]),
                "cleanup": report["cleanup"],
            }
        )
    )


def normal_campaign(campaign, cwd, marker, created, report, attempt):
    session = Session(campaign, "sandbox", f"sandbox-{attempt}")
    try:
        profiles = session.data("profile_list", {})
        require("acceptance" in json.dumps(profiles), "operator profile absent")
        require(
            not session.data("sandbox_list", {})["sessions"],
            "fresh sandbox process is not empty",
        )
        for _ in range(3):
            identifier = session.data(
                "sandbox_create",
                {"profile": PROFILE, "cwd": str(cwd), "session_id": marker},
            )["id"]
            require(identifier.startswith("docker:"), "unexpected backend identifier")
            require(owned_container(identifier, marker), "created container absent")
            created.append(identifier)
        first = created[0]
        info = session.data("sandbox_info", {"id": first})
        require(
            info["id"] == first and info["working_dir"] == str(cwd),
            "sandbox info mismatch",
        )
        require(
            session.data("sandbox_stats", {"id": first})["id"] == first,
            "stats owner mismatch",
        )
        result = session.data(
            "sandbox_exec",
            {
                "id": first,
                "command": "printf 'HTLK_MARKER'; printf 'HTLK_STDERR' >&2; printf retained > /tmp/htlk-state",
                "timeout": 10,
                "summary": "write synthetic markers",
            },
        )
        require(
            result["exit_code"] == 0
            and "HTLK_MARKER" in result["stdout"]
            and "HTLK_STDERR" in result["stderr"],
            "exec output mismatch",
        )
        result = session.data(
            "sandbox_exec",
            {
                "id": first,
                "command": "cat /tmp/htlk-state; exit 7",
                "timeout": 10,
                "summary": "read retained state",
            },
        )
        require(
            result["exit_code"] == 7 and "retained" in result["stdout"],
            "nonzero exit or same-session state mismatch",
        )
        session.call(
            "sandbox_exec",
            {"id": first, "command": "id", "user": "root", "timeout": 5},
            success=False,
        )
        session.call(
            "sandbox_create",
            {"profile": "missing-profile", "cwd": str(cwd), "session_id": marker},
            success=False,
        )
        session.data("sandbox_destroy", {"id": first})
        require(
            not owned_container(first, marker),
            "destroy receipt did not remove container",
        )
        session.data("sandbox_destroy", {"id": first})
        destroyed = session.data("sandbox_destroy_all", {})
        require(
            destroyed["destroyed"] == 2,
            "destroy-all did not remove the two owned survivors",
        )
        require(
            not session.data("sandbox_list", {})["sessions"],
            "sandbox process retained sessions",
        )
        require(
            all(not owned_container(identifier, marker) for identifier in created),
            "physical sandbox remained",
        )
        session.close()
        session = None
        report["tools"] = [
            "profile_list",
            "sandbox_list",
            "sandbox_create",
            "sandbox_info",
            "sandbox_stats",
            "sandbox_exec",
            "sandbox_destroy",
            "sandbox_destroy_all",
        ]
    finally:
        if session:
            session.kill()


if __name__ == "__main__":
    main()
