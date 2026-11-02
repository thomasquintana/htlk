#!/usr/bin/env python3
"""Print bounded tool argument shapes from an actual captured catalog."""

import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("catalog", type=Path)
    args = parser.parse_args()
    for item in json.loads(args.catalog.read_text())["tools"]["tools"]:
        tool = item["descriptor"]
        schema = tool["inputSchema"]
        print(
            json.dumps(
                {
                    "tool": tool["name"],
                    "required": schema.get("required", []),
                    "properties": {
                        name: {
                            key: value
                            for key, value in spec.items()
                            if key
                            in {
                                "type",
                                "enum",
                                "description",
                                "default",
                                "minimum",
                                "maximum",
                            }
                        }
                        for name, spec in schema.get("properties", {}).items()
                    },
                }
            )
        )


if __name__ == "__main__":
    main()
