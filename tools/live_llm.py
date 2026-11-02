#!/usr/bin/env python3
"""Small real-provider acceptance of all four llm-mcp tools through HTLK."""

import argparse
import json
import math
from pathlib import Path
from live_support import ROOT, Session, require, reserve, write_json


def generation(value):
    texts = [
        part.get("text", "")
        for item in value.get("output", [])
        for part in item.get("content", [])
        if part.get("type") == "output_text"
    ]
    require(
        "orchid" in " ".join(texts).lower(),
        "generation did not return the controlled marker",
    )


def classification(value):
    answers = {answer["name"]: answer for answer in value["answers"]}
    require(
        answers["red_square"]["probability"] >= 0.8, "predicate classification mismatch"
    )
    require(answers["selection"]["choice"] == "left", "choice classification mismatch")


def embeddings(value):
    vectors = value["response"]["data"]
    require(
        sorted(item["index"] for item in vectors) == [0, 1],
        "embedding index mapping mismatch",
    )
    for item in vectors:
        vector = item["embedding"]
        require(
            len(vector) == 64 and all(math.isfinite(number) for number in vector),
            "embedding shape mismatch",
        )
        require(
            sum(number * number for number in vector) > 0, "embedding vector is zero"
        )


def transcription(value):
    require(
        "red" in value["text"].lower() and "blue" in value["text"].lower(),
        "synthetic speech was not transcribed",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--alias", default="llm")
    parser.add_argument("--name", default="llm-stdio")
    args = parser.parse_args()
    campaign = args.campaign.resolve()
    audio = (
        (ROOT.parent / "llm-mcp/tests/fixtures/shapes.wav.base64").read_text().strip()
    )
    cases = [
        (
            "generate",
            {
                "model": "gpt-4.1-mini",
                "input": [
                    {"role": "user", "content": "Reply with only the word ORCHID."}
                ],
                "store": False,
                "max_output_tokens": 32,
            },
            generation,
        ),
        (
            "classify",
            {
                "model": "gpt-6-luna",
                "input": "A red square is to the left of a blue circle.",
                "questions": [
                    {
                        "name": "red_square",
                        "type": "predicate",
                        "instructions": "Does the input describe a red square?",
                    },
                    {
                        "name": "selection",
                        "type": "choice",
                        "instructions": "Which side of the blue circle is the red square on?",
                        "choices": [{"value": "left"}, {"value": "right"}],
                    },
                ],
            },
            classification,
        ),
        (
            "embed",
            {
                "model": "text-embedding-3-small",
                "input": ["A red square next to a blue circle.", "A green triangle."],
                "dimensions": 64,
                "encoding_format": "float",
            },
            embeddings,
        ),
        (
            "transcribe",
            {
                "model": "gpt-4o-mini-transcribe",
                "file": {
                    "filename": "shapes.wav",
                    "mime_type": "audio/wav",
                    "data_base64": audio,
                },
                "language": "en",
                "response_format": "json",
            },
            transcription,
        ),
    ]
    report = {
        "cases": [],
        "successful_tools": [],
        "provider": "actual OpenAI API-key endpoints",
    }
    session = Session(campaign, args.alias, args.name)
    try:
        for tool, arguments, check in cases:
            reserve(campaign, "openai", f"{args.name}/{tool}", 0.20)
            envelope = session.call(tool, arguments, success=None)
            entry = {"tool": tool, "status": "failed"}
            if envelope.get("event") == "result":
                entry["failure_code"] = envelope["failure_code"]
            else:
                try:
                    check(envelope["data"])
                    entry["status"] = "passed"
                    report["successful_tools"].append(tool)
                    data = envelope["data"]
                    entry["usage"] = data.get(
                        "usage", data.get("response", {}).get("usage")
                    )
                except (AssertionError, KeyError, TypeError):
                    entry["failure"] = (
                        "semantic assertion failed; protected evidence retained"
                    )
            report["cases"].append(entry)
        rejected = session.call(
            "generate",
            {
                "model": "gpt-4.1-mini",
                "input": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "type": "input_image",
                                "image_url": "https://example.invalid/disallowed.png",
                            }
                        ],
                    }
                ],
                "store": False,
                "max_output_tokens": 16,
            },
            success=False,
        )
        require(
            rejected["failure_code"] == "MCP_TOOL_ERROR",
            "local media validation failure classification mismatch",
        )
        report["local_media_error"] = True
        session.close()
    finally:
        if not session.closed:
            session.kill()
        write_json(campaign / f"{args.name}.report.json", report)
    print(
        json.dumps(
            {
                "server": args.alias,
                "successful_tools": report["successful_tools"],
                "cases": report["cases"],
            }
        )
    )


if __name__ == "__main__":
    main()
