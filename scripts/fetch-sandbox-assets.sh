#!/usr/bin/env bash
# サンドボックス実行アセット（Pyodide 一式）を pin+検証付きで取得する（PIT-33）。
#
# vendor/secure-exec/asset-manifest.sha256 の各行（<sha256> <相対パス> <URL>）について:
#   1. 相対パスが既に存在し SHA-256 が一致 → skip
#   2. それ以外 → URL から取得（SANDBOX_ASSET_BASE が設定されていればそのミラーを優先）→
#      SHA-256 検証 → 配置。検証失敗は即エラー（改竄/バージョン不一致を止める）。
#
# 実行時ダウンロードは行わない。この取得は CI とサンドボックスイメージのビルド前段でのみ走る。
# エアギャップ: SANDBOX_ASSET_BASE=file:///path/to/mirror などで同一 SHA のローカルミラーを差す。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
MANIFEST="vendor/secure-exec/asset-manifest.sha256"
BASE="${SANDBOX_ASSET_BASE:-}"

if [ ! -f "$MANIFEST" ]; then
  echo "❌ manifest が見つかりません: $MANIFEST" >&2
  exit 1
fi

verify() { # <path> <expected_sha>
  [ -f "$1" ] || return 1
  local got
  got="$(sha256sum "$1" | awk '{print $1}')"
  [ "$got" = "$2" ]
}

fetched=0 skipped=0
while read -r sha rel url; do
  case "$sha" in ''|'#'*) continue ;; esac
  dest="vendor/secure-exec/$rel"
  if verify "$dest" "$sha"; then
    skipped=$((skipped + 1))
    continue
  fi
  # ミラー優先: BASE が指定されていれば URL のファイル名部分を BASE に付け替える
  src="$url"
  if [ -n "$BASE" ]; then
    src="${BASE%/}/$(basename "$rel")"
  fi
  echo "→ 取得: $rel"
  mkdir -p "$(dirname "$dest")"
  tmp="$dest.tmp.$$"
  if ! curl -fsSL --retry 3 -o "$tmp" "$src"; then
    rm -f "$tmp"
    echo "❌ ダウンロード失敗: $src" >&2
    exit 1
  fi
  if ! verify "$tmp" "$sha"; then
    got="$(sha256sum "$tmp" | awk '{print $1}')"
    rm -f "$tmp"
    echo "❌ SHA-256 不一致: $rel" >&2
    echo "   期待: $sha" >&2
    echo "   実際: $got" >&2
    echo "   → 改竄またはバージョン不一致。manifest とソースを確認すること。" >&2
    exit 1
  fi
  mv "$tmp" "$dest"
  fetched=$((fetched + 1))
done < "$MANIFEST"

echo "✅ サンドボックスアセット: 取得 $fetched / 検証済み skip $skipped"
