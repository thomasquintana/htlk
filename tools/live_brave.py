#!/usr/bin/env python3
"""Bounded actual Brave endpoints; preserve entitlement and semantic failures."""

import argparse
import json
from pathlib import Path
import re
from live_support import Session, require, reserve, write_json


def text_blocks(envelope):
    return [
        block["text"]
        for block in envelope["protocol_result"]["content"]
        if block["type"] == "text"
    ]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--alias", default="brave")
    parser.add_argument("--name", default="brave-stdio")
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    report = {"cases": [], "successful_tools": []}
    cases = [
        (
            "brave_web_search",
            {
                "query": "Rust programming language",
                "count": 1,
                "result_filter": ["web"],
                "summary": True,
            },
        ),
        ("brave_local_search", {"query": "coffee shops in San Francisco", "count": 1}),
        ("brave_video_search", {"query": "NASA moon landing", "count": 1}),
        ("brave_news_search", {"query": "NASA", "count": 1, "freshness": "pm"}),
        ("brave_image_search", {"query": "Saturn planet", "count": 1}),
        (
            "brave_place_search",
            {"query": "museum", "location": "paris france", "count": 1},
        ),
        (
            "brave_llm_context",
            {
                "query": "What is the tallest mountain on Mars?",
                "count": 1,
                "maximum_number_of_urls": 1,
                "maximum_number_of_tokens": 1024,
            },
        ),
    ]
    session = Session(campaign, args.alias, args.name)
    summary_key = None
    try:
        for tool, arguments in cases:
            reserve(
                campaign,
                "brave",
                f"{args.name}/{tool}",
                0.05,
                2 if tool == "brave_local_search" else 1,
            )
            envelope = session.call(tool, arguments, success=None)
            entry = {"tool": tool, "status": "failed"}
            if envelope.get("event") == "result":
                entry["failure_code"] = envelope["failure_code"]
            else:
                blocks = text_blocks(envelope)
                require(bool(blocks), "successful search has no display content")
                entry.update(
                    status="passed",
                    extraction=envelope["extraction"]["status"],
                    structured_content="structuredContent"
                    in envelope["protocol_result"],
                )
                if tool == "brave_web_search":
                    for block in blocks:
                        found = re.fullmatch(r"Summarizer key: (\S+)", block.strip())
                        if found:
                            summary_key = found[1]
                    require(
                        any("http" in block and "url" in block for block in blocks),
                        "web result URL absent",
                    )
                if tool in {
                    "brave_image_search",
                    "brave_place_search",
                    "brave_llm_context",
                }:
                    require(
                        entry["structured_content"], "declared structured result absent"
                    )
                    require(
                        envelope["validation"]["declared_schema"]["status"] == "valid",
                        "declared search result did not validate",
                    )
                report["successful_tools"].append(tool)
            report["cases"].append(entry)
        if summary_key:
            reserve(campaign, "brave", f"{args.name}/brave_summarizer", 0.50, 20)
            envelope = session.call(
                "brave_summarizer", {"key": summary_key}, success=None
            )
            if envelope.get("event") == "result":
                report["cases"].append(
                    {
                        "tool": "brave_summarizer",
                        "status": "failed",
                        "failure_code": envelope["failure_code"],
                    }
                )
            else:
                require(
                    bool(" ".join(text_blocks(envelope)).strip()), "summary text empty"
                )
                report["successful_tools"].append("brave_summarizer")
                report["cases"].append(
                    {
                        "tool": "brave_summarizer",
                        "status": "passed",
                        "extraction": envelope["extraction"]["status"],
                    }
                )
        else:
            report["cases"].append(
                {
                    "tool": "brave_summarizer",
                    "status": "blocked",
                    "reason": "actual web search did not return a summarizer key",
                }
            )
        error = session.call(
            "brave_web_search",
            {"query": " ".join(["x"] * 51), "count": 1},
            success=False,
        )
        report["local_query_error"] = error["failure_code"]
        session.close()
    finally:
        if not session.closed:
            session.kill()
        write_json(campaign / f"{args.name}.report.json", report)
    print(json.dumps({"server": args.alias, **report}))


if __name__ == "__main__":
    main()
