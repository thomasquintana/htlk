#!/usr/bin/env python3
"""Offline metadata manifest and provider-usage totals from a private live campaign.

Requires cbor2 for stopped context histories. This does not contact any provider,
publish raw responses, infer semantic acceptance, or remove resources.
"""

import argparse
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path
import re

from live_history_audit import audit
from live_support import require, write_json


def read(path):
    return json.loads(path.read_text())


def file_digest(path):
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def campaign_audit(campaign):
    sessions = []
    direct_usages = []
    for path in sorted(campaign.glob("*/session.json")):
        directory = path.parent
        session = read(path)
        steps = [read(step) for step in sorted(directory.glob("step-*.json"))]
        outcomes = Counter(
            (step["tool"], step["status"], step["failure_code"]) for step in steps
        )
        evidence = []
        for step in steps:
            sequence = step["sequence"]
            raw = directory / f"raw-{sequence:03}.json"
            record = directory / f"evidence-{sequence:03}.json"
            if record.is_file():
                value = read(record)
                normalization = value.get("normalization") or {}
                evidence.append(
                    {
                        "sequence": sequence,
                        "tool": step["tool"],
                        "attempt_id": value["attempt_id"],
                        "invocation_id": value["invocation_id"],
                        "result_artifact": step["result_artifact"],
                        "binding_digest": normalization.get("binding_digest"),
                        "envelope_digest": normalization.get("envelope_digest"),
                        "raw_file_sha256": file_digest(raw) if raw.is_file() else None,
                    }
                )
            if directory.name in {"llm-stdio", "llm-http-two"} and raw.is_file():
                data = read(raw).get("structuredContent", {})
                data = data.get("response", data)
                if isinstance(data, dict) and "usage" in data:
                    model = data.get("model")
                    if step["tool"] == "classify":
                        model = "gpt-6-luna"
                    if step["tool"] == "transcribe":
                        model = "gpt-4o-mini-transcribe"
                    direct_usages.append({"model": model, "usage": data["usage"]})
        sessions.append(
            {
                "session": directory.name,
                **session,
                "catalog_file_sha256": file_digest(directory / "catalogs.json"),
                "program_file_sha256": file_digest(directory / "program.cbor"),
                "recorded_steps": len(steps),
                "provider_calls": max(
                    (step["provider_calls"] for step in steps), default=0
                ),
                "offline_verified": (directory / "offline-summary.json").is_file(),
                "outcomes": [
                    {
                        "tool": key[0],
                        "status": key[1],
                        "failure_code": key[2],
                        "count": count,
                    }
                    for key, count in sorted(outcomes.items())
                ],
                "evidence": evidence,
            }
        )
    histories = [
        audit(path)
        for path in sorted(campaign.glob("context*/application-htlk.sqlite"))
    ]
    nested = [entry for history in histories for entry in history["provider_usages"]]
    totals = defaultdict(
        lambda: {
            "responses": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "cached_input_tokens": 0,
        }
    )
    for entry in [*direct_usages, *nested]:
        require(
            bool(entry["model"]), "provider response lacks auditable model identity"
        )
        total = totals[entry["model"]]
        usage = entry["usage"]
        total["responses"] += 1
        total["input_tokens"] += usage.get(
            "input_tokens", usage.get("prompt_tokens", 0)
        )
        total["output_tokens"] += usage.get("output_tokens", 0)
        total["cached_input_tokens"] += usage.get("input_tokens_details", {}).get(
            "cached_tokens", 0
        )
    budget = read(campaign / "budget.json")
    return {
        "campaign": campaign.name,
        "sessions": sessions,
        "context_history": histories,
        "provider_usage_by_model": dict(sorted(totals.items())),
        "direct_usage_responses": len(direct_usages),
        "nested_usage_responses": len(nested),
        "budget": {
            key: budget[key]
            for key in ["limit_usd", "reserved_usd", "provider_requests"]
        },
        "budget_note": "provider_requests is the reserved bound, not observed requests; usage is received responses, not an invoice",
        "context_cleanup": [
            read(path) for path in sorted(campaign.glob("context*/cleanup.json"))
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--matrix",
        type=Path,
        help="check published tool rows against this campaign's actual inventory",
    )
    parser.add_argument(
        "--sessions", action="store_true", help="include compact session identities"
    )
    args = parser.parse_args()
    report = campaign_audit(args.campaign.resolve())
    if args.matrix:
        names = set()
        for alias in ["sandbox", "llm", "brave", "chrome", "context-acquisition"]:
            catalog = read(args.campaign / f"{alias}.catalogs.json")
            names.update(
                entry["descriptor"]["name"] for entry in catalog["tools"]["tools"]
            )
        rows = re.findall(r"(?m)^\| `([a-z][a-z_]+)` \|", args.matrix.read_text())
        require(
            set(rows) == names and len(rows) == len(names),
            "tool matrix does not exactly match discovered inventories",
        )
        print(json.dumps({"matrix_tools_verified": len(names)}))
    if args.output:
        require(not args.output.exists(), "refuse to replace an audit manifest")
        write_json(args.output, report)
    if args.sessions:
        for session in report["sessions"]:
            print(
                " | ".join(
                    str(session[key])
                    for key in [
                        "session",
                        "run_id",
                        "fingerprint",
                        "recorded_steps",
                        "offline_verified",
                    ]
                )
            )
    print(
        json.dumps(
            {
                key: report[key]
                for key in [
                    "campaign",
                    "provider_usage_by_model",
                    "direct_usage_responses",
                    "nested_usage_responses",
                    "budget",
                ]
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
