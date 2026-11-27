#!/usr/bin/env python3
"""Export static figures from summary.csv; no separately entered chart values."""

import argparse
import csv
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    rows = list(csv.DictReader((args.directory / "summary.csv").open()))
    methods = [r["method"] for r in rows if r["scope"] == "all"]
    families = sorted({r["scope"][7:] for r in rows if r["scope"].startswith("family:")})
    values = np.array([[float(next(r["remaining_pct"] for r in rows
                                  if r["scope"] == f"family:{f}" and r["method"] == m))
                        for m in methods] for f in families])
    fig, ax = plt.subplots(figsize=(15, 7))
    im = ax.imshow(values, cmap="YlOrRd", vmin=0, vmax=max(100, values.max()), aspect="auto")
    ax.set_xticks(range(len(methods)), methods, rotation=35, ha="right")
    ax.set_yticks(range(len(families)), families)
    for i in range(len(families)):
        for j in range(len(methods)):
            ax.text(j, i, f"{values[i, j]:.1f}", ha="center", va="center", fontsize=9,
                    color="white" if values[i, j] > 65 else "black")
    ax.set_title("Stored bytes as % of current whole-file CAS (lower is smaller)")
    fig.colorbar(im, ax=ax, label="% of baseline; 64 B/object + 48 B/version model included")
    fig.tight_layout()
    fig.savefig(args.directory / "capacity.svg")
    plt.close(fig)

    all_rows = [r for r in rows if r["scope"] == "all"]
    x = np.arange(len(all_rows))
    fig, ax = plt.subplots(figsize=(13, 5))
    ax.bar(x - .2, [float(r["encode_p95_ms"]) for r in all_rows], width=.4, label="Save p95")
    ax.bar(x + .2, [float(r["restore_p95_ms"]) for r in all_rows], width=.4, label="Restore p95")
    ax.set_yscale("log")
    ax.set_ylabel("Milliseconds (log scale)")
    ax.set_xticks(x, methods, rotation=35, ha="right")
    ax.set_title("Local codec timings; xdelta includes CLI startup and temporary-file I/O")
    ax.legend()
    fig.tight_layout()
    fig.savefig(args.directory / "latency.svg")
    plt.close(fig)


if __name__ == "__main__":
    main()
