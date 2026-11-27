#!/usr/bin/env python3
"""S3 layout microbenchmark, separate from native Rust codec/DB integration tests.

Synthetic data only. SDK/CPU timings are Python; layouts use the same client,
8 concurrent GETs, warm server caches. Range is deliberately a benchmark-only
capability: the application's ObjectStore trait is unchanged.
"""

import argparse
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor, wait
import hashlib
import json
import os
from pathlib import Path
import random
import time
from urllib.parse import urlparse
import uuid

import boto3
from botocore.config import Config
from fastcdc import fastcdc

from bench import pack, unpack


def histories(size, seed, edit):
    rng = random.Random(seed)
    data = rng.randbytes(size)
    yield data
    count = 32 if edit == "churn32" else 1
    for i in range(count):
        if edit == "rewrite":
            data = rng.randbytes(size)
        elif edit == "head":
            data = f"insert {i}\n".encode() + data
        else:
            data = bytearray(data)
            for _ in range(16 if edit == "scatter" else 1):
                length = len(data) // 4 if edit == "churn32" else 24
                pos = rng.randrange(len(data) - length)
                data[pos:pos + length] = rng.randbytes(length)
            data = bytes(data)
        yield data


def upload_all(pool, put, items):
    futures = []
    try:
        for item in items:
            futures.append(pool.submit(put, item))
        for future in futures:
            future.result()
    finally:
        # 例外時も、削除より先に遅延PUTを完了させる。
        wait(futures)


def run_case(client, bucket, pool, size, seed, edit, output):
    prefix = f"layout-bench/{uuid.uuid4()}/"
    pieces, packs, index = {}, {}, {}
    current = []
    for data in histories(size, seed, edit):
        refs, fresh = [], {}
        for c in fastcdc(data, min_size=4096, avg_size=16384, max_size=65536):
            encoded = pack(data[c.offset:c.offset + c.length])
            sha = hashlib.sha256(encoded).hexdigest()
            refs.append((sha, c.length))
            if sha not in pieces:
                fresh[sha] = encoded
                pieces[sha] = encoded
        if fresh:
            payload = b"".join(fresh.values())
            key = prefix + "packs/" + hashlib.sha256(payload).hexdigest()
            packs[key] = payload
            offset = 0
            for sha, encoded in fresh.items():
                index[sha] = (key, offset, len(encoded))
                offset += len(encoded)
        current = refs
    expected = data
    objects = {prefix + "pieces/" + sha: data for sha, data in pieces.items()}
    objects.update(packs)
    created = list(objects)

    def put(item):
        key, payload = item
        client.put_object(Bucket=bucket, Key=key, Body=payload)

    def get(item):
        key, start, end = item
        args = {"Bucket": bucket, "Key": key}
        if start is not None:
            args["Range"] = f"bytes={start}-{end - 1}"
        response = client.get_object(**args)
        body = response["Body"]
        try:
            payload = body.read()
        finally:
            body.close()
        if start is not None and (response["ResponseMetadata"]["HTTPStatusCode"] != 206 or len(payload) != end - start):
            raise RuntimeError("Range response mismatch")
        return key, start, payload

    try:
        upload_all(pool, put, objects.items())
        unique = set(sha for sha, _ in current)
        required = set(index[sha][0] for sha in unique)
        intervals = defaultdict(list)
        for sha in unique:
            key, offset, length = index[sha]
            intervals[key].append((offset, offset + length))
        ranges = []
        for key, spans in intervals.items():
            merged = []
            for start, end in sorted(spans):
                if merged and start <= merged[-1][1]:
                    merged[-1] = (merged[-1][0], max(merged[-1][1], end))
                else:
                    merged.append((start, end))
            ranges.extend((key, a, b) for a, b in merged)
        for trial in range(3):
            modes = ["pieces", "whole_pack", "range_pack"]
            modes = modes[trial:] + modes[:trial]
            for mode in modes:
                requests = ([(prefix + "pieces/" + sha, None, None) for sha in unique] if mode == "pieces" else
                            [(key, None, None) for key in required] if mode == "whole_pack" else ranges)
                start = time.perf_counter_ns()
                downloaded = list(pool.map(get, requests))
                parts = []
                if mode == "pieces":
                    values = {key: value for key, _, value in downloaded}
                    for sha, _ in current:
                        parts.append(unpack(values[prefix + "pieces/" + sha]))
                else:
                    for sha, _ in current:
                        key, offset, length = index[sha]
                        for pack_key, range_start, value in downloaded:
                            begin = range_start or 0
                            if key == pack_key and begin <= offset and offset + length <= begin + len(value):
                                parts.append(unpack(value[offset - begin:offset - begin + length]))
                                break
                        else:
                            raise RuntimeError("Missing segment")
                result = b"".join(parts)
                elapsed = (time.perf_counter_ns() - start) / 1e6
                if result != expected:
                    raise RuntimeError("Round-trip mismatch")
                row = {"size": size, "seed": seed, "edit": edit, "trial": trial, "layout": mode,
                       "restore_ms": elapsed, "gets": len(requests),
                       "downloaded_bytes": sum(len(v) for _, _, v in downloaded), "restored_bytes": len(result),
                       "stored_payload_bytes": sum(map(len, pieces.values())), "verified": True}
                output.write(json.dumps(row) + "\n")
                output.flush()
    finally:
        for start in range(0, len(created), 1000):
            client.delete_objects(Bucket=bucket, Delete={"Objects": [{"Key": k} for k in created[start:start + 1000]], "Quiet": True})


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    endpoint = os.environ.get("STORAGE_TEST_S3_ENDPOINT", "http://localhost:19000")
    if urlparse(endpoint).hostname not in ("localhost", "127.0.0.1"):
        raise SystemExit("This fixture benchmark requires a local test store")
    client = boto3.client("s3", endpoint_url=endpoint, region_name="us-east-1",
                          aws_access_key_id=os.environ["STORAGE_TEST_S3_ACCESS_KEY"],
                          aws_secret_access_key=os.environ["STORAGE_TEST_S3_SECRET_KEY"],
                          config=Config(signature_version="s3v4", s3={"addressing_style": "path"}, max_pool_connections=8))
    bucket = "shiki-version-layout-bench"
    existing = [b["Name"] for b in client.list_buckets()["Buckets"]]
    if bucket not in existing:
        client.create_bucket(Bucket=bucket)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w") as output, ThreadPoolExecutor(max_workers=8) as pool:
        for size in (65536, 1048576, 4194304):
            for seed in (73, 941):
                for edit in ("local", "head", "scatter", "rewrite", "churn32"):
                    run_case(client, bucket, pool, size, seed, edit, output)


if __name__ == "__main__":
    main()
