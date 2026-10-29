#!/usr/bin/env python3
"""Real recorded video/trace analysis using the server's documented ffmpeg mode."""

import argparse
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
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    alias = "chrome-recording"
    files = campaign / "browser-files" / alias
    files.mkdir(mode=0o700)
    host = json.loads((campaign / "chrome.host.json").read_text())
    host.update(
        alias=alias,
        deployment_id="live-" + alias,
        max_calls=16,
        selected_tools=[
            "new_page",
            "close_page",
            "evaluate_script",
            "screencast_start",
            "screencast_stop",
            "performance_start_trace",
            "performance_stop_trace",
            "performance_analyze_insight",
        ],
    )
    host["arguments"].extend(
        [
            "--workspace=" + str(files),
            "--experimentalScreencast",
            "--experimentalFfmpegPath=/opt/homebrew/bin/ffmpeg",
            "--screencastFps=5",
        ]
    )
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
    server = fixture_server("HTLK_RECORD_" + uuid.uuid4().hex)
    session = Session(campaign, alias, alias)
    report = {"recording_mode": "documented experimental ffmpeg", "cleanup": False}
    page = None
    try:
        opened = display(
            session.call("new_page", {"url": f"http://127.0.0.1:{server.server_port}/"})
        )
        page = int(re.search(r"^(\d+):.*\[selected\]", opened, re.MULTILINE)[1])
        started = display(
            session.call("screencast_start", {"filePath": str(files / "recording.mp4")})
        )
        require(
            "recording started" in started.lower(),
            "video recorder did not actually start",
        )
        session.call(
            "evaluate_script",
            {
                "function": "() => { window.animation = setInterval(() => { document.body.style.background = Math.random() > .5 ? 'pink' : 'lightblue'; }, 100); return true; }",
                "waitForStableDom": False,
            },
        )
        time.sleep(1)
        stopped = display(session.call("screencast_stop", {}))
        require(
            "stopped and saved" in stopped.lower(),
            "video stop did not confirm a real recording",
        )
        video = files / "recording.mp4"
        require(
            video.stat().st_size > 100 and b"ftyp" in video.read_bytes()[:32],
            "recording file is not MP4",
        )
        session.call(
            "evaluate_script",
            {
                "function": "() => { clearInterval(window.animation); return true; }",
                "waitForStableDom": False,
            },
        )
        session.call("performance_start_trace", {"reload": True, "autoStop": False})
        time.sleep(0.5)
        trace = display(session.call("performance_stop_trace", {}))
        (files / "trace-summary.txt").write_text(trace)
        insight_set = re.search(r"(?m)^## insight set id:\s*(\S+)", trace)
        insight = re.search(r"insight name:\s*(\S+)", trace)
        require(bool(insight_set and insight), "actual trace has no available insight")
        result = display(
            session.call(
                "performance_analyze_insight",
                {"insightSetId": insight_set[1], "insightName": insight[1]},
            )
        )
        require(
            insight[1] in result or "insight" in result.lower(),
            "actual insight detail absent",
        )
        report.update(
            video_bytes=video.stat().st_size,
            insight=insight[1],
            successful_tools=[
                "screencast_start",
                "screencast_stop",
                "performance_analyze_insight",
            ],
        )
        session.call("close_page", {"pageId": page})
        page = None
        session.close()
        report["cleanup"] = True
    finally:
        if not session.closed:
            try:
                if page is not None:
                    session.call("close_page", {"pageId": page}, success=None)
                session.close()
            except (OSError, AssertionError):
                session.kill()
        server.shutdown()
        server.server_close()
        write_json(campaign / f"{alias}.report.json", report)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
