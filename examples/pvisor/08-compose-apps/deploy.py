#!/usr/bin/env python3
"""Deploy real applications through pvisor compose; only Python stdlib is needed."""

import argparse
import base64
import hashlib
import http.cookiejar
import json
import os
import re
import secrets
import shutil
import socket
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

from more_apps import celery, minio, nextcloud, seaweedfs
from service_apps import couchdb, monitoring, rabbitmq

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[2]


class Deployment:
    def __init__(
        self,
        binary,
        root,
        case,
        sockets_in_shm=False,
        minio_manifest=None,
        nextcloud_install_timeout=150,
    ):
        self.binary = binary
        self.root = root / case
        self.root.mkdir(mode=0o700)
        self.case = case
        self.minio_manifest = minio_manifest if case == "minio" else None
        self.nextcloud_install_timeout = nextcloud_install_timeout
        self.env = dict(os.environ)
        if self.minio_manifest:
            self.env["CASE_MINIO_IMAGE"] = "pvisor-local-minio:source"
        # Deliberate, separately reported application configuration workaround.
        # Keep default socket paths unless the caller explicitly requests it.
        self.env.pop("CASE_DB_SOCKET_DIR", None)
        if sockets_in_shm:
            self.env["CASE_DB_SOCKET_DIR"] = "/dev/shm"
        for key in ("CASE_DB_PASSWORD", "CASE_DB_ROOT_PASSWORD", "CASE_ADMIN_PASSWORD"):
            self.env[key] = secrets.token_urlsafe(24)
        self.secret_values = [
            self.env[key]
            for key in ("CASE_DB_PASSWORD", "CASE_DB_ROOT_PASSWORD", "CASE_ADMIN_PASSWORD")
        ]
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]
        self.env["CASE_HTTP_PORT"] = str(self.port)
        self.env["PVISOR_DOCKER_HOME"] = str(self.root / "store")
        self.env.setdefault("PVISOR_IMAGE_STORE", str(REPO / "target/compose-acceptance/images"))
        self.file = self.root / "compose.yaml"
        shutil.copyfile(HERE / f"{case}.yaml", self.file)
        for extra in {
            "grafana": ("nginx.conf",),
            "minio": ("nginx-s3.conf",),
            "celery": ("tasks.py",),
            "seaweedfs": ("nginx-seaweed.conf",),
            "couchdb": ("couchdb.ini", "nginx-couchdb.conf"),
            "rabbitmq": ("rabbitmq.conf", "nginx-rabbitmq.conf"),
            "monitoring": ("prometheus.yml",),
        }.get(case, ()):
            shutil.copyfile(HERE / extra, self.root / extra)
        if case == "seaweedfs":
            # A generated credential file lives only under this ignored 0700
            # test directory. Never place resolved credentials in source YAML.
            credentials = {
                "identities": [
                    {
                        "name": "composeadmin",
                        "credentials": [
                            {
                                "accessKey": "composeadmin",
                                "secretKey": self.env["CASE_ADMIN_PASSWORD"],
                            }
                        ],
                        "actions": ["Admin", "Read", "Write", "List", "Tagging"],
                    }
                ]
            }
            fd = os.open(self.root / "s3.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(fd, "w") as stream:
                json.dump(credentials, stream)
        self.project = f"real-{case}-{secrets.token_hex(3)}"
        self.base = f"http://127.0.0.1:{self.port}"
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()),
        )
        self.result = {
            "case": case,
            "project": self.project,
            "database_socket_variant": "dev-shm" if sockets_in_shm else "default",
            "compose_sha256": hashlib.sha256(self.file.read_bytes()).hexdigest(),
            "platform": "linux/KVM",
            "checks": [],
            "outcome": "running",
            "endpoint": self.base,
            "images": [],
        }

    def redact(self, text):
        for value in self.secret_values:
            text = text.replace(value, "[REDACTED]")
        return text

    def log(self, text):
        with (self.root / "commands.log").open("a") as stream:
            stream.write(self.redact(text) + "\n")

    def command(self, args, timeout=600, check=True):
        # Never log resolved config/environment or HTTP authorization headers.
        self.log("$ pvisor " + " ".join(args))
        completed = subprocess.run(
            [str(self.binary), *args],
            env=self.env,
            cwd=self.root,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
        self.log(f"exit={completed.returncode}\n{completed.stdout}\n{completed.stderr}")
        if check and completed.returncode:
            raise RuntimeError(
                self.redact(
                    f"pvisor {' '.join(args)}: exit {completed.returncode}\n{completed.stderr}"
                )
            )
        return completed

    def compose(self, *args, **kwargs):
        return self.command(["compose", "-f", str(self.file), "-p", self.project, *args], **kwargs)

    def check(self, description):
        self.result["checks"].append(description)
        print(f"[{self.case}] PASS {description}", flush=True)

    def records(self):
        return [
            json.loads(path.read_text())
            for path in (self.root / "store/instances").glob("*/instance.json")
        ]

    def pull(self):
        self.compose("config", "-q")
        self.check("adapted Compose configuration validates")
        if self.minio_manifest:
            manifest = json.loads(self.minio_manifest.read_text())
            binary = Path(manifest["binary_path"])
            if manifest["schema"] != 1:
                raise ValueError("unsupported MinIO build manifest schema")
            if hashlib.sha256(binary.read_bytes()).hexdigest() != manifest["binary_sha256"]:
                raise ValueError("MinIO build binary SHA-256 mismatch")
            context = self.root / "minio-image"
            context.mkdir(mode=0o700)
            shutil.copyfile(binary, context / "minio")
            (context / "minio").chmod(0o755)
            shutil.copyfile(manifest["license_path"], context / "LICENSE")
            (context / "Dockerfile").write_text(
                "FROM alpine:3.20\nCOPY minio /usr/local/bin/minio\n"
                "COPY LICENSE /licenses/LICENSE.minio\n"
                'ENTRYPOINT ["/usr/local/bin/minio"]\nCMD ["server", "/data"]\n'
            )
            self.command(["build", "-t", self.env["CASE_MINIO_IMAGE"], str(context)], timeout=900)
            self.result["minio_source_build"] = {
                key: manifest[key]
                for key in (
                    "source_tag",
                    "source_url",
                    "source_sha256",
                    "go_mod_sha256",
                    "binary_sha256",
                    "go_version",
                )
            }
            self.check("locally built upstream MinIO source packaged with verified binary SHA-256")
            self.compose("pull", "nginx", timeout=900)
        else:
            self.compose("pull", timeout=900)
        catalog = self.root / "store/images.json"
        self.result["images"] = [
            {key: image[key] for key in ("reference", "digest", "platform")}
            for image in json.loads(catalog.read_text())
        ]
        self.check("application stack images pulled")

    def up(self, completed=()):
        self.compose("up", "--wait", "--wait-timeout", "180", timeout=300)
        self.compose("ps", "-a")
        records = self.records()
        assert records
        observed = set()
        for record in records:
            service = record["compose"]["service"]
            if service in completed:
                assert record["state"] == "exited" and record["exit_code"] == 0, service
                observed.add(service)
            else:
                assert record["state"] == "running", service
        assert observed == set(completed), "expected completed initialization services absent"
        return {record["id"] for record in records}

    def request(
        self,
        path,
        data=None,
        auth=None,
        form=False,
        timeout=10,
        method=None,
        headers=None,
        binary=False,
        allow_error=False,
        use_cookies=True,
    ):
        headers = dict(headers or {})
        if auth:
            token = base64.b64encode(f"{auth[0]}:{auth[1]}".encode()).decode()
            headers["Authorization"] = f"Basic {token}"
        if data is not None:
            if isinstance(data, bytes):
                headers.setdefault("Content-Type", "application/octet-stream")
            elif form:
                data = urllib.parse.urlencode(data).encode()
                headers["Content-Type"] = "application/x-www-form-urlencoded"
            else:
                data = json.dumps(data).encode()
                headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            self.base + path, data=data, headers=headers, method=method
        )
        opener = (
            self.opener
            if use_cookies
            else urllib.request.build_opener(urllib.request.ProxyHandler({}))
        )
        try:
            with opener.open(request, timeout=timeout) as response:
                body = response.read()
                return response.status, dict(response.headers), body if binary else body.decode()
        except urllib.error.HTTPError as error:
            body = error.read(4096)
            if allow_error:
                return (
                    error.code,
                    dict(error.headers),
                    body if binary else body.decode(errors="replace"),
                )
            body = body.decode(errors="replace")
            raise RuntimeError(self.redact(f"HTTP {error.code} {path}: {body}")) from None

    def wait_http(self, path, marker, auth=None):
        deadline = time.monotonic() + 150
        last = "no response"
        while time.monotonic() < deadline:
            try:
                response = self.request(path, auth=auth)
                if marker in response[2]:
                    return response
                last = f"HTTP {response[0]}, expected marker absent"
            except (OSError, RuntimeError) as error:
                last = str(error)
            time.sleep(1)
        raise RuntimeError(self.redact(f"HTTP readiness timeout {path}: {last}"))

    def recreate(self, original, completed=()):
        self.compose("down", "-t", "10", timeout=120)
        assert not self.records(), "down left container records"
        current = self.up(completed=completed)
        assert original.isdisjoint(current), "persistence check did not recreate containers"
        self.check("down/up created new containers while retaining named volumes")

    def cleanup(self):
        try:
            self.compose("logs", timeout=45, check=False)
        except (OSError, subprocess.TimeoutExpired) as error:
            self.log(f"log collection: {error}")
        try:
            completed = self.compose("down", "-v", "-t", "5", timeout=120, check=False)
            volumes = self.root / "store/volumes"
            remaining = sorted(path.name for path in volumes.iterdir()) if volumes.exists() else []
            self.result["remaining_volume_directories"] = remaining
            clean = completed.returncode == 0 and not self.records() and not remaining
            self.result["cleanup"] = "pass" if clean else "fail"
            if clean and self.case == "seaweedfs":
                (self.root / "s3.json").unlink(missing_ok=True)
        except (OSError, subprocess.TimeoutExpired) as error:
            self.result["cleanup"] = "fail"
            self.result["cleanup_error"] = self.redact(str(error))
        if self.result["cleanup"] != "pass":
            self.result["outcome"] = "fail"


def wordpress(deployment):
    original = deployment.up()
    deployment.wait_http("/wp-admin/install.php", "WordPress")
    _, _, installed = deployment.request(
        "/wp-admin/install.php?step=2",
        {
            "weblog_title": "pVisor real Compose deployment",
            "user_name": "composeadmin",
            "admin_password": deployment.env["CASE_ADMIN_PASSWORD"],
            "admin_password2": deployment.env["CASE_ADMIN_PASSWORD"],
            "admin_email": "compose@example.invalid",
            "blog_public": "0",
            "pw_weak": "1",
            "Submit": "Install WordPress",
        },
        form=True,
    )
    assert "wordpress has been installed" in installed.lower(), (
        "WordPress HTTP installation did not report success"
    )
    deployment.check("WordPress HTTP installer completed against MariaDB")
    php = (
        "require '/var/www/html/wp-load.php';"
        "$id=wp_insert_post(['post_title'=>'Compose deployment proof',"
        "'post_content'=>'pvisor-wordpress-persisted-content','post_status'=>'publish'],true);"
        "if(is_wp_error($id)){fwrite(STDERR,$id->get_error_message());exit(1);}echo $id;"
    )
    completed = deployment.compose(
        "exec", "-T", "--interactive=false", "wordpress", "php", "-r", php
    )
    post = int(completed.stdout.strip())
    deployment.wait_http(f"/?p={post}", "pvisor-wordpress-persisted-content")
    deployment.check("published a real WordPress post and read it over HTTP")
    deployment.recreate(original)
    deployment.wait_http(f"/?p={post}", "pvisor-wordpress-persisted-content")
    deployment.check("WordPress post persists after database and web container recreation")


def gitea(deployment):
    original = deployment.up()
    deployment.wait_http("/api/healthz", "pass")
    _, _, signup = deployment.request("/user/sign_up")
    match = re.search(r'name="_csrf"\s+value="([^"]+)"', signup)
    assert match, "Gitea registration form did not contain CSRF token"
    password = deployment.env["CASE_ADMIN_PASSWORD"]
    deployment.request(
        "/user/sign_up",
        {
            "_csrf": match.group(1),
            "user_name": "composeuser",
            "email": "compose@example.invalid",
            "password": password,
            "retype": password,
        },
        form=True,
    )
    auth = ("composeuser", password)
    _, _, user = deployment.request("/api/v1/user", auth=auth)
    assert json.loads(user)["login"] == "composeuser"
    deployment.check("registered a real Gitea user through its HTTP form")
    started = time.monotonic()
    deployment.result["repository_create_http_timeout_seconds"] = 60
    try:
        _, _, created = deployment.request(
            "/api/v1/user/repos",
            {
                "name": "compose-proof",
                "description": "pvisor-gitea-persisted-repository",
                "private": False,
                "auto_init": True,
            },
            auth=auth,
            timeout=60,
        )
    except TimeoutError:
        deployment.compose(
            "exec", "-T", "--interactive=false", "gitea", "ps", "-ef", timeout=20, check=False
        )
        raise
    finally:
        deployment.result["repository_create_seconds"] = round(time.monotonic() - started, 3)
    repository = json.loads(created)
    assert repository["full_name"] == "composeuser/compose-proof"
    _, _, commits = deployment.request("/api/v1/repos/composeuser/compose-proof/commits", auth=auth)
    assert json.loads(commits), "repository has no initial commit"
    deployment.check("Gitea API created a repository with an initial Git commit")
    deployment.recreate(original)
    deployment.wait_http("/api/healthz", "pass")
    _, _, restored = deployment.request("/api/v1/repos/composeuser/compose-proof", auth=auth)
    assert json.loads(restored)["description"] == "pvisor-gitea-persisted-repository"
    _, _, restored_commits = deployment.request(
        "/api/v1/repos/composeuser/compose-proof/commits", auth=auth
    )
    assert json.loads(restored_commits)[0]["sha"] == json.loads(commits)[0]["sha"]
    deployment.check("PostgreSQL repository metadata and Git commit persist after down/up")


def grafana(deployment):
    original = deployment.up()
    _, headers, health = deployment.wait_http("/api/health", '"database"')
    assert json.loads(health)["database"] == "ok"
    assert headers.get("X-Pvisor-Proxy") == "nginx"
    deployment.check("Grafana SQLite health is reachable through Nginx and service DNS")
    auth = ("composeadmin", deployment.env["CASE_ADMIN_PASSWORD"])
    _, _, saved = deployment.request(
        "/api/dashboards/db",
        {
            "dashboard": {
                "id": None,
                "uid": "pvisor-compose-proof",
                "title": "pVisor persisted Compose dashboard",
                "schemaVersion": 39,
                "panels": [],
            },
            "overwrite": False,
        },
        auth=auth,
    )
    assert json.loads(saved)["status"] == "success"
    deployment.check("authenticated Grafana API created a dashboard through the proxy")
    deployment.recreate(original)
    deployment.wait_http("/api/health", '"database"')
    _, _, restored = deployment.request("/api/dashboards/uid/pvisor-compose-proof", auth=auth)
    assert json.loads(restored)["dashboard"]["title"] == "pVisor persisted Compose dashboard"
    deployment.check("Grafana dashboard persists after Grafana and Nginx recreation")


def upstream_checks(binary, root):
    results = []
    sources = (
        (
            "wordpress-mysql",
            "https://raw.githubusercontent.com/docker/awesome-compose/master/wordpress-mysql/compose.yaml",
        ),
        (
            "flask-redis",
            "https://raw.githubusercontent.com/docker/awesome-compose/master/flask-redis/compose.yaml",
        ),
        (
            "nextcloud",
            "https://raw.githubusercontent.com/nextcloud/docker/master/.examples/docker-compose/insecure/postgres/apache/compose.yaml",
        ),
        (
            "minio-distributed",
            "https://raw.githubusercontent.com/minio/minio/master/docs/orchestration/docker-compose/docker-compose.yaml",
        ),
    )
    for case, url in sources:
        record = {"sample": case, "url": url, "check": "config only, not deployment"}
        try:
            data = urllib.request.urlopen(url, timeout=30).read()
            sample = root / f"upstream-{case}.yaml"
            sample.write_bytes(data)
            env = dict(os.environ, PVISOR_DOCKER_HOME=str(root / f"unused-{case}-store"))
            result = subprocess.run(
                [
                    str(binary),
                    "compose",
                    "-f",
                    str(sample),
                    "-p",
                    "original-sample",
                    "config",
                    "-q",
                ],
                env=env,
                capture_output=True,
                text=True,
                timeout=30,
            )
            record.update(
                sha256=hashlib.sha256(data).hexdigest(),
                exit_code=result.returncode,
                compatible=result.returncode == 0,
                diagnostic=result.stderr,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            record.update(compatible=None, diagnostic=str(error))
        results.append(record)
        print(f"[upstream {case}] config compatible={record['compatible']}", flush=True)
    return results


CASES = {
    "wordpress": wordpress,
    "gitea": gitea,
    "grafana": grafana,
    "nextcloud": nextcloud,
    "minio": minio,
    "celery": celery,
    "seaweedfs": seaweedfs,
    "couchdb": couchdb,
    "rabbitmq": rabbitmq,
    "monitoring": monitoring,
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/pvisor")
    parser.add_argument("--case", choices=CASES, action="append")
    parser.add_argument(
        "--database-sockets-in-shm",
        action="store_true",
        help="explicit workaround: put database Unix sockets on guest /dev/shm",
    )
    parser.add_argument(
        "--minio-build-manifest",
        type=Path,
        help="package the local upstream source build recorded by build_minio.py",
    )
    parser.add_argument(
        "--nextcloud-install-timeout",
        type=int,
        default=150,
        help="explicit Nextcloud installation readiness budget in seconds (default: 150)",
    )
    args = parser.parse_args()
    if args.nextcloud_install_timeout <= 0:
        parser.error("--nextcloud-install-timeout must be positive")
    binary = args.binary.resolve(strict=True)
    work = HERE / ".work"
    work.mkdir(mode=0o700, exist_ok=True)
    root = work / (time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(3))
    root.mkdir(mode=0o700)
    print(f"Report: {root / 'report.json'}", flush=True)
    report = {
        "binary": str(binary),
        "upstream_config_checks": upstream_checks(binary, root),
        "deployments": [],
    }
    for case in args.case or CASES:
        deployment = Deployment(
            binary,
            root,
            case,
            args.database_sockets_in_shm,
            args.minio_build_manifest,
            args.nextcloud_install_timeout,
        )
        started = time.monotonic()
        try:
            deployment.pull()
            CASES[case](deployment)
            deployment.result["outcome"] = "pass"
        except Exception as error:
            deployment.result["outcome"] = "fail"
            deployment.result["error"] = deployment.redact(str(error))
            print(f"[{case}] FAIL {deployment.result['error']}", flush=True)
        finally:
            deployment.cleanup()
            deployment.result["elapsed_seconds"] = round(time.monotonic() - started, 3)
            report["deployments"].append(deployment.result)
            (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(
            f"[{case}] outcome={deployment.result['outcome']} cleanup={deployment.result['cleanup']}",
            flush=True,
        )
    return 0 if all(result["outcome"] == "pass" for result in report["deployments"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
