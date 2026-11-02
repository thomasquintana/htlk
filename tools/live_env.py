#!/usr/bin/env python3
"""Load selected literal dotenv values privately, then replace this process.

Only explicitly allowed variable names are read into the child environment. No
shell evaluation/interpolation is performed and diagnostics never include values.
"""

import argparse
import json
import os
from pathlib import Path
import re


def selected_values(path, allowed):
    if path.stat().st_size > 65536:
        raise ValueError("credential file exceeds its bound")
    selected = {}
    for line in path.read_text().splitlines():
        line = line.strip()
        if line.startswith("export "):
            line = line[7:].lstrip()
        key, separator, value = line.partition("=")
        key = key.strip()
        if not separator or key not in allowed:
            continue
        value = value.strip()
        if value.startswith('"'):
            value = json.loads(value)
        elif value.startswith("'"):
            if not value.endswith("'"):
                raise ValueError("invalid selected credential quoting")
            value = value[1:-1]
        else:
            value = re.split(r"\s+#", value, maxsplit=1)[0].strip()
        if not isinstance(value, str) or "\0" in value:
            raise ValueError("invalid selected credential value")
        selected[key] = value
    return selected


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", type=Path)
    parser.add_argument("--allow", action="append", default=[])
    parser.add_argument("--set", action="append", default=[])
    parser.add_argument("--check", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    try:
        values = (
            selected_values(args.env_file, set(args.allow)) if args.env_file else {}
        )
        environment = os.environ.copy()
        # Do not accidentally select OAuth or an alternate provider from the shell.
        for key in list(environment):
            if key.startswith(("OPENAI_", "LLM_MCP_", "BRAVE_API_")):
                environment.pop(key)
        environment.update(values)
        for assignment in args.set:
            key, separator, value = assignment.partition("=")
            if not separator or not re.fullmatch(r"[A-Z][A-Z0-9_]*", key):
                raise ValueError("invalid environment override")
            environment[key] = value
        if args.check:
            print(json.dumps({key: bool(environment.get(key)) for key in args.allow}))
            return
        command = args.command
        if command and command[0] == "--":
            command = command[1:]
        if not command:
            raise ValueError("child command required")
        os.execvpe(command[0], command, environment)
    except (OSError, ValueError, TypeError):
        parser.exit(2, "Private environment preparation failed; no values emitted.\n")


if __name__ == "__main__":
    main()
