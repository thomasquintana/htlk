"""Private controller for the ordinary HTLK live-session graph."""

import fcntl
import json
import math
import os
from pathlib import Path
import select
import signal
import subprocess
import time
from live_process_cleanup import descendants, terminate_known

ROOT = Path(__file__).resolve().parents[1]


def write_json(path, value):
    with path.open("w") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")
    path.chmod(0o600)


def require(condition, message):
    if not condition:
        raise AssertionError(message)


class Session:
    def __init__(self, campaign, alias, name=None, timeout=220):
        self.campaign = campaign.resolve()
        self.output = self.campaign / (name or f"{alias}-stdio")
        self.sequence = 0
        self.timeout = timeout
        self.closed = False
        log = self.campaign / f"{self.output.name}.stderr"
        self.log = os.fdopen(
            os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w"
        )
        self.process = subprocess.Popen(
            [
                str(ROOT / "target/debug/examples/mcp_live_session"),
                str(self.campaign / f"{alias}.host.json"),
                str(self.campaign / f"{alias}.catalogs.json"),
                str(self.output),
            ],
            cwd=ROOT,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self.log,
            text=True,
            start_new_session=True,
        )
        write_json(
            self.campaign / f"{self.output.name}.controller.json",
            {"runner_pid": self.process.pid, "output": str(self.output)},
        )
        try:
            require(
                self.event(120).get("event") == "ready",
                "live runner did not become ready",
            )
        except BaseException:
            self.kill()
            raise

    def event(self, timeout):
        ready, _, _ = select.select([self.process.stdout], [], [], timeout)
        require(bool(ready), "live controller event timed out; private stderr retained")
        line = self.process.stdout.readline()
        require(bool(line), "live runner exited; inspect private stderr")
        return json.loads(line)

    def call(self, tool, arguments, *, success=True):
        started = time.monotonic()
        self.process.stdin.write(
            json.dumps({"tool": tool, "arguments": arguments}) + "\n"
        )
        self.process.stdin.flush()
        event = self.event(self.timeout)
        require(
            event.get("event") == "result" and event.get("tool") == tool,
            "unexpected live event",
        )
        self.sequence += 1
        require(event["sequence"] == self.sequence, "live sequence discontinuity")
        if success is not None:
            expected = "succeeded" if success else "failed"
            require(
                event["status"] == expected,
                f"{tool}: expected {expected}, got {event['status']}; private evidence retained",
            )
        result = json.loads(
            (self.output / f"response-{self.sequence:03}.json").read_text()
        )
        print(
            json.dumps(
                {
                    "campaign": self.output.name,
                    "tool": tool,
                    "status": event["status"],
                    "failure_code": event.get("failure_code"),
                    "calls": event["provider_calls"],
                    "seconds": round(time.monotonic() - started, 3),
                }
            ),
            flush=True,
        )
        return result.get("result") if event["status"] == "succeeded" else event

    def data(self, tool, arguments, **options):
        envelope = self.call(tool, arguments, **options)
        require("data" in envelope, f"{tool}: normalized JSON absent")
        return envelope["data"]

    def close(self):
        if self.closed:
            return
        try:
            owned = descendants(self.process.pid)
            self.process.stdin.write('{"stop":true}\n')
            self.process.stdin.flush()
            require(
                self.event(30).get("event") == "closed",
                "live runner cleanup acknowledgement missing",
            )
            self.process.stdin.close()
            require(self.process.wait(timeout=15) == 0, "live runner cleanup failed")
            self.closed = True
            self.log.close()
            terminate_known(owned)
            inspected = subprocess.run(
                [
                    str(ROOT / "target/debug/examples/mcp_live_session"),
                    "--inspect",
                    str(self.output),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                timeout=40,
                check=False,
            )
            require(
                inspected.returncode == 0,
                "offline inspection failed; private store retained",
            )
            summary = json.loads(inspected.stdout)
            require(
                summary["provider_calls"] == 0
                and summary["verified_steps"] == self.sequence,
                "offline evidence mismatch",
            )
            write_json(self.output / "offline-summary.json", summary)
        finally:
            if not self.closed:
                self.kill()

    def kill(self):
        if self.process.poll() is None:
            terminate_known(descendants(self.process.pid))
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=5)
        self.log.close()


def reserve(campaign, provider, case, maximum_usd, request_bound=1, *, once=False):
    """Conservatively retain reservations even if delivery or charges are uncertain."""
    path = campaign / "budget.json"
    require(
        isinstance(maximum_usd, (int, float))
        and math.isfinite(maximum_usd)
        and maximum_usd >= 0,
        "invalid provider cost reservation",
    )
    require(
        isinstance(request_bound, int) and 0 <= request_bound <= 10000,
        "invalid provider request bound",
    )
    with path.open("r+") as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        ledger = json.load(stream)
        if once and any(
            entry["provider"] == provider
            and entry.get("released_before_provider_dispatch_usd", 0)
            < entry["reserved_usd"]
            for entry in ledger["entries"]
        ):
            return
        require(
            ledger["reserved_usd"] + maximum_usd <= ledger["limit_usd"],
            "provider budget exhausted",
        )
        ledger["reserved_usd"] += maximum_usd
        ledger["provider_requests"] += request_bound
        ledger["entries"].append(
            {
                "provider": provider,
                "case": case,
                "reserved_usd": maximum_usd,
                "upstream_request_bound": request_bound,
                "time": time.time(),
            }
        )
        stream.seek(0)
        json.dump(ledger, stream, indent=2)
        stream.truncate()
