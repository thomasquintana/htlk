#!/usr/bin/env python3
"""A factual summary-enabled query with the summarizer result group included."""

import argparse
import json
from pathlib import Path
import re
import subprocess
from live_support import ROOT, Session, require, reserve, write_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    alias = "brave-summary"
    host = json.loads((campaign / "brave.host.json").read_text())
    host.update(
        alias=alias,
        deployment_id="live-" + alias,
        max_calls=4,
        selected_tools=["brave_web_search", "brave_summarizer"],
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
    session = Session(campaign, alias, alias)
    report = {"normal_summary": "blocked"}
    try:
        reserve(campaign, "brave", "factual-summary-key", 0.05)
        response = session.call(
            "brave_web_search",
            {
                "query": "What causes ocean tides?",
                "count": 1,
                "summary": True,
                "result_filter": ["web", "summarizer"],
            },
        )
        keys = [
            re.fullmatch(r"Summarizer key: (\S+)", block["text"].strip())
            for block in response["protocol_result"]["content"]
            if block["type"] == "text"
        ]
        keys = [match[1] for match in keys if match]
        if keys:
            reserve(campaign, "brave", "actual-summary-key", 0.50, 20)
            result = session.call("brave_summarizer", {"key": keys[0]}, success=None)
            if result.get("event") != "result":
                text = " ".join(
                    block["text"]
                    for block in result["protocol_result"]["content"]
                    if block["type"] == "text"
                )
                require(
                    "tide" in text.lower(),
                    "summary does not address the controlled factual question",
                )
                report["normal_summary"] = "passed"
            else:
                report["normal_summary"] = "failed"
        else:
            report["reason"] = (
                "actual factual query with summary group still returned no key"
            )
        session.close()
    finally:
        if not session.closed:
            session.kill()
        write_json(campaign / f"{alias}.report.json", report)
    print(json.dumps(report))


if __name__ == "__main__":
    main()
