#!/usr/bin/env python3
"""Version storage codec benchmark. See README for measurement boundaries."""

import argparse
import collections
import gzip
import hashlib
import importlib.metadata
import json
import math
import os
import platform
import random
import shutil
import statistics
import subprocess
import tempfile
import time
from pathlib import Path

import zstandard as zstd
from fastcdc import fastcdc

from corpus import cases, digest

METHODS = ("full-cas", "full-zstd", "fixed64-zstd", "cdc16-zstd", "cdc64-zstd",
           "zdict8", "zdict32", "xdelta8", "xdelta32", "zanchor8", "zanchor32")
COMPRESSOR = zstd.ZstdCompressor(level=3, write_checksum=True)
DECOMPRESSOR = zstd.ZstdDecompressor()
VERSION_BYTES = 48  # Logical version reference; same across methods. Not a PostgreSQL row estimate.
OBJECT_BYTES = 64  # Explicit model: key/index overhead per physical object, not provider metadata.


def pack(data):
    compressed = COMPRESSOR.compress(data)
    return b"Z" + compressed if len(compressed) < len(data) else b"R" + data


def unpack(data):
    return DECOMPRESSOR.decompress(data[1:]) if data[:1] == b"Z" else data[1:]


def percentile(values, q):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, math.ceil(len(ordered) * q) - 1)]


class Store:
    def __init__(self, method, xdelta, workdir, dictionary_window_log=0):
        self.method = method
        self.objects = {}
        self.references = []
        self.xdelta = xdelta
        self.workdir = Path(workdir)
        self.depths = {}
        self.previous = None
        self.previous_key = None
        self.anchor = None
        self.anchor_key = None
        self.reads = 0
        self.bytes_read = 0
        self.dictionary_window_log = dictionary_window_log

    def xd(self, mode, base, value):
        # Include CLI startup and temporary file I/O in xdelta timings. No shell execution.
        source = self.workdir / "base"
        source.write_bytes(base)
        result = subprocess.run([self.xdelta, mode, "-c", "-D", "-R", "-A", "-S", "none",
                                 "-B", "1048576", "-W", "1048576", "-3", "-s", str(source)],
                                input=value, capture_output=True, check=True)
        return result.stdout

    def add(self, data, index):
        key = hashlib.sha256(data).digest()
        self.references.append(key)
        if key in self.objects:
            self.previous, self.previous_key = data, key
            return
        if self.method == "full-cas":
            representation = b"F" + data
        elif self.method == "full-zstd":
            representation = b"F" + pack(data)
        elif self.method.startswith(("cdc", "fixed")):
            if self.method.startswith("fixed"):
                chunks = [data[i:i + 65_536] for i in range(0, len(data), 65_536)]
            else:
                avg = 16_384 if self.method.startswith("cdc16") else 65_536
                chunks = [data[c.offset:c.offset + c.length] for c in
                          fastcdc(data, min_size=avg // 4, avg_size=avg, max_size=avg * 4)]
            refs = []
            for chunk in chunks:
                ck = hashlib.sha256(b"chunk\0" + chunk).digest()
                if ck not in self.objects:
                    self.objects[ck] = b"C" + pack(chunk)
                refs.append(ck)
            representation = b"M" + b"".join(refs)
        else:
            interval = 8 if self.method.endswith("8") else 32
            representation = b"F" + pack(data)
            depth = 0
            anchored = self.method.startswith("zanchor")
            base, base_key = ((self.anchor, self.anchor_key) if anchored else (self.previous, self.previous_key))
            if base is not None and index % interval and self.depths[base_key] < interval - 1:
                if self.method.startswith(("zdict", "zanchor")):
                    dictionary = zstd.ZstdCompressionDict(base, dict_type=zstd.DICT_TYPE_RAWCONTENT)
                    if self.dictionary_window_log:
                        params = zstd.ZstdCompressionParameters.from_level(
                            3, window_log=self.dictionary_window_log, write_checksum=1)
                        compressor = zstd.ZstdCompressor(compression_params=params, dict_data=dictionary)
                    else:
                        compressor = zstd.ZstdCompressor(level=3, dict_data=dictionary, write_checksum=True)
                    patch = compressor.compress(data)
                else:
                    patch = pack(self.xd("-e", base, data))
                candidate = b"D" + base_key + patch
                if len(candidate) < len(representation):
                    representation = candidate
                    depth = self.depths[base_key] + 1
            self.depths[key] = depth
            if anchored and representation[:1] == b"F":
                self.anchor, self.anchor_key = data, key
        self.objects[key] = representation
        self.depths.setdefault(key, 0)
        self.previous, self.previous_key = data, key

    def get(self, key):
        representation = self.objects[key]
        self.reads += 1
        self.bytes_read += len(representation)
        tag, payload = representation[:1], representation[1:]
        if tag == b"M":
            return b"".join(self.get(payload[i:i + 32]) for i in range(0, len(payload), 32))
        if tag == b"C":
            return unpack(payload)
        if tag == b"F":
            return payload if self.method == "full-cas" else unpack(payload)
        base = self.get(payload[:32])
        if self.method.startswith(("zdict", "zanchor")):
            dictionary = zstd.ZstdCompressionDict(base, dict_type=zstd.DICT_TYPE_RAWCONTENT)
            return zstd.ZstdDecompressor(dict_data=dictionary).decompress(payload[32:])
        return self.xd("-d", base, unpack(payload[32:]))


class FileObjects:
    """One local file per object, warm OS page cache, no fsync; not an S3 emulator."""
    def __init__(self, directory):
        self.directory = Path(directory)
        if self.directory.exists():
            shutil.rmtree(self.directory)
        self.directory.mkdir()
        self.keys = set()

    def __contains__(self, key):
        return key in self.keys

    def __len__(self):
        return len(self.keys)

    def __setitem__(self, key, value):
        (self.directory / key.hex()).write_bytes(value)
        self.keys.add(key)

    def __getitem__(self, key):
        return (self.directory / key.hex()).read_bytes()

    def values(self):
        return (self[key] for key in self.keys)


def run(case, method, trial, xdelta, workdir, samples, backend="memory", dictionary_window_log=0):
    store = Store(method, xdelta, workdir, dictionary_window_log)
    if backend == "files":
        store.objects = FileObjects(Path(workdir) / "objects")
    encode = []
    for i, data in enumerate(case.versions):
        start = time.perf_counter_ns()
        store.add(data, i)
        encode.append((time.perf_counter_ns() - start) / 1e6)
    # Independent, randomly ordered reads: no reconstructed-base cache across requests.
    order = list(range(len(case.versions)))
    random.Random(97 + trial).shuffle(order)
    decode, read_counts, read_bytes = [], [], []
    for i in order:
        old_reads, old_bytes = store.reads, store.bytes_read
        start = time.perf_counter_ns()
        actual = store.get(store.references[i])
        elapsed = (time.perf_counter_ns() - start) / 1e6
        if actual != case.versions[i]:
            raise ValueError((case.name, method, i, "round-trip mismatch"))
        decode.append(elapsed)
        read_counts.append(store.reads - old_reads)
        read_bytes.append(store.bytes_read - old_bytes)
        samples.write(json.dumps([case.name, method, trial, i, encode[i], elapsed,
                                  read_counts[-1], read_bytes[-1]], separators=(",", ":")) + "\n")
    payload_bytes = sum(map(len, store.objects.values()))
    return {"case": case.name, "family": case.family, "source": case.source, "edit": case.edit,
            "seed": case.seed, "method": method, "trial": trial, "versions": len(case.versions),
            "logical_bytes": sum(map(len, case.versions)), "latest_bytes": len(case.versions[-1]),
            "unique_versions": len(set(store.references)), "objects": len(store.objects),
            "payload_bytes": payload_bytes,
            "modeled_bytes": payload_bytes + OBJECT_BYTES * len(store.objects) + VERSION_BYTES * len(case.versions),
            "encode_ms": sum(encode), "encode_p50_ms": statistics.median(encode),
            "encode_p95_ms": percentile(encode, .95), "encode_max_ms": max(encode),
            "decode_ms": sum(decode), "decode_p50_ms": statistics.median(decode),
            "decode_p95_ms": percentile(decode, .95), "decode_max_ms": max(decode),
            "reads_mean": statistics.mean(read_counts), "reads_max": max(read_counts),
            "read_bytes_mean": statistics.mean(read_bytes), "depth_max": max(store.depths.values()),
            "latest_depth": store.depths[store.references[-1]],
            "latest_materialized_extra_bytes": (0 if method == "full-cas" else len(case.versions[-1]) + OBJECT_BYTES)}


RESUME_FIELDS = ("backend", "dictionary_window_log", "object_model_bytes", "version_model_bytes",
                 "xdelta_binary_sha256", "packages", "python", "trials", "quick", "methods", "instrument_sha256")


def validate_resume(previous, current):
    for field in RESUME_FIELDS:
        if field not in previous or previous[field] != current[field]:
            raise ValueError(f"Measurement conditions changed during resume: {field}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--trials", type=int, default=3)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--methods", nargs="+", choices=METHODS, default=list(METHODS))
    parser.add_argument("--xdelta", default="xdelta3")
    parser.add_argument("--filter", default="")
    parser.add_argument("--select-cases", type=Path)
    parser.add_argument("--backend", choices=("memory", "files"), default="memory")
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--dictionary-window-log", type=int, default=0)
    args = parser.parse_args()
    args.methods = list(dict.fromkeys(["full-cas", *args.methods]))
    args.output.mkdir(parents=True, exist_ok=True)
    selected = set(json.loads(args.select_cases.read_text())) if args.select_cases else None
    completed = {}
    known_corpus = {}
    if args.resume:
        for line in (args.output / "runs.jsonl").read_text().splitlines():
            row = json.loads(line)
            completed[(row["case"], row["method"], row["trial"])] = row
        for line in (args.output / "corpus.jsonl").read_text().splitlines():
            entry = json.loads(line)
            known_corpus[entry["case"]] = entry
    versions = {p: importlib.metadata.version(p) for p in
                ("fastcdc", "zstandard", "python-docx", "openpyxl", "python-pptx", "Pillow", "reportlab", "numpy")}
    meta = {"started_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "python": platform.python_version(), "platform": platform.platform(), "cpu_count": os.cpu_count(),
            "cpu_model": next((s.split(":", 1)[1].strip() for s in Path("/proc/cpuinfo").read_text().splitlines()
                               if s.startswith("model name")), "unknown"),
            "cpu_quota": Path("/sys/fs/cgroup/cpu.max").read_text().strip(),
            "memory_limit": Path("/sys/fs/cgroup/memory.max").read_text().strip(),
            "packages": versions, "trials": args.trials, "quick": args.quick, "methods": args.methods,
            "backend": args.backend,
            "instrument_sha256": {name: digest(Path(__file__).with_name(name).read_bytes()) for name in ("bench.py", "corpus.py")},
            "dictionary_window_log": args.dictionary_window_log,
            "object_model_bytes": OBJECT_BYTES, "version_model_bytes": VERSION_BYTES,
            "baseline_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
            "xdelta_version": subprocess.run([args.xdelta, "-V"], capture_output=True, text=True).stderr.splitlines()[:1],
            "xdelta_binary_sha256": digest(Path(shutil.which(args.xdelta) or args.xdelta).read_bytes()),
            "timing_boundary": ("single-process serial codec + " +
                                ("local object files, warm OS page cache" if args.backend == "files" else "in-memory object index") +
                                "; xdelta additionally includes subprocess and temp-file I/O; no DB/network/fsync")}
    if args.resume:
        previous = json.loads((args.output / "environment.json").read_text())
        validate_resume(previous, meta)
        meta["resumed_utc"] = meta["started_utc"]
        meta["started_utc"] = previous["started_utc"]
    (args.output / "environment.json").write_text(json.dumps(meta, indent=2) + "\n")
    started = time.perf_counter()
    counts = collections.Counter()
    COMPRESSOR.compress(b"warmup" * 1000)
    mode = "a" if args.resume else "w"
    with tempfile.TemporaryDirectory(prefix="version-storage-") as workdir, \
            (args.output / "runs.jsonl").open(mode) as runs, \
            (args.output / "corpus.jsonl").open(mode) as corpus, \
            gzip.open(args.output / "samples.jsonl.gz", mode + "t") as samples:
        def wanted(name):
            return (not args.filter or args.filter in name) and (selected is None or name in selected)
        for case in cases(args.quick, wanted):
            if args.filter and args.filter not in case.name:
                continue
            if selected is not None and case.name not in selected:
                continue
            hashes = [digest(v) for v in case.versions]
            entry = {"case": case.name, "family": case.family, "source": case.source,
                     "edit": case.edit, "seed": case.seed,
                     "versions": [{"sha256": h, "bytes": len(v)} for h, v in zip(hashes, case.versions)]}
            if case.name in known_corpus:
                if entry != known_corpus[case.name]:
                    raise ValueError(f"Corpus changed during resume: {case.name}")
            else:
                corpus.write(json.dumps(entry) + "\n")
            for trial in range(args.trials):
                methods = list(args.methods)
                random.Random(23 + trial + counts["histories"]).shuffle(methods)
                for method in methods:
                    if (case.name, method, trial) not in completed:
                        row = run(case, method, trial, args.xdelta, workdir, samples, args.backend,
                                  args.dictionary_window_log)
                        runs.write(json.dumps(row, separators=(",", ":")) + "\n")
                    counts["verified_reads"] += len(case.versions)
            counts["histories"] += 1
            counts["versions"] += len(case.versions)
            counts["logical_bytes"] += sum(map(len, case.versions))
            runs.flush()
            if counts["histories"] % 10 == 0:
                print(json.dumps({**counts, "elapsed_s": round(time.perf_counter() - started, 1), "last": case.name}), flush=True)
    meta.update(counts)
    meta["elapsed_s"] = time.perf_counter() - started
    (args.output / "environment.json").write_text(json.dumps(meta, indent=2) + "\n")
    print(json.dumps(meta), flush=True)


if __name__ == "__main__":
    main()
