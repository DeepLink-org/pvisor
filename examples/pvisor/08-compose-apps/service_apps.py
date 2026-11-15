"""Document, message-broker and monitoring workflows against real services."""

import base64
import hashlib
import json
import time
import urllib.parse


def couchdb(deployment):
    original = deployment.up(completed=("init",))
    auth = ("composeadmin", deployment.env["CASE_ADMIN_PASSWORD"])
    _, headers, _ = deployment.wait_http("/_up", '"ok"', auth=auth)
    assert headers.get("X-Pvisor-Proxy") == "nginx"
    process_uid = r"""
for file in /proc/[0-9]*/comm; do
    read -r name < "$file" || continue
    if [ "$name" = beam.smp ]; then
        awk '/^Uid:/ { print $2, $3, $4, $5 }' "${file%/comm}/status"
    fi
done
"""

    def verify_uid():
        observed = deployment.compose(
            "exec", "-T", "--interactive=false", "couchdb", "sh", "-c", process_uid
        ).stdout.splitlines()
        assert observed and all(row.split() == ["5984"] * 4 for row in observed), observed

    verify_uid()
    deployment.result["database_process_uid"] = 5984
    assert deployment.request("/_all_dbs", allow_error=True, use_cookies=False)[0] == 401
    assert deployment.request("/compose-proof", {}, method="PUT", auth=auth)[0] == 201
    path = "/compose-proof/document"
    _, _, created = deployment.request(
        path, {"kind": "compose", "value": 42}, method="PUT", auth=auth
    )
    first_revision = json.loads(created)["rev"]
    payload = bytes(range(256)) * 256
    _, _, attached = deployment.request(
        path + "/proof.bin?rev=" + first_revision, payload, method="PUT", auth=auth
    )
    revision = json.loads(attached)["rev"]
    assert deployment.request(path + "/proof.bin", auth=auth, binary=True)[2] == payload
    stale = deployment.request(
        path, {"_rev": first_revision, "value": -1}, method="PUT", auth=auth, allow_error=True
    )
    assert stale[0] == 409, "stale document revisions must not overwrite data"
    _, _, found = deployment.request(
        "/compose-proof/_find", {"selector": {"kind": "compose", "value": 42}}, auth=auth
    )
    assert [doc["_id"] for doc in json.loads(found)["docs"]] == ["document"]
    deployment.result.update(
        payload_bytes=len(payload),
        payload_sha256=hashlib.sha256(payload).hexdigest(),
        document_revision=revision,
    )
    deployment.check(
        "CouchDB document, binary attachment and Mango query pass through Nginx; stale revision conflicts and anonymous access are rejected"
    )
    deployment.recreate(original, completed=("init",))
    deployment.wait_http("/_up", '"ok"', auth=auth)
    verify_uid()
    _, _, restored = deployment.request(path, auth=auth)
    assert json.loads(restored)["_rev"] == revision
    assert json.loads(restored)["value"] == 42
    assert deployment.request(path + "/proof.bin", auth=auth, binary=True)[2] == payload
    deployment.check(
        "CouchDB document revision and exact attachment bytes persist after database and proxy recreation"
    )
    assert deployment.request(path + "?rev=" + revision, method="DELETE", auth=auth)[0] == 200
    assert deployment.request(path, auth=auth, allow_error=True)[0] == 404
    deployment.check("CouchDB revision-qualified deletion removes the document")


def rabbitmq(deployment):
    original = deployment.up()
    auth = ("composeadmin", deployment.env["CASE_ADMIN_PASSWORD"])
    _, headers, _ = deployment.wait_http("/api/overview", '"rabbitmq_version"', auth=auth)
    assert headers.get("X-Pvisor-Proxy") == "nginx"
    assert deployment.request("/api/overview", allow_error=True, use_cookies=False)[0] == 401
    queue = "/api/queues/%2F/compose-proof"
    response = deployment.request(
        queue, {"durable": True, "auto_delete": False, "arguments": {}}, method="PUT", auth=auth
    )
    assert response[0] == 201
    payload = bytes(range(256)) * 256
    message = {
        "properties": {"delivery_mode": 2, "content_type": "application/octet-stream"},
        "routing_key": "compose-proof",
        "payload_encoding": "base64",
        "payload": base64.b64encode(payload).decode(),
    }
    _, _, published = deployment.request(
        "/api/exchanges/%2F/amq.default/publish", message, auth=auth
    )
    assert json.loads(published)["routed"] is True

    def consume(requeue):
        _, _, body = deployment.request(
            queue + "/get",
            {
                "count": 1,
                "ackmode": "ack_requeue_true" if requeue else "ack_requeue_false",
                "encoding": "base64",
                "truncate": len(payload) + 1,
            },
            auth=auth,
        )
        messages = json.loads(body)
        assert len(messages) == 1, "durable message was not available"
        assert base64.b64decode(messages[0]["payload"], validate=True) == payload
        assert messages[0]["properties"]["delivery_mode"] == 2
        return messages[0]

    consume(True)
    deployment.result.update(
        payload_bytes=len(payload),
        payload_sha256=hashlib.sha256(payload).hexdigest(),
        message_transport="RabbitMQ management HTTP API, not AMQP client",
    )
    deployment.check(
        "RabbitMQ durable queue accepts a persistent binary message; management HTTP consume/requeue and anonymous rejection pass"
    )
    deployment.recreate(original)
    deployment.wait_http("/api/overview", '"rabbitmq_version"', auth=auth)
    consume(False)
    deployment.check(
        "RabbitMQ durable queue and exact persistent message survive broker and Nginx recreation"
    )
    _, _, empty = deployment.request(
        queue + "/get",
        {"count": 1, "ackmode": "ack_requeue_false", "encoding": "base64"},
        auth=auth,
    )
    assert json.loads(empty) == []
    assert deployment.request(queue, method="DELETE", auth=auth)[0] == 204
    deployment.check("RabbitMQ acknowledged delivery empties the queue; queue deletion succeeds")


def monitoring(deployment):
    original = deployment.up(completed=("init",))
    auth = ("composeadmin", deployment.env["CASE_ADMIN_PASSWORD"])
    deployment.wait_http("/api/health", '"ok"')
    datasource = {
        "uid": "compose-prometheus",
        "name": "Compose Prometheus",
        "type": "prometheus",
        "url": "http://prometheus:9090",
        "access": "proxy",
        "isDefault": True,
    }
    assert deployment.request("/api/datasources", datasource, auth=auth)[0] == 200
    proxy = "/api/datasources/proxy/uid/compose-prometheus"

    def query(at=None):
        params = {"query": 'process_start_time_seconds{job="prometheus"}'}
        if at is not None:
            params["time"] = str(at)
        _, _, body = deployment.request(
            proxy + "/api/v1/query?" + urllib.parse.urlencode(params), auth=auth
        )
        value = json.loads(body)
        assert value["status"] == "success"
        return value["data"]["result"]

    deadline = time.monotonic() + 90
    while True:
        samples = query()
        if len(samples) == 1:
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("Prometheus did not scrape its process metric within 90 seconds")
        time.sleep(1)
    timestamp, started = samples[0]["value"]
    assert float(started) > 0
    dashboard = {
        "dashboard": {
            "id": None,
            "uid": "compose-monitoring",
            "title": "Real Prometheus samples",
            "schemaVersion": 39,
            "panels": [
                {
                    "id": 1,
                    "type": "timeseries",
                    "title": "Prometheus process start",
                    "datasource": {"type": "prometheus", "uid": "compose-prometheus"},
                    "targets": [
                        {"refId": "A", "expr": 'process_start_time_seconds{job="prometheus"}'}
                    ],
                }
            ],
        },
        "overwrite": False,
    }
    assert deployment.request("/api/dashboards/db", dashboard, auth=auth)[0] == 200
    denied = deployment.request("/api/datasources", allow_error=True, use_cookies=False)[0]
    assert denied in (401, 403), denied
    deployment.result.update(historical_query_time=timestamp, original_process_start=started)
    deployment.check(
        "successful init service enables non-root Prometheus storage; Grafana proxy queries real self-scraped metrics over project DNS"
    )
    deployment.recreate(original, completed=("init",))
    deployment.wait_http("/api/health", '"ok"')
    _, _, restored = deployment.request("/api/dashboards/uid/compose-monitoring", auth=auth)
    restored = json.loads(restored)["dashboard"]
    assert restored["panels"][0]["datasource"]["uid"] == "compose-prometheus"
    assert query(timestamp)[0]["value"] == [timestamp, started], "old TSDB sample was lost"
    deadline = time.monotonic() + 90
    while True:
        latest = query()
        if latest and float(latest[0]["value"][1]) > float(started):
            break
        if time.monotonic() >= deadline:
            raise RuntimeError("recreated Prometheus did not produce a fresh process-start sample")
        time.sleep(1)
    deployment.result["recreated_process_start"] = latest[0]["value"][1]
    deployment.check(
        "Grafana datasource/dashboard and historical Prometheus WAL samples persist; recreated Prometheus produces a newer process-start metric"
    )
