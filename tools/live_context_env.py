"""An isolated real context deployment with explicit lifecycle ownership."""

import copy
import json
import os
import signal
import subprocess
import time
import urllib.request
import uuid
from live_http import discover, free_port
from live_sandbox import cleanup
from live_support import ROOT, Session, require, reserve, write_json

CONTEXT = ROOT.parent / "context-mcp"


class Environment:
    def __init__(self, campaign, attempt="one"):
        self.campaign = campaign.resolve()
        self.directory = self.campaign / (
            "context" if attempt == "one" else "context-" + attempt
        )
        self.directory.mkdir(mode=0o700)
        self.marker = "htlk-context-" + uuid.uuid4().hex
        self.application = None
        self.logs = []
        self.ports = {}
        for name in ["postgres", "s3", "typesense", "http"]:
            for _ in range(32):
                port = free_port()
                if port not in self.ports.values():
                    self.ports[name] = port
                    break
            else:
                raise AssertionError("could not select distinct loopback ports")
        self.url = f"http://127.0.0.1:{self.ports['http']}/mcp"
        self.binary = CONTEXT / "target/debug/context-mcp"
        self.compose = [
            "docker",
            "compose",
            "--project-name",
            self.marker,
            "--file",
            str(self.directory / "compose.json"),
        ]
        self.environment = os.environ.copy()
        self.environment.update(
            {
                "CONTEXT_DATABASE_URL": f"postgres://context:fixture-password@127.0.0.1:{self.ports['postgres']}/context",
                "CONTEXT_S3_ACCESS_KEY": "fixture-access",
                "CONTEXT_S3_SECRET_KEY": "fixture-private-secret",
                "CONTEXT_TYPESENSE_KEY": "fixture-index-key",
                "DOCKER_HOST": json.loads(
                    (self.campaign / "sandbox.host.json").read_text()
                )["environment"]["DOCKER_HOST"],
                "ALLOW_DIRS": str(self.directory / "staging"),
                "SANDBOX_PROFILES_DIR": str(self.directory / "profiles"),
            }
        )
        self.docker_environment = {
            **self.environment,
            "DOCKER_CONFIG": str(self.campaign / "docker"),
        }

    def command(self, arguments, timeout=180):
        result = subprocess.run(
            [str(self.binary), *arguments],
            cwd=CONTEXT,
            env=self.environment,
            text=True,
            capture_output=True,
            timeout=timeout,
            check=False,
        )
        if result.returncode:
            log = self.directory / f"operator-failure-{time.time_ns()}.log"
            log.write_text(result.stderr)
            log.chmod(0o600)
            raise AssertionError(
                "context operator command failed; private diagnostic retained"
            )
        return result.stdout

    def prepare(self):
        compose = {
            "services": {
                "postgres": {
                    "image": "postgres:16@sha256:9e2cf6287f25c3151f4c9421ccce2b17f3e60061c111a29165e0d2cdd9ff0a97",
                    "environment": {
                        "POSTGRES_USER": "context",
                        "POSTGRES_PASSWORD": "fixture-password",
                        "POSTGRES_DB": "context",
                    },
                    "ports": [f"127.0.0.1:{self.ports['postgres']}:5432"],
                    "volumes": ["postgres:/var/lib/postgresql/data"],
                },
                "minio": {
                    "image": "context-mcp-minio:2025-10-15",
                    "user": "0:0",
                    "environment": {
                        "MINIO_ROOT_USER": "fixture-access",
                        "MINIO_ROOT_PASSWORD": "fixture-private-secret",
                    },
                    "ports": [f"127.0.0.1:{self.ports['s3']}:9000"],
                    "volumes": ["objects:/data"],
                },
                "typesense": {
                    "image": "typesense/typesense:29.1@sha256:2d9a7e348ce0de03a6061291023c54f83c47f0fa81f26b1b0d1110286e415b0c",
                    "command": ["--data-dir", "/data"],
                    "environment": {"TYPESENSE_API_KEY": "fixture-index-key"},
                    "ports": [f"127.0.0.1:{self.ports['typesense']}:8108"],
                    "volumes": ["indexes:/data"],
                },
            },
            "volumes": {"postgres": {}, "objects": {}, "indexes": {}},
        }
        write_json(self.directory / "compose.json", compose)
        subprocess.run(
            [*self.compose, "up", "--detach"],
            env=self.docker_environment,
            check=True,
            timeout=180,
        )
        for url in [
            f"http://127.0.0.1:{self.ports['s3']}/minio/health/ready",
            f"http://127.0.0.1:{self.ports['typesense']}/health",
        ]:
            for _ in range(120):
                try:
                    with urllib.request.urlopen(url, timeout=1) as response:
                        if response.status == 200:
                            break
                except OSError:
                    time.sleep(0.2)
            else:
                raise AssertionError("owned storage service readiness failed")
        for _ in range(100):
            result = subprocess.run(
                [
                    *self.compose,
                    "exec",
                    "-T",
                    "postgres",
                    "pg_isready",
                    "-U",
                    "context",
                ],
                env=self.docker_environment,
                capture_output=True,
                timeout=5,
            )
            if result.returncode == 0:
                break
            time.sleep(0.2)
        else:
            raise AssertionError("owned database readiness failed")
        self.configure()
        self.command(["provision-data", "--config", str(self.config)])
        self.command(["migrate", "--config", str(self.config)])
        self.command(
            [
                "discover-providers",
                "--config",
                str(self.directory / "discovery.json"),
                "--output",
                str(self.directory / "catalogs.json"),
            ]
        )
        self.owner = self.bootstrap("owner")
        self.guest = self.bootstrap("guest")
        reserve(
            self.campaign,
            "context nested OpenAI",
            "shared-context-campaign",
            15.0,
            150,
            once=True,
        )
        self.environment["CONTEXT_AUTHORIZATION"] = (
            "Bearer " + (self.directory / "owner.token").read_text().strip()
        )
        self.environment["CONTEXT_GUEST_AUTHORIZATION"] = (
            "Bearer " + (self.directory / "guest.token").read_text().strip()
        )
        self.environment["CONTEXT_STDIO_TOKEN"] = (
            (self.directory / "owner.token").read_text().strip()
        )
        # Endpoint secrets stay in this process's environment; host files contain references.
        os.environ.update(
            {
                key: value
                for key, value in self.environment.items()
                if key.startswith("CONTEXT_")
                or key in {"ALLOW_DIRS", "SANDBOX_PROFILES_DIR", "DOCKER_HOST"}
            }
        )
        write_json(
            self.directory / "inventory.json",
            {
                "project": self.marker,
                "ports": self.ports,
                "owner": self.owner,
                "guest": self.guest,
            },
        )

    def configure(self):
        (self.directory / "staging").mkdir(mode=0o700)
        (self.directory / "profiles").mkdir(mode=0o700)
        (self.directory / "roles").mkdir(mode=0o700)
        subprocess.run(
            [
                "python3",
                str(CONTEXT / "tools/processing_profile.py"),
                str(self.directory / "profiles/context.yml"),
                "--image",
                "htlk-context-media:siblings-20261029",
            ],
            env=self.docker_environment,
            check=True,
            timeout=30,
        )
        config = json.loads(
            (CONTEXT / "examples/operator/development.json").read_text()
        )
        config.update(
            bind=f"127.0.0.1:{self.ports['http']}",
            public_url=self.url,
            allowed_hosts=["127.0.0.1", "localhost"],
            allowed_origins=[self.url.rsplit("/", 1)[0]],
        )
        app = config["application"]
        app["oidc"] = None
        workflows = app["workflows"]
        workflows["database"] = str(self.directory / "application-htlk.sqlite")
        workflows["workers"] = 1
        workflows["endpoints"]["initial-model-provider"]["program"] = str(
            ROOT.parent / "llm-mcp/target/release/llm-mcp"
        )
        workflows["endpoints"]["initial-model-provider"]["environment"].update(
            LLM_MCP_MAX_CONCURRENCY="1"
        )
        workflows["endpoints"]["initial-sandbox-provider"]["program"] = str(
            ROOT.parent / "mcp-sandbox/dist/mcp-sandbox_darwin_arm64_v8.0/mcp-sandbox"
        )
        for role in workflows["roles"].values():
            original = CONTEXT / "examples/operator" / role["source"]
            source = (
                original.read_text()
                .replace("max_output_tokens = 2048", "max_output_tokens = 512")
                .replace("max_output_tokens = 1024", "max_output_tokens = 512")
            )
            source = source.replace(
                "session_id = inputs.request.job",
                "session_id = " + json.dumps(self.marker),
            )
            target = self.directory / "roles" / original.name
            target.write_text(source)
            role.update(
                source=str(target), catalogs=str(self.directory / "catalogs.json")
            )
        slow = self.directory / "roles/extract-slow.htlk"
        slow.write_text(
            (self.directory / "roles/extract.htlk")
            .read_text()
            .replace(
                'command = "context-process"', 'command = "sleep 15; context-process"'
            )
        )
        workflows["roles"]["extract.slow"] = {
            **workflows["roles"]["extract"],
            "source": str(slow),
        }
        data = app["data"]
        data.update(
            staging_directory=str(self.directory / "staging"),
            s3_endpoint=f"http://127.0.0.1:{self.ports['s3']}",
            s3_public_endpoint=f"http://127.0.0.1:{self.ports['s3']}",
            typesense_endpoint=f"http://127.0.0.1:{self.ports['typesense']}",
            max_upload_bytes=1048576,
            max_segments=16,
            workers=1,
            max_pending_jobs=16,
            upload_lifetime_seconds=30,
            url_hosts=["127.0.0.1"],
            url_allow_http=True,
        )
        data["profiles"]["slow"] = {
            **copy.deepcopy(data["profiles"]["multimodal"]),
            "extract_role": "extract.slow",
        }
        self.config = self.directory / "runtime.json"
        write_json(self.config, config)
        discovery = json.loads(
            (CONTEXT / "examples/integration/discover.json").read_text()
        )
        discovery["endpoints"] = workflows["endpoints"]
        actual = json.loads((self.campaign / "sandbox.catalogs.json").read_text())
        discovery["servers"]["sandbox"]["implementation_version"] = actual["tools"][
            "servers"
        ]["sandbox"]["implementation_version"]
        write_json(self.directory / "discovery.json", discovery)

    def bootstrap(self, name):
        identity = json.loads(
            self.command(
                [
                    "bootstrap",
                    "--config",
                    str(self.config),
                    "--issuer",
                    "htlk-live",
                    "--subject",
                    self.marker + "-" + name,
                    "--kind",
                    "service",
                    "--label",
                    "Synthetic HTLK " + name,
                ]
            )
        )
        arguments = [
            "issue-credential",
            "--config",
            str(self.config),
            "--principal",
            identity["principal"],
        ]
        for scope in ["read", "ingest", "share", "write_memory", "synthesize", "purge"]:
            arguments.extend(["--scope", scope])
        arguments.extend(["--output", str(self.directory / f"{name}.token")])
        self.command(arguments)
        return identity

    def start_http(self):
        log = self.directory / "application.stderr"
        stream = os.fdopen(
            os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w"
        )
        self.logs.append(stream)
        self.application = subprocess.Popen(
            [
                str(self.binary),
                "serve",
                "--config",
                str(self.config),
                "--transport",
                "http",
            ],
            env=self.environment,
            cwd=CONTEXT,
            stdout=stream,
            stderr=stream,
            start_new_session=True,
        )
        for _ in range(150):
            try:
                with urllib.request.urlopen(
                    self.url.replace("/mcp", "/health/ready"), timeout=1
                ) as response:
                    if response.status == 200:
                        return
            except OSError:
                time.sleep(0.2)
        raise AssertionError("owned context application readiness failed")

    def stop_http(self):
        if self.application and self.application.poll() is None:
            os.killpg(self.application.pid, signal.SIGTERM)
            self.application.wait(timeout=180)
        self.application = None

    def phase(self, name, tools, guest=False, stdio=False):
        alias = self.directory.name + "-" + name
        host = {
            "alias": alias,
            "deployment_id": "live-" + alias,
            "transport": "streamable_http",
            "url": self.url,
            "header_environment": {
                "Authorization": "CONTEXT_GUEST_AUTHORIZATION"
                if guest
                else "CONTEXT_AUTHORIZATION"
            },
            "max_calls": 80,
            "selected_tools": tools,
            "attempt_timeout_ms": 240000,
        }
        if stdio:
            host = {
                **host,
                "transport": "stdio",
                "program": str(self.binary),
                "arguments": [
                    "serve",
                    "--config",
                    str(self.config),
                    "--transport",
                    "stdio",
                ],
                "environment": {},
                "inherit_environment": [
                    "CONTEXT_DATABASE_URL",
                    "CONTEXT_S3_ACCESS_KEY",
                    "CONTEXT_S3_SECRET_KEY",
                    "CONTEXT_TYPESENSE_KEY",
                    "CONTEXT_STDIO_TOKEN",
                    "OPENAI_API_KEY",
                    "ALLOW_DIRS",
                    "SANDBOX_PROFILES_DIR",
                    "DOCKER_HOST",
                ],
            }
        write_json(self.campaign / f"{alias}.host.json", host)
        catalog = self.campaign / f"{alias}.catalogs.json"
        if stdio:
            subprocess.run(
                [
                    "node",
                    "tools/discover-mcp.mjs",
                    str(self.campaign / f"{alias}.host.json"),
                    str(catalog),
                ],
                cwd=ROOT,
                env=self.environment,
                check=True,
                timeout=60,
            )
        else:
            token = self.environment[
                "CONTEXT_GUEST_AUTHORIZATION" if guest else "CONTEXT_AUTHORIZATION"
            ]
            discover(host, catalog, {"Authorization": token})
        return Session(self.campaign, alias, alias, timeout=280)

    def cleanup(self):
        self.stop_http()
        for log in self.logs:
            log.close()
        orphans = cleanup(self.marker)
        subprocess.run(
            [*self.compose, "down", "--volumes", "--remove-orphans"],
            env=self.docker_environment,
            check=True,
            timeout=120,
        )
        remaining = subprocess.run(
            [*self.compose, "ps", "--all", "--quiet"],
            env=self.docker_environment,
            capture_output=True,
            text=True,
            check=True,
            timeout=30,
        ).stdout
        require(not remaining.strip(), "campaign backing services survived teardown")
        write_json(
            self.directory / "cleanup.json",
            {
                "project": self.marker,
                "removed_owned_orphans": orphans,
                "services_removed": True,
            },
        )
