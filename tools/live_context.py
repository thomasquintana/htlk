#!/usr/bin/env python3
"""Every real context tool on an isolated deployment and actual reference providers."""

import argparse
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import struct
import threading
import time
import urllib.request
from live_context_env import Environment
from live_support import ROOT, require, write_json


class Campaign:
    def __init__(self, environment):
        self.environment = environment
        self.phase = None
        self.tools = set()
        self.report = {"tools": {}, "cleanup": False}

    def open(self, name, tools, **options):
        if self.phase:
            self.phase.close()
        self.phase = self.environment.phase(name, tools, **options)

    def call(self, tool, arguments, *, success=True):
        if success:
            result = self.phase.data(tool, arguments)
            self.tools.add(tool)
            self.report["tools"][tool] = "passed"
            return result
        return self.phase.call(tool, arguments, success=False)

    def job(self, receipt, expected="succeeded"):
        for _ in range(50):
            progress = self.call("context_job_status", {"job": receipt["job"]})
            if progress["state"] in {"succeeded", "failed", "cancelled"}:
                if progress["state"] != expected:
                    write_json(
                        self.environment.directory / "unexpected-job.json", progress
                    )
                require(
                    progress["state"] == expected,
                    "actual processing job failed; protected evidence retained",
                )
                if expected != "cancelled" or progress.get("cleanup_state") not in {
                    "pending",
                    "running",
                }:
                    return progress
            time.sleep(0.2)
        raise AssertionError("context job did not settle within its poll bound")

    def purge(self, receipt):
        for _ in range(60):
            progress = self.call("context_purge_status", {"purge": receipt["purge"]})
            if progress["completed"]:
                return progress
            time.sleep(0.5)
        raise AssertionError("context purge did not complete")

    def execute(
        self, fixture_url, remaining_only=False, purge_only=False, retry_only=False
    ):
        owner = self.environment.owner
        workspace, collection = owner["workspace"], owner["collection"]
        scope = {"kind": "workspace", "id": workspace}
        marker = self.environment.marker
        if retry_only:
            self.open(
                "retry-factual",
                [
                    "context_ingest_text",
                    "context_job_status",
                    "context_cancel_job",
                    "context_retry_job",
                ],
            )
            slow = self.call(
                "context_ingest_text",
                {
                    "collection": collection,
                    "name": "slow-factual.txt",
                    "text": "The ocean is blue.",
                    "profile": "slow",
                    "idempotency_key": "slow-factual",
                },
            )
            self.call(
                "context_cancel_job",
                {"job": slow["job"], "idempotency_key": "cancel-factual"},
            )
            self.job(slow, "cancelled")
            retried = self.call(
                "context_retry_job",
                {"job": slow["job"], "idempotency_key": "retry-factual"},
            )
            self.job(retried)
            self.report["retried_job_published"] = True
            self.phase.close()
            self.phase = None
            return
        if purge_only:
            self.open("http-purge", ["context_purge", "context_purge_status"])
            self.purge(
                self.call(
                    "context_purge",
                    {
                        "target": scope,
                        "idempotency_key": "http-purge-owned-empty-workspace",
                    },
                )
            )
            self.phase.close()
            self.phase = None
            return
        self.open(
            "acquisition",
            [
                "context_identity",
                "context_resource",
                "context_create_collection",
                "context_ingest_text",
                "context_ingest_url",
                "context_job_status",
                "context_query",
                "context_expand",
            ],
        )
        require(
            self.call("context_identity", {})["principal"] == owner["principal"],
            "authenticated context identity mismatch",
        )
        if not remaining_only:
            self.call(
                "context_create_collection",
                {
                    "workspace": workspace,
                    "label": "Synthetic extra collection",
                    "idempotency_key": "create-collection",
                },
            )
            self.call("context_resource", {"resource": scope})
        source = self.call(
            "context_ingest_text",
            {
                "collection": collection,
                "name": "blue.txt",
                "text": "The ocean is blue. " + marker,
                "idempotency_key": "inline-text",
            },
        )
        self.job(source)
        if not remaining_only:
            imported = self.call(
                "context_ingest_url",
                {
                    "collection": collection,
                    "name": "url.txt",
                    "url": fixture_url,
                    "media_type": "text/plain",
                    "idempotency_key": "exact-url",
                },
            )
            self.job(imported)
        context = self.call(
            "context_query",
            {"scope": scope, "query": "ocean blue", "token_budget": 512},
        )
        require(
            bool(context["evidence"])
            and "blue" in json.dumps(context["evidence"]).lower(),
            "real-provider retrieval evidence absent",
        )
        self.call(
            "context_expand",
            {
                "pointer": {
                    "kind": "asset",
                    "asset": source["asset"],
                    "revision": source["revision"],
                }
            },
        )
        self.report["source"] = source

        if remaining_only:
            self.memory_and_cleanup(workspace, collection, scope, source, context)
            return

        self.open(
            "media",
            [
                "context_create_upload",
                "context_finalize_upload",
                "context_job_status",
                "context_asset_history",
                "context_reprocess",
                "context_extract_media",
                "context_expand",
            ],
        )
        image = base64.b64decode(
            (ROOT.parent / "llm-mcp/tests/fixtures/shapes.png.base64").read_text()
        )
        upload = self.call(
            "context_create_upload",
            {
                "collection": collection,
                "name": "shapes.png",
                "media_type": "image/png",
                "size_bytes": len(image),
                "sha256": hashlib.sha256(image).hexdigest(),
                "idempotency_key": "image-upload",
            },
        )
        transfer = urllib.request.Request(
            upload["url"], data=image, headers=upload["headers"], method="PUT"
        )
        with urllib.request.urlopen(transfer, timeout=15) as response:
            require(
                response.status in {200, 201, 204}, "actual data-plane transfer failed"
            )
        image_receipt = self.call(
            "context_finalize_upload",
            {"upload": upload["upload"], "idempotency_key": "finalize-image"},
        )
        self.job(image_receipt)
        self.call(
            "context_asset_history", {"asset": image_receipt["asset"], "limit": 10}
        )
        replacement = self.call(
            "context_reprocess",
            {
                "asset": image_receipt["asset"],
                "expected_revision": image_receipt["revision"],
                "idempotency_key": "reprocess-image",
            },
        )
        self.job(replacement)
        clip = self.call(
            "context_extract_media",
            {
                "asset": replacement["asset"],
                "revision": replacement["revision"],
                "location": {
                    "kind": "region",
                    "x": 0,
                    "y": 0,
                    "width": 16,
                    "height": 16,
                },
                "idempotency_key": "crop-image",
            },
        )
        cropped = self.job(clip)
        expanded = self.call(
            "context_expand",
            {"pointer": {"kind": "content", "content": cropped["result"]["content"]}},
        )
        with urllib.request.urlopen(expanded["url"], timeout=15) as response:
            data = response.read(2 * 1024 * 1024 + 1)
        require(
            data.startswith(b"\x89PNG")
            and struct.unpack(">II", data[16:24]) == (16, 16),
            "materialized crop dimensions mismatch",
        )
        self.report["crop"] = {
            "width": 16,
            "height": 16,
            "sha256": hashlib.sha256(data).hexdigest(),
        }

        self.open(
            "cancel",
            [
                "context_ingest_text",
                "context_job_status",
                "context_cancel_job",
                "context_retry_job",
            ],
        )
        slow = self.call(
            "context_ingest_text",
            {
                "collection": collection,
                "name": "slow.txt",
                "text": "A bounded slow synthetic operation.",
                "profile": "slow",
                "idempotency_key": "slow-source",
            },
        )
        self.call(
            "context_cancel_job", {"job": slow["job"], "idempotency_key": "cancel-slow"}
        )
        self.job(slow, "cancelled")
        retried = self.call(
            "context_retry_job", {"job": slow["job"], "idempotency_key": "retry-slow"}
        )
        try:
            self.job(retried)
        except AssertionError:
            self.report["async_retry"] = "failed contract; original evidence retained"
            self.tools.discard("context_retry_job")
            self.report["tools"]["context_retry_job"] = (
                "accepted; asynchronous job failed"
            )

        self.memory_and_cleanup(workspace, collection, scope, source, context)

    def memory_and_cleanup(self, workspace, collection, scope, source, context):
        self.open(
            "memory",
            [
                "context_write_memory",
                "context_reconcile_memory",
                "context_read_memory",
                "context_synthesize",
                "context_rebuild_index",
                "context_set_access",
                "context_resource",
            ],
        )
        reference = {
            "asset": source["asset"],
            "revision": source["revision"],
            "location": {"kind": "asset"},
            "available": True,
        }
        memory = {
            "workspace": workspace,
            "subject": "Synthetic ocean color",
            "description": "Controlled evidence assertion",
            "value": {"kind": "text", "statement": "The ocean is blue."},
            "expected_version": 0,
            "derived_from": [reference],
            "idempotency_key": "memory-blue",
        }
        created_memory = self.call("context_write_memory", memory)
        memory.update(
            memory=created_memory["memory"],
            value={"kind": "text", "statement": "The ocean is red."},
            expected_version=1,
            idempotency_key="memory-red",
        )
        conflict = self.call("context_reconcile_memory", memory)
        require(
            conflict["view"]["state"] == "conflicted",
            "model reconciliation failed to preserve contradiction",
        )
        self.call(
            "context_read_memory",
            {"memory": created_memory["memory"], "history": True, "limit": 5},
        )
        answer = self.call(
            "context_synthesize",
            {
                "workspace": workspace,
                "question": "What color is the ocean in the supplied source evidence?",
                "evidence": [item["id"] for item in context["evidence"]],
                "save": True,
                "idempotency_key": "saved-answer",
                "token_budget": 512,
            },
        )
        require(
            "blue" in answer["text"].lower() and answer["citations"],
            "actual cited synthesis mismatch",
        )
        self.call("context_rebuild_index", {"workspace": workspace})
        asset = {"kind": "asset", "id": source["asset"]}
        resource = self.call("context_resource", {"resource": asset})
        version = resource.get("acl_version", 1)
        self.call(
            "context_set_access",
            {
                "resource": asset,
                "expected_acl_version": version,
                "inherit": True,
                "grants": [
                    {
                        "recipient": {
                            "kind": "principal",
                            "id": self.environment.guest["principal"],
                        },
                        "permissions": ["read"],
                    }
                ],
                "idempotency_key": "grant-guest",
            },
        )
        self.open("guest", ["context_resource"], guest=True)
        self.call("context_resource", {"resource": asset})
        self.open("revoke", ["context_set_access", "context_resource"])
        self.call(
            "context_set_access",
            {
                "resource": asset,
                "expected_acl_version": version + 1,
                "inherit": True,
                "grants": [],
                "idempotency_key": "revoke-guest",
            },
        )
        self.open("guest-denied", ["context_resource"], guest=True)
        self.call("context_resource", {"resource": asset}, success=False)
        self.phase.close()
        self.phase = None
        self.environment.stop_http()

        self.open(
            "stdio",
            [
                "context_identity",
                "context_ingest_text",
                "context_job_status",
                "context_query",
                "context_purge",
                "context_purge_status",
            ],
            stdio=True,
        )
        self.call("context_identity", {})
        stdio = self.call(
            "context_ingest_text",
            {
                "collection": collection,
                "name": "stdio.txt",
                "text": "The ocean is blue in a synthetic stdio fixture.",
                "idempotency_key": "stdio-source",
            },
        )
        self.job(stdio)
        require(
            self.call(
                "context_query",
                {"scope": scope, "query": "blue ocean", "token_budget": 256},
            )["evidence"],
            "stdio retrieval absent",
        )
        self.purge(
            self.call(
                "context_purge",
                {"target": scope, "idempotency_key": "purge-owned-workspace"},
            )
        )
        self.phase.close()
        self.phase = None
        self.report["successful_tools"] = sorted(self.tools)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("campaign", type=Path)
    parser.add_argument("--attempt", default="one")
    parser.add_argument("--remaining-only", action="store_true")
    parser.add_argument("--purge-only", action="store_true")
    parser.add_argument("--retry-only", action="store_true")
    args = parser.parse_args()
    environment = Environment(args.campaign, args.attempt)
    campaign = Campaign(environment)
    fixture = b"The ocean is blue. Exact URL synthetic fixture."

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(fixture)))
            self.end_headers()
            self.wfile.write(fixture)

        def log_message(self, *_):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        environment.prepare()
        environment.start_http()
        campaign.execute(
            f"http://127.0.0.1:{server.server_port}/fixture.txt",
            args.remaining_only,
            args.purge_only,
            args.retry_only,
        )
    finally:
        if campaign.phase and not campaign.phase.closed:
            campaign.phase.kill()
        server.shutdown()
        server.server_close()
        environment.cleanup()
        campaign.report["cleanup"] = True
        write_json(environment.directory / "report.json", campaign.report)
    print(
        json.dumps(
            {
                "server": "context",
                "successful_tools": sorted(campaign.tools),
                "cleanup": campaign.report["cleanup"],
            }
        )
    )


if __name__ == "__main__":
    main()
