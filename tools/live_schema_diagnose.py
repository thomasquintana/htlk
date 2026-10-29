#!/usr/bin/env python3
"""Independently validate saved actual output against its untouched descriptor.

Requires jsonschema in a test-only environment. Prints paths/keywords, never values.
"""

import argparse
import json
from pathlib import Path
import jsonschema


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("catalog", type=Path)
    parser.add_argument("tool")
    parser.add_argument("response", type=Path)
    args = parser.parse_args()
    catalog = json.loads(args.catalog.read_text())
    tool = next(
        entry["descriptor"]
        for entry in catalog["tools"]["tools"]
        if entry["descriptor"]["name"] == args.tool
    )
    raw = json.loads(args.response.read_text())
    validator = jsonschema.validators.validator_for(tool["outputSchema"])(
        tool["outputSchema"]
    )
    errors = [
        {
            "instance_path": list(error.absolute_path),
            "schema_path": list(error.absolute_schema_path),
            "keyword": error.validator,
        }
        for error in validator.iter_errors(raw.get("structuredContent"))
    ]
    extra = sorted(
        set(raw.get("structuredContent", {}))
        - set(tool["outputSchema"].get("properties", {}))
    )
    print(
        json.dumps(
            {
                "tool": args.tool,
                "independent_json_schema_valid": not errors,
                "unexpected_root_keys": extra,
                "errors": errors,
            },
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
