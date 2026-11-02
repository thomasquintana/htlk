#!/usr/bin/env python3
"""Actual file/performance Chrome cases with a narrowly scoped operator root."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import time
import uuid
from live_chrome import display, fixture_server
from live_support import ROOT, Session, require, write_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--attempt", default="one")
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    alias = "chrome-files-" + args.attempt
    files = campaign / "browser-files" / alias
    files.mkdir(mode=0o700)
    marker = "HTLK_FILES_" + uuid.uuid4().hex
    upload = files / "synthetic.txt"
    upload.write_text(marker)
    host = json.loads((campaign / "chrome.host.json").read_text())
    host.update(alias=alias, deployment_id="live-" + alias, max_calls=24)
    host["selected_tools"] = [
        "new_page",
        "close_page",
        "take_snapshot",
        "evaluate_script",
        "upload_file",
        "take_heapsnapshot",
        "screencast_start",
        "screencast_stop",
        "performance_start_trace",
        "performance_stop_trace",
        "performance_analyze_insight",
        "lighthouse_audit",
    ]
    host["arguments"].append("--workspace=" + str(files))
    write_json(campaign / f"{alias}.host.json", host)
    subprocess.run(
        [
            "node",
            "tools/discover-mcp.mjs",
            str(campaign / f"{alias}.host.json"),
            str(campaign / f"{alias}.catalogs.json"),
        ],
        cwd=ROOT,
        check=True,
        timeout=60,
    )
    server = fixture_server(marker)
    report = {"tools": {}, "files": {}, "cleanup": False}
    session = None
    page_id = None

    def call(tool, arguments):
        result = session.call(tool, arguments, success=None)
        if result.get("event") == "result":
            report["tools"][tool] = {
                "status": "failed",
                "failure_code": result["failure_code"],
            }
            return None
        report["tools"][tool] = {"status": "passed"}
        return result

    try:
        session = Session(campaign, alias, alias)
        opened = display(
            call("new_page", {"url": f"http://127.0.0.1:{server.server_port}/"})
        )
        page_id = int(re.search(r"^(\d+):.*\[selected\]", opened, re.MULTILINE)[1])
        snapshot = display(call("take_snapshot", {}))
        uid = re.search(r'uid=(\S+) button "Upload file"', snapshot)[1]
        call("upload_file", {"uid": uid, "filePaths": [str(upload)]})
        uploaded = call(
            "evaluate_script",
            {
                "function": "() => ({name: document.getElementById('upload').files[0].name})",
                "waitForStableDom": False,
            },
        )
        require(
            uploaded["data"]["name"] == upload.name, "actual upload filename mismatch"
        )
        call("take_heapsnapshot", {"filePath": str(files / "page.heapsnapshot")})
        call("screencast_start", {"filePath": str(files / "page.mp4")})
        call(
            "evaluate_script",
            {
                "function": "() => { document.body.style.background='pink'; return true; }",
                "waitForStableDom": False,
            },
        )
        time.sleep(0.5)
        call("screencast_stop", {})
        call("performance_start_trace", {"reload": True, "autoStop": False})
        time.sleep(0.5)
        trace = display(
            call("performance_stop_trace", {"filePath": str(files / "trace.json.gz")})
        )
        (files / "trace-summary.txt").write_text(trace)
        candidates = re.findall(r"insightSetId[:=]\s*[`\"']?([^\s`\"']+)", trace)
        if candidates:
            call(
                "performance_analyze_insight",
                {"insightSetId": candidates[0], "insightName": "DocumentLatency"},
            )
        else:
            report["tools"]["performance_analyze_insight"] = {
                "status": "blocked",
                "reason": "inspect saved actual trace-set identifiers",
            }
        call(
            "lighthouse_audit",
            {
                "mode": "snapshot",
                "device": "desktop",
                "outputDirPath": str(files / "audit"),
            },
        )
        call("close_page", {"pageId": page_id})
        page_id = None
        session.close()
        session = None
        for path in files.rglob("*"):
            if path.is_file():
                digest = hashlib.file_digest(path.open("rb"), "sha256").hexdigest()
                report["files"][str(path.relative_to(files))] = {
                    "bytes": path.stat().st_size,
                    "sha256": digest,
                }
        report["cleanup"] = True
    finally:
        if session:
            try:
                if page_id is not None:
                    session.call("close_page", {"pageId": page_id}, success=None)
                session.close()
                report["cleanup"] = True
            except (OSError, AssertionError):
                session.kill()
        server.shutdown()
        server.server_close()
        write_json(campaign / f"{alias}.report.json", report)
    print(json.dumps({"server": "chrome", "phase": "files", **report}))


if __name__ == "__main__":
    main()
