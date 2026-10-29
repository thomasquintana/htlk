#!/usr/bin/env python3
"""Every discovered Chrome tool on a localhost-only synthetic browser fixture."""

import argparse
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import re
import threading
import time
import uuid
from live_support import ROOT, Session, require, write_json


def display(envelope):
    if envelope is None:
        return ""
    return "\n".join(
        block["text"]
        for block in envelope["protocol_result"]["content"]
        if block["type"] == "text"
    )


def fixture_server(marker):
    page = (
        (ROOT / "tools/fixtures/live_browser.html")
        .read_text()
        .replace("__MARKER__", marker)
        .encode()
    )

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            data = (
                json.dumps({"marker": marker, "ok": True}).encode()
                if self.path == "/payload.json"
                else page
            )
            self.send_response(200)
            self.send_header(
                "Content-Type",
                "application/json"
                if self.path == "/payload.json"
                else "text/html; charset=utf-8",
            )
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--attempt", default="one")
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    marker = "HTLK_BROWSER_" + uuid.uuid4().hex
    server = fixture_server(marker)
    url = f"http://127.0.0.1:{server.server_port}/"
    files = campaign / "browser-files" / args.attempt
    files.mkdir(mode=0o700)
    upload = files / "synthetic-upload.txt"
    upload.write_text(marker)
    report = {"marker": marker, "tools": {}, "artifacts": {}, "cleanup": False}
    session = None
    page_ids = []
    initial_snapshot = ""

    def call(tool, arguments):
        value = session.call(tool, arguments, success=None)
        if value.get("event") == "result":
            report["tools"].setdefault(
                tool, {"status": "failed", "failure_code": value["failure_code"]}
            )
            return None
        report["tools"][tool] = {"status": "passed"}
        return value

    def named(snapshot, label):
        role = (
            "textbox"
            if label in {"First field", "Second field"}
            else "heading"
            if label == "Styled heading"
            else "button"
        )
        matches = re.findall(
            r"uid=(\S+) " + role + r' "' + re.escape(label) + r'"', snapshot
        )
        require(len(matches) == 1, "fixture accessibility target missing or ambiguous")
        return matches[0]

    def uid(label):
        return named(initial_snapshot, label)

    def evaluate(script):
        return call("evaluate_script", {"function": script, "waitForStableDom": False})

    try:
        session = Session(campaign, "chrome", f"chrome-{args.attempt}")
        opened = display(call("new_page", {"url": url}))
        match = re.search(r"^(\d+):.*\[selected\]", opened, re.MULTILINE)
        require(bool(match), "created test-owned page identity absent")
        first = int(match[1])
        page_ids.append(first)
        initial_snapshot = display(call("take_snapshot", {}))
        require(marker in initial_snapshot, "controlled browser marker absent")
        call("wait_for", {"text": [marker], "timeout": 5000})
        result = evaluate(
            "() => ({marker: document.getElementById('marker').textContent})"
        )
        require(
            result["data"]["marker"] == marker,
            "automatic evaluation JSON extraction failed",
        )
        call("click", {"uid": uid("Click target")})
        call("hover", {"uid": uid("Click target")})
        call("fill", {"uid": uid("First field"), "value": "single field"})
        snapshot = initial_snapshot
        inputs = {}
        for label in ["First field", "Second field"]:
            inputs[label] = named(snapshot, label)
        call(
            "fill_form",
            {
                "elements": [
                    {"uid": inputs["First field"], "value": "first form"},
                    {"uid": inputs["Second field"], "value": "second form"},
                ]
            },
        )
        call("click", {"uid": uid("First field")})
        call("press_key", {"key": "End"})
        call("type_text", {"text": " typed", "submitKey": "Tab"})
        values = evaluate(
            "() => ({first: document.getElementById('first').value, second: document.getElementById('second').value})"
        )
        require(
            "typed" in values["data"]["first"]
            and values["data"]["second"] == "second form",
            "actual form state mismatch",
        )
        snapshot = initial_snapshot
        drag = {}
        for label in ["Drag source", "Drop target"]:
            drag[label] = named(snapshot, label)
        call("drag", {"from_uid": drag["Drag source"], "to_uid": drag["Drop target"]})
        call("upload_file", {"uid": uid("Upload file"), "filePaths": [str(upload)]})
        call("get_css_styles", {"uid": uid("Styled heading"), "pageSize": 3})
        call("emulate", {"colorScheme": "dark", "cpuThrottlingRate": 1})
        call("resize_page", {"width": 820, "height": 620})
        evaluate(
            "() => { setTimeout(() => alert('HTLK controlled dialog'), 150); return 'scheduled'; }"
        )
        time.sleep(0.3)
        call("handle_dialog", {"action": "accept"})
        messages = display(call("list_console_messages", {"pageSize": 20}))
        message = re.search(r"msgid=(\d+).*HTLK console marker", messages)
        if message:
            call("get_console_message", {"msgid": int(message[1])})
        else:
            report["tools"]["get_console_message"] = {
                "status": "blocked",
                "reason": "no returned marker message ID",
            }
        requests = display(call("list_network_requests", {"pageSize": 30}))
        request = re.search(r"reqid=(\d+).*payload\.json", requests)
        if request:
            call("get_network_request", {"reqid": int(request[1])})
        else:
            report["tools"]["get_network_request"] = {
                "status": "blocked",
                "reason": "no returned fixture request ID",
            }
        shot = call("take_screenshot", {"format": "jpeg", "quality": 30})
        require(
            any(
                block["type"] == "image" for block in shot["protocol_result"]["content"]
            ),
            "screenshot image block absent",
        )
        call("take_heapsnapshot", {"filePath": str(files / "page.heapsnapshot")})
        call("screencast_start", {"filePath": str(files / "page.mp4")})
        evaluate(
            "() => { document.body.style.background='rgb(200,220,240)'; return true; }"
        )
        time.sleep(0.3)
        call("screencast_stop", {})
        call("navigate_page", {"type": "url", "url": url + "next", "timeout": 10000})
        call("performance_start_trace", {"reload": True, "autoStop": False})
        time.sleep(0.5)
        evaluate(
            "() => ({marker: document.getElementById('marker').textContent, price: 1})"
        )
        trace = display(
            call("performance_stop_trace", {"filePath": str(files / "trace.json.gz")})
        )
        identifiers = re.findall(r"insightSetId[:=]\s*[`\"']?([^\s`\"']+)", trace)
        if identifiers:
            call(
                "performance_analyze_insight",
                {"insightSetId": identifiers[0], "insightName": "DocumentLatency"},
            )
        else:
            report["tools"]["performance_analyze_insight"] = {
                "status": "blocked",
                "reason": "no returned insight-set ID parsed",
            }
        call(
            "lighthouse_audit",
            {
                "mode": "snapshot",
                "device": "desktop",
                "outputDirPath": str(files / "audit"),
            },
        )
        second_page = display(
            call("new_page", {"url": url + "other", "background": True})
        )
        listed = display(call("list_pages", {}))
        for value in re.findall(r"^(\d+):", second_page + "\n" + listed, re.MULTILINE):
            identifier = int(value)
            if identifier != 0 and identifier not in page_ids:
                page_ids.append(identifier)
        call("select_page", {"pageId": first})
        for identifier in reversed(page_ids):
            call("close_page", {"pageId": identifier})
        require(
            marker not in display(call("list_pages", {})),
            "test-owned page marker survived cleanup",
        )
        session.close()
        session = None
        for path in files.rglob("*"):
            if path.is_file():
                digest = hashlib.sha256()
                with path.open("rb") as stream:
                    while block := stream.read(1024 * 1024):
                        digest.update(block)
                report["artifacts"][str(path.relative_to(files))] = {
                    "bytes": path.stat().st_size,
                    "sha256": digest.hexdigest(),
                }
        report["cleanup"] = True
    finally:
        if session:
            try:
                for identifier in reversed(page_ids):
                    session.call("close_page", {"pageId": identifier}, success=None)
                session.close()
                report["cleanup"] = True
            except (OSError, AssertionError):
                session.kill()
        server.shutdown()
        server.server_close()
        write_json(campaign / f"chrome-{args.attempt}.report.json", report)
    print(
        json.dumps(
            {
                "server": "chrome",
                "passed": sum(
                    item["status"] == "passed" for item in report["tools"].values()
                ),
                "tools": report["tools"],
                "cleanup": report["cleanup"],
            }
        )
    )


if __name__ == "__main__":
    main()
