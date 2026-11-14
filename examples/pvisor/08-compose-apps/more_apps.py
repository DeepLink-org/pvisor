"""WebDAV, S3 and task-queue business assertions for the real deployment runner."""

import hashlib
import hmac
import json
import time
import urllib.parse
import xml.etree.ElementTree as ET
from datetime import datetime, timezone


def nextcloud(deployment):
    original = deployment.up()
    started = time.monotonic()
    budget = deployment.nextcloud_install_timeout
    deployment.result["installation_timeout_seconds"] = budget
    deadline = started + budget
    diagnostic = started + 60
    while True:
        try:
            _, _, status = deployment.request("/status.php")
            ready = json.loads(status)
            if ready.get("installed") and not ready.get("maintenance"):
                break
        except (OSError, RuntimeError, ValueError):
            pass
        if time.monotonic() >= diagnostic:
            deployment.log(f"Nextcloud initialization elapsed={time.monotonic() - started:.1f}s")
            deployment.compose(
                "exec",
                "-T",
                "--interactive=false",
                "nextcloud",
                "sh",
                "-c",
                "ps -eo pid,stat,etime,comm; find /var/www/html -type f | wc -l",
                timeout=30,
                check=False,
            )
            diagnostic = time.monotonic() + 60
        if time.monotonic() >= deadline:
            raise RuntimeError(f"Nextcloud did not finish installation within {budget} seconds")
        time.sleep(1)
    deployment.result["installation_elapsed_seconds"] = round(time.monotonic() - started, 3)
    deployment.check("Nextcloud auto-install completed with PostgreSQL and Redis")
    # Exercise Nextcloud's own database and cache adapters, rather than infer
    # integration merely from configured environment or standalone health probes.
    integrations = r"""
define('OC_CONSOLE', true);
require '/var/www/html/lib/base.php';
$db = \OC::$server->get(\OCP\IDBConnection::class);
$version = $db->executeQuery('SELECT version()')->fetchOne();
if (!str_starts_with($version, 'PostgreSQL')) { throw new \RuntimeException('not PostgreSQL'); }
$cache = new \OC\Memcache\Redis('compose-acceptance');
$cache->set('proof', 'nextcloud-redis-roundtrip', 60);
if ($cache->get('proof') !== 'nextcloud-redis-roundtrip') { throw new \RuntimeException('Redis roundtrip failed'); }
$cache->remove('proof');
echo 'nextcloud-postgresql-redis-ok';
"""
    observed = deployment.compose(
        "exec",
        "-T",
        "--interactive=false",
        "-u",
        "www-data",
        "nextcloud",
        "php",
        "-r",
        integrations,
    )
    assert observed.stdout.strip() == "nextcloud-postgresql-redis-ok"
    deployment.check(
        "Nextcloud adapters query PostgreSQL and perform a Redis cache write/read/delete"
    )
    auth = ("composeadmin", deployment.env["CASE_ADMIN_PASSWORD"])
    path = "/remote.php/dav/files/composeadmin/compose-proof.bin"
    payload = bytes(range(256)) * 256
    response = deployment.request(path, payload, auth=auth, method="PUT")
    assert response[0] in (201, 204), response[0]
    assert deployment.request(path, auth=auth, binary=True)[2] == payload
    properties = deployment.request(path, b"", auth=auth, method="PROPFIND", headers={"Depth": "0"})
    assert properties[0] == 207
    assert "compose-proof.bin" in properties[2]
    denied = deployment.request(path, allow_error=True, use_cookies=False)
    assert denied[0] == 401, denied[0]
    deployment.result["payload_sha256"] = hashlib.sha256(payload).hexdigest()
    deployment.result["payload_bytes"] = len(payload)
    deployment.check(
        "authenticated WebDAV PUT/GET/PROPFIND preserves binary bytes; anonymous access denied"
    )
    deployment.recreate(original)
    deployment.wait_http("/status.php", '"installed"')
    assert deployment.request(path, auth=auth, binary=True)[2] == payload
    deployment.check("Nextcloud file and database metadata persist across full stack recreation")
    assert deployment.request(path, auth=auth, method="DELETE")[0] == 204
    assert deployment.request(path, auth=auth, allow_error=True)[0] == 404
    deployment.check("WebDAV deletion removes the uploaded object")


def s3(deployment, method, path, payload=b"", query=None, allow_error=False):
    """AWS Signature V4, path-style S3, fixed region and SHA-256 payload signing."""
    now = datetime.now(timezone.utc)
    date = now.strftime("%Y%m%d")
    timestamp = now.strftime("%Y%m%dT%H%M%SZ")
    host = urllib.parse.urlsplit(deployment.base).netloc
    uri = urllib.parse.quote(path, safe="/-_.~")
    query_string = "&".join(
        urllib.parse.quote(key, safe="-_.~") + "=" + urllib.parse.quote(value, safe="-_.~")
        for key, value in sorted((query or {}).items())
    )
    digest = hashlib.sha256(payload).hexdigest()
    signed_headers = "host;x-amz-content-sha256;x-amz-date"
    canonical_headers = f"host:{host}\nx-amz-content-sha256:{digest}\nx-amz-date:{timestamp}\n"
    canonical = "\n".join((method, uri, query_string, canonical_headers, signed_headers, digest))
    scope = f"{date}/us-east-1/s3/aws4_request"
    to_sign = (
        "AWS4-HMAC-SHA256\n"
        + timestamp
        + "\n"
        + scope
        + "\n"
        + hashlib.sha256(canonical.encode()).hexdigest()
    )
    key = ("AWS4" + deployment.env["CASE_ADMIN_PASSWORD"]).encode()
    for value in (date, "us-east-1", "s3", "aws4_request"):
        key = hmac.new(key, value.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    headers = {
        "Host": host,
        "X-Amz-Date": timestamp,
        "X-Amz-Content-Sha256": digest,
        "Authorization": f"AWS4-HMAC-SHA256 Credential=composeadmin/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
    }
    url = uri + ("?" + query_string if query_string else "")
    return deployment.request(
        url,
        payload if method in ("PUT", "POST") else None,
        method=method,
        headers=headers,
        binary=True,
        allow_error=allow_error,
        timeout=30,
    )


def minio(deployment):
    original = deployment.up()
    _, headers, _ = deployment.wait_http("/minio/health/ready", "")
    assert headers.get("X-Pvisor-Proxy") == "nginx"
    deployment.check("MinIO ready endpoint is reachable through Nginx and project DNS")
    object_storage_checks(deployment, original, "MinIO")


def object_storage_checks(deployment, original, product):
    assert s3(deployment, "PUT", "/compose-proof")[0] == 200
    path = "/compose-proof/folder/compose-proof.bin"
    payload = bytes(range(256)) * 4096
    assert s3(deployment, "PUT", path, payload)[0] == 200
    downloaded = s3(deployment, "GET", path)[2]
    assert downloaded == payload
    listing = s3(deployment, "GET", "/compose-proof", query={"list-type": "2"})[2]
    keys = [
        node.text
        for node in ET.fromstring(listing).iter()
        if node.tag.endswith("}Key") or node.tag == "Key"
    ]
    assert "folder/compose-proof.bin" in keys
    assert deployment.request(path, binary=True, allow_error=True, use_cookies=False)[0] == 403
    deployment.result["payload_sha256"] = hashlib.sha256(payload).hexdigest()
    deployment.result["payload_bytes"] = len(payload)
    deployment.check(
        "signed S3 bucket creation, binary PUT/GET and ListObjectsV2 pass; anonymous access denied"
    )
    deployment.recreate(original)
    wait_s3(deployment)
    assert s3(deployment, "GET", path)[2] == payload
    deployment.check(f"{product} bucket and 1 MiB object persist after proxy and server recreation")
    assert s3(deployment, "DELETE", path)[0] == 204
    assert s3(deployment, "GET", path, allow_error=True)[0] == 404
    deployment.check("signed S3 deletion removes the stored object")


def wait_s3(deployment):
    deadline = time.monotonic() + 150
    last = "no response"
    while time.monotonic() < deadline:
        try:
            response = s3(deployment, "GET", "/", allow_error=True)
            if response[0] == 200:
                ET.fromstring(response[2])
                return response
            last = f"HTTP {response[0]}: {response[2][:1024]!r}"
        except (OSError, RuntimeError, ET.ParseError) as error:
            last = str(error)
        time.sleep(1)
    raise RuntimeError(f"signed S3 readiness timeout: {last}")


def seaweedfs(deployment):
    original = deployment.up()
    _, headers, _ = wait_s3(deployment)
    assert headers.get("X-Pvisor-Proxy") == "nginx"
    deployment.check("SeaweedFS S3 gateway accepts signed requests through Nginx and project DNS")
    object_storage_checks(deployment, original, "SeaweedFS")


def celery(deployment):
    original = deployment.up()
    auth = ("composeadmin", deployment.env["CASE_ADMIN_PASSWORD"])
    deadline = time.monotonic() + 150
    last = "no worker observation"
    while True:
        try:
            # Flower's initial inspect can precede worker readiness. The API
            # defaults to cached observations; explicitly request a new inspect.
            _, _, workers = deployment.request("/api/workers?refresh=1", auth=auth)
            if json.loads(workers):
                break
            last = "worker list is empty"
        except (OSError, RuntimeError, ValueError) as error:
            last = str(error)
        if time.monotonic() >= deadline:
            raise RuntimeError(
                f"Flower did not discover a Celery worker within 150 seconds: {last}"
            )
        time.sleep(1)
    assert deployment.request("/api/workers", allow_error=True, use_cookies=False)[0] == 401
    deployment.check(
        "authenticated Flower API discovers a real worker; anonymous API access denied"
    )
    submitted = deployment.compose(
        "exec",
        "-T",
        "--interactive=false",
        "worker",
        "python",
        "-c",
        "from tasks import add; import json; r=add.delay(20,22); value=r.get(timeout=45); "
        "assert value==42; print(json.dumps({'id':r.id,'result':value}))",
        timeout=70,
    )
    task = json.loads(submitted.stdout.strip())
    deployment.result["task_id"] = task["id"]
    deployment.check("Celery prefork worker executes a Redis-brokered task and stores result 42")
    deployment.recreate(original)
    deployment.wait_http("/", "Flower", auth=auth)
    restored = deployment.compose(
        "exec",
        "-T",
        "--interactive=false",
        "worker",
        "python",
        "-c",
        "from tasks import app; r=app.AsyncResult('" + task["id"] + "'); "
        "assert r.state=='SUCCESS', r.state; assert r.get(timeout=15)==42; print('persisted-result=42')",
        timeout=30,
    )
    assert "persisted-result=42" in restored.stdout
    deployment.check("Redis AOF retains the task result after broker, worker and Flower recreation")
