#!/usr/bin/env bash
# compose のホストポート二重 publish を検出する。
#
# 背景（#453）: ingestion-worker が `127.0.0.1:8090:8000`、shiki-server が `8090:8090` を
# publish しており、`docker compose up`（引数なし・フルスタック）が
# `Bind for 0.0.0.0:8090 failed: port is already allocated` で起動できなかった。
#
# CI はこの種の衝突を構造的に検出できない。全サービスをまとめて起動せず、ジョブごとに
# 必要なものだけ名指しで起動するため（ci.yml の `docker compose up -d postgres minio ...`）、
# 衝突する組み合わせが同時に立ち上がらない。ローカルでフルスタックを起動した人だけが踏む。
# check-migration-versions.sh（#413）と同じ「CI で拾えない構造的バグ」なので、
# 起動せずファイル定義だけで判定できるここで止める。
#
# 判定は「同一ポート・同一プロトコルで、bind アドレスが重なるか」で行う。
# ⚠️ host_ip をキーに含めて単純比較してはいけない。#453 は 127.0.0.1 と 0.0.0.0 の
#    組み合わせであり、キーが異なるため「重複なし」と誤判定される（実際には衝突する）。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

command -v docker >/dev/null 2>&1 || {
  echo "check-compose-ports: docker が見つかりません（このゲートは docker compose config を使います）。" >&2
  exit 2
}

# .env はリポジトリに含めない（.gitignore: *.env）ため、CI では未配置。
# compose 定義側の既定値だけで解決させ、開発者ごとの .env に判定を左右させない。
CONFIG_JSON=$(docker compose --env-file /dev/null -f deploy/compose/docker-compose.yml config --format json) || {
  echo "check-compose-ports: docker compose config に失敗しました。" >&2
  exit 2
}

printf '%s' "$CONFIG_JSON" | python3 -c '
import json, sys
from collections import defaultdict

cfg = json.load(sys.stdin)

WILDCARD = {"", "0.0.0.0", "::", "*"}

def expand(published):
    """published は "8080" または "8000-8010"（範囲 publish）。"""
    s = str(published)
    if "-" in s:
        lo, hi = s.split("-", 1)
        return range(int(lo), int(hi) + 1)
    return [int(s)]

# (port, protocol) -> [(service, host_ip)]
binds = defaultdict(list)
for name, svc in sorted((cfg.get("services") or {}).items()):
    for p in svc.get("ports") or []:
        pub = p.get("published")
        if not pub:
            continue  # 未 publish（ephemeral）は衝突しない
        proto = p.get("protocol", "tcp")
        host_ip = p.get("host_ip", "") or ""
        for port in expand(pub):
            binds[(port, proto)].append((name, host_ip))

def overlaps(a, b):
    """bind アドレスが重なるか。ワイルドカードは全アドレスと重なる。"""
    return a in WILDCARD or b in WILDCARD or a == b

conflicts = []
for (port, proto), entries in sorted(binds.items()):
    for i in range(len(entries)):
        for j in range(i + 1, len(entries)):
            (s1, ip1), (s2, ip2) = entries[i], entries[j]
            if overlaps(ip1, ip2):
                conflicts.append((port, proto, s1, ip1, s2, ip2))

if conflicts:
    print("ホストポートの二重 publish を検出しました（同時起動できません）:", file=sys.stderr)
    for port, proto, s1, ip1, s2, ip2 in conflicts:
        fmt = lambda ip: ip if ip else "0.0.0.0"
        print(
            f"  {proto}/{port}: {s1}({fmt(ip1)}) と {s2}({fmt(ip2)}) が衝突します",
            file=sys.stderr,
        )
    print("", file=sys.stderr)
    print("どちらかの publish 先ホストポートを変更してください。", file=sys.stderr)
    sys.exit(1)

print(f"check-compose-ports: OK（{len(binds)} 件の publish に衝突なし）")
'
