#!/usr/bin/env python3
"""公開コーパスの document vectors を ingestion-worker と同じ推論で生成する。"""

import argparse
import hashlib
import json
from pathlib import Path


def prepare(source, output):
    from ingestion_worker.model_registry import get_registry
    from ingestion_worker.settings import get_settings

    if (source / "manifest.json").stat().st_size > 65536:
        raise ValueError("manifest size")
    if (source / "chunks.jsonl").stat().st_size > 64 * 1024 * 1024:
        raise ValueError("chunks size")
    manifest_bytes = (source / "manifest.json").read_bytes()
    manifest = json.loads(manifest_bytes)
    data = (source / "chunks.jsonl").read_bytes()
    if len(data) > 64 * 1024 * 1024 or len(data) != manifest["chunks_bytes"]:
        raise ValueError("corpus size")
    if hashlib.sha256(data).hexdigest() != manifest["chunks_sha256"]:
        raise ValueError("corpus hash")
    if (
        manifest["schema_version"] != 1
        or manifest["corpus_id"] != "shiki-help"
        or manifest["visibility"] != "public"
        or manifest["asset_count"] != 0
    ):
        raise ValueError("public corpus contract")
    chunks = [json.loads(line) for line in data.splitlines() if line]
    if len(chunks) != manifest["chunk_count"] or len(chunks) > 20000:
        raise ValueError("chunk count")
    for chunk in chunks:
        if (
            chunk["status"] not in {"reviewed", "final"}
            or chunk["corpus_version"] != manifest["corpus_version"]
            or hashlib.sha256(chunk["text"].encode()).hexdigest()
            != chunk["content_sha256"]
        ):
            raise ValueError("chunk contract")
    output.mkdir(parents=True, exist_ok=True)
    rows = []
    dimension = 0
    for start in range(0, len(chunks), 8):
        batch = chunks[start : start + 8]
        vectors = (
            get_registry().embedding().encode(["検索文書: " + c["text"] for c in batch])
        )
        for chunk, vector in zip(batch, vectors, strict=True):
            dimension = len(vector)
            rows.append(
                json.dumps(
                    {"chunk_id": chunk["chunk_id"], "vector": vector},
                    ensure_ascii=False,
                )
                + "\n"
            )
    vector_bytes = "".join(rows).encode()
    vm = {
        "schema_version": 1,
        "corpus_version": manifest["corpus_version"],
        "chunks_sha256": manifest["chunks_sha256"],
        "model_version": get_settings().embed_model,
        "dimension": dimension,
        "normalized": True,
        "query_prefix": "検索クエリ: ",
        "document_prefix": "検索文書: ",
        "vectors_sha256": hashlib.sha256(vector_bytes).hexdigest(),
    }
    (output / "manifest.json").write_bytes(manifest_bytes)
    (output / "chunks.jsonl").write_bytes(data)
    (output / "vectors.jsonl").write_bytes(vector_bytes)
    (output / "vectors.manifest.json").write_text(
        json.dumps(vm, ensure_ascii=False) + "\n"
    )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    prepare(args.source, args.output)
