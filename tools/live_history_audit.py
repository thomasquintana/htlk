#!/usr/bin/env python3
"""Read-only immutable audit of private HTLK histories; emit bounded metadata only."""

import argparse
import json
from pathlib import Path
import sqlite3
import cbor2


def audit(database, metadata=False):
    """Only use immutable mode after all writers have stopped and checkpointed."""
    if not database.is_file():
        raise ValueError("existing stopped history database required")
    wal = database.with_name(database.name + "-wal")
    if wal.exists() and wal.stat().st_size:
        raise ValueError("checkpoint and close the history before immutable inspection")
    uri = database.resolve().as_uri() + "?mode=ro&immutable=1"
    with sqlite3.connect(uri, uri=True) as connection:
        return audit_connection(connection, database, metadata)


def audit_connection(connection, database, metadata):
    tables = [
        row[0]
        for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        )
    ]
    schema = {
        name: [row[1] for row in connection.execute(f'PRAGMA table_info("{name}")')]
        for name in tables
    }
    if metadata:
        return schema
    else:
        graphs = dict(connection.execute("SELECT fingerprint,graph_id FROM graphs"))
        runs = {
            row[0]: graphs.get(row[1], "unknown")
            for row in connection.execute("SELECT run_id,fingerprint FROM runs")
        }
        usages, failures, commands = [], [], []
        for run, family, identifier, payload in connection.execute(
            "SELECT run_id,family,record_id,payload FROM records"
        ):
            value = cbor2.loads(payload)
            if not isinstance(value, dict):
                continue
            failure = value.get("failure")
            if isinstance(failure, dict):
                failures.append(
                    {
                        "graph": runs.get(run),
                        "local_id": value.get("local_id"),
                        "code": failure.get("code"),
                    }
                )
            raw = value.get("raw")
            if not isinstance(raw, bytes):
                continue
            try:
                response = json.loads(raw)
            except (ValueError, UnicodeError):
                continue
            data = response.get("structuredContent", {})
            if not isinstance(data, dict):
                continue
            if "exit_code" in data:
                commands.append(
                    {
                        "graph": runs.get(run),
                        "exit_code": data["exit_code"],
                        "stdout_bytes": len(data.get("stdout", "")),
                        "stderr_bytes": len(data.get("stderr", "")),
                    }
                )
            data = data.get("response", data)
            if not isinstance(data, dict) or "usage" not in data:
                continue
            entry = {
                "graph": runs.get(run),
                "model": data.get("model"),
                "usage": data["usage"],
            }
            if runs.get(run) == "operator.enrich":
                output = "".join(
                    part.get("text", "")
                    for item in data.get("output", [])
                    for part in item.get("content", [])
                )
                try:
                    application = json.loads(output)
                    entry["application_json_keys"] = (
                        sorted(application) if isinstance(application, dict) else []
                    )
                    entry["entities"] = len(application.get("entities", []))
                    entry["relationships"] = len(application.get("relationships", []))
                    names = {
                        entity.get("name") for entity in application.get("entities", [])
                    }
                    entry["undeclared_relationship_refs"] = sum(
                        relation.get(key) not in names
                        for relation in application.get("relationships", [])
                        for key in ["from", "to"]
                    )
                except (ValueError, AttributeError):
                    entry["application_json_valid"] = False
                    entry["markdown_fenced"] = output.lstrip().startswith("```")
            usages.append(entry)
        return {
            "attempt": database.parent.name,
            "provider_usages": usages,
            "failed_boundaries": failures,
            "native_commands": commands,
        }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("database", type=Path, nargs="+")
    parser.add_argument("--metadata", action="store_true")
    args = parser.parse_args()
    reports = [audit(database, args.metadata) for database in args.database]
    print(json.dumps(reports, indent=2))


if __name__ == "__main__":
    main()
