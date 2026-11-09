#!/usr/bin/env python3
"""Local infrastructure smoke test; no Shiki application or IAM assertions."""
import argparse
import base64
import json
import time
import urllib.parse
import urllib.request
import uuid

BASE = "http://127.0.0.1:4588"
BUCKET = "shiki-local-blobs"
# Never send local emulator requests through an inherited outbound proxy.
HTTP = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def request(method, path, body=None, content_type="application/json"):
    req = urllib.request.Request(
        BASE + path, data=body, method=method,
        headers={"Content-Type": content_type},
    )
    with HTTP.open(req, timeout=15) as response:
        return response.read()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pubsub", action="store_true")
    args = parser.parse_args()
    cluster = json.loads(request(
        "GET", "/container/v1/projects/shiki-local/locations/us-central1/clusters/shiki-local"
    ))
    assert cluster["status"] == "RUNNING", cluster
    print("PASS: GKE management API (Kubernetes execution NOT tested by this script)")

    name = "smoke/" + uuid.uuid4().hex + ".txt"
    encoded = urllib.parse.quote(name, safe="")
    content = ("shiki-infrastructure-smoke:" + name).encode()
    request("POST", f"/upload/storage/v1/b/{BUCKET}/o?uploadType=media&name={encoded}",
            content, "text/plain")
    try:
        assert request("GET", f"/storage/v1/b/{BUCKET}/o/{encoded}?alt=media") == content
        print("PASS: GCS object upload/download")
        if args.pubsub:
            sub = "/v1/projects/shiki-local/subscriptions/shiki-local-object-events-smoke"
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                result = json.loads(request("POST", sub + ":pull", b'{"maxMessages":100}'))
                for item in result.get("receivedMessages", []):
                    message = item["message"]
                    event = json.loads(base64.b64decode(message["data"]))
                    if event.get("name") == name:
                        assert event["bucket"] == BUCKET, event
                        assert message["attributes"]["eventType"] == "OBJECT_FINALIZE", message
                        request("POST", sub + ":acknowledge",
                                json.dumps({"ackIds": [item["ackId"]]}).encode())
                        print("PASS: GCS OBJECT_FINALIZE -> Pub/Sub pull/ack")
                        return
                time.sleep(0.5)
            raise TimeoutError("GCS notification not received within 30 seconds")
    finally:
        request("DELETE", f"/storage/v1/b/{BUCKET}/o/{encoded}")
    print("Pub/Sub experiment disabled (Shiki uses Postgres jobq)")


if __name__ == "__main__":
    main()
