#!/usr/bin/env python3
"""Check completeness and produce tables directly from measured observations."""

import argparse
import collections
import csv
import gzip
import json
import statistics
from pathlib import Path

from bench import METHODS, percentile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--allow-partial", action="store_true")
    args = parser.parse_args()
    directory = args.directory
    meta = json.loads((directory / "environment.json").read_text())
    runs = [json.loads(line) for line in (directory / "runs.jsonl").read_text().splitlines()]
    by_key = {(r["case"], r["method"], r["trial"]): r for r in runs}
    if len(by_key) != len(runs):
        raise ValueError("Duplicate completed runs")
    cases = {r["case"]: r for r in runs if r["method"] == "full-cas"}
    methods = [m for m in METHODS if any(r["method"] == m for r in runs)]
    if not args.allow_partial:
        for name in cases:
            for method in meta["methods"]:
                for trial in range(meta["trials"]):
                    if (name, method, trial) not in by_key:
                        raise ValueError(f"Missing {name}/{method}/{trial}")
        if len(cases) != meta["histories"]:
            raise ValueError("History count mismatch")
    samples = {}
    with gzip.open(directory / "samples.jsonl.gz", "rt") as stream:
        for line in stream:
            s = json.loads(line)
            # Interrupted, uncommitted method runs may have written samples; the
            # last successful retry is canonical. Completed runs never duplicate.
            if (s[0], s[1], s[2]) in by_key:
                samples[tuple(s[:4])] = s
    if not args.allow_partial:
        expected = sum(r["versions"] for r in runs)
        if len(samples) != expected:
            raise ValueError(f"Expected {expected} samples, got {len(samples)}")
    per_case = {}
    for r in runs:
        if r["trial"] == 0:
            per_case[(r["case"], r["method"])] = r
    scopes = {"all": set(cases), "active": {n for n, r in cases.items() if r["unique_versions"] > 1},
              "library": {n for n, r in cases.items() if r["source"] == "library-load-edit-save"},
              "fixtures": {n for n, r in cases.items() if "/" in r["source"]},
              "long": {n for n, r in cases.items() if r["source"] == "generated-long-history"},
              "large": {n for n, r in cases.items() if r["source"] == "generated-4MiB"}}
    for family in sorted({r["family"] for r in cases.values()}):
        scopes[f"family:{family}"] = {n for n, r in cases.items() if r["family"] == family}
    for edit in sorted({r["edit"] for r in cases.values()}):
        scopes[f"edit:{edit}"] = {n for n, r in cases.items() if r["edit"] == edit}
    result = []
    for scope, names in scopes.items():
        if not names:
            continue
        for method in methods:
            names_with_data = {n for n in names if (n, method) in per_case}
            if not names_with_data:
                continue
            rows = [per_case[(n, method)] for n in names_with_data]
            base = sum(per_case[(n, "full-cas")]["modeled_bytes"] for n in names_with_data)
            ratios = [100 * r["modeled_bytes"] / per_case[(r["case"], "full-cas")]["modeled_bytes"] for r in rows]
            ss = [s for s in samples.values() if s[0] in names_with_data and s[1] == method]
            result.append({"scope": scope, "method": method, "histories": len(rows),
                           "baseline_bytes": base, "payload_bytes": sum(r["payload_bytes"] for r in rows),
                           "modeled_bytes": sum(r["modeled_bytes"] for r in rows),
                           "remaining_pct": 100 * sum(r["modeled_bytes"] for r in rows) / base,
                           "median_remaining_pct": statistics.median(ratios),
                           "with_latest_pct": 100 * sum(r["modeled_bytes"] + r["latest_materialized_extra_bytes"] for r in rows) / base,
                           "objects": sum(r["objects"] for r in rows),
                           "encode_p50_ms": statistics.median(s[4] for s in ss),
                           "encode_p95_ms": percentile([s[4] for s in ss], .95),
                           "restore_p50_ms": statistics.median(s[5] for s in ss),
                           "restore_p95_ms": percentile([s[5] for s in ss], .95),
                           "restore_max_ms": max(s[5] for s in ss),
                           "reads_mean": statistics.mean(s[6] for s in ss),
                           "reads_max": max(s[6] for s in ss),
                           "depth_max": max(r["depth_max"] for r in rows),
                           "larger_than_baseline_cases": sum(r > 100 for r in ratios),
                           "worst_remaining_pct": max(ratios)})
    with (directory / "summary.csv").open("w") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(result[0]))
        writer.writeheader()
        writer.writerows(result)
    with (directory / "sensitivity.csv").open("w") as stream:
        writer = csv.writer(stream)
        writer.writerow(["object_overhead_bytes", "method", "remaining_pct"])
        for overhead in (0, 64, 256, 1024):
            for method in methods:
                names = [n for n in cases if (n, method) in per_case]
                base = sum(per_case[(n, "full-cas")]["payload_bytes"] + overhead * per_case[(n, "full-cas")]["objects"] + 48 * cases[n]["versions"] for n in names)
                size = sum(per_case[(n, method)]["payload_bytes"] + overhead * per_case[(n, method)]["objects"] + 48 * cases[n]["versions"] for n in names)
                writer.writerow([overhead, method, 100 * size / base])
    with (directory / "trials.csv").open("w") as stream:
        writer = csv.writer(stream)
        writer.writerow(["method", "trial", "save_total_ms", "restore_total_ms", "restore_p95_ms"])
        for method in methods:
            for trial in range(meta["trials"]):
                rs = [r for r in runs if r["method"] == method and r["trial"] == trial]
                ss = [s for s in samples.values() if s[1] == method and s[2] == trial]
                if rs and ss:
                    writer.writerow([method, trial, sum(r["encode_ms"] for r in rs),
                                     sum(r["decode_ms"] for r in rs), percentile([s[5] for s in ss], .95)])
    lines = ["<!-- Generated by summarize.py; values are measured except explicit storage overhead model. -->", "",
             "| 方式 | 現行比の容量 % | 最新版の全文併置込み % | 保存 p95 ms | 復元 p95 ms | 平均 / 最大読出数 |", "|---|---:|---:|---:|---:|---:|"]
    for r in result:
        if r["scope"] == "all":
            lines.append(f'| {r["method"]} | {r["remaining_pct"]:.2f} | {r["with_latest_pct"]:.2f} | {r["encode_p95_ms"]:.3f} | {r["restore_p95_ms"]:.3f} | {r["reads_mean"]:.2f} / {r["reads_max"]} |')
    lines += ["", "形式別の容量（現行比 %。各形式内のバイト加重、実利用の構成比は仮定しない）。", "",
              "| 形式 | " + " | ".join(methods) + " |", "|---|" + "---:|" * len(methods)]
    for scope in scopes:
        if scope.startswith("family:"):
            rs = {r["method"]: r for r in result if r["scope"] == scope}
            lines.append("| " + scope[7:] + " | " + " | ".join(f'{rs[m]["remaining_pct"]:.2f}' if m in rs else "—" for m in methods) + " |")
    lines += ["", "特定ケース群の容量（現行比 %）。", "", "| ケース群 | " + " | ".join(methods) + " |", "|---|" + "---:|" * len(methods)]
    for scope in ("library", "fixtures", "long", "large", "edit:rewrite", "edit:repack", "edit:undo"):
        rs = {r["method"]: r for r in result if r["scope"] == scope}
        if rs:
            lines.append("| " + scope + " | " + " | ".join(f'{rs[m]["remaining_pct"]:.2f}' if m in rs else "—" for m in methods) + " |")
    (directory / "tables.md").write_text("\n".join(lines) + "\n")
    print(json.dumps({"histories": len(cases), "runs": len(runs), "verified_samples": len(samples), "methods": methods}))
    print("\n".join(lines[:4 + len(methods)]))


if __name__ == "__main__":
    main()
