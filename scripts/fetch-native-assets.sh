#!/usr/bin/env bash
# ネイティブティア（gVisor/Firecracker）のアセットを pin+検証付きで取得する（PIT-33）。
#
# deploy/sandbox-assets/native-manifest.sha256 の各行（<sha256> <相対パス> <URL>）について:
#   1. 相対パスが既に存在し SHA-256 一致 → skip
#   2. それ以外 → URL（SANDBOX_ASSET_BASE 指定時はそのミラー優先）から取得 → SHA-256 検証 → 配置。
#
# 実行時ダウンロードは行わない。CI とサンドボックスイメージのビルド前段でのみ走る。
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || { cd "$(dirname "$0")/.." && pwd; })"
DEST_BASE="${SANDBOX_ASSETS_DIR:-$ROOT/deploy/sandbox-assets}"
MANIFEST="$DEST_BASE/native-manifest.sha256"
BASE="${SANDBOX_ASSET_BASE:-}"

if [ ! -f "$MANIFEST" ]; then
  echo "❌ manifest が見つかりません: $MANIFEST" >&2
  exit 1
fi

verify() { # <path> <expected_sha>
  [ -f "$1" ] || return 1
  [ "$(sha256sum "$1" | awk '{print $1}')" = "$2" ]
}

fetched=0 skipped=0
while read -r sha rel url extra; do
  case "$sha" in ''|'#'*) continue ;; esac
  dest="$DEST_BASE/$rel"
  if verify "$dest" "$sha"; then
    skipped=$((skipped + 1))
    continue
  fi
  src="$url"
  [ -n "$BASE" ] && src="${BASE%/}/$(basename "$url")"
  echo "→ 取得: $rel ← $src"
  mkdir -p "$(dirname "$dest")"
  dl="$(mktemp)"
  curl -fsSL "$src" -o "$dl"
  # SHA はダウンロードしたファイル（tgz の場合はアーカイブ）に対して検証する。
  if ! verify "$dl" "$sha"; then
    echo "❌ SHA-256 不一致: $rel（改竄/バージョン不一致）" >&2
    rm -f "$dl"
    exit 1
  fi
  case "$extra" in
    tgz:*)
      glob="${extra#tgz:}"
      tmpd="$(mktemp -d)"
      tar -xzf "$dl" -C "$tmpd"
      found="$(find "$tmpd" -path "$tmpd/$glob" -type f | head -1)"
      if [ -z "$found" ]; then
        echo "❌ tgz 内に $glob が見つかりません: $rel" >&2
        rm -rf "$dl" "$tmpd"; exit 1
      fi
      install -m 0755 "$found" "$dest"
      rm -rf "$tmpd" "$dl"
      ;;
    *)
      chmod +x "$dl"
      mv "$dl" "$dest"
      ;;
  esac
  fetched=$((fetched + 1))
done < "$MANIFEST"

echo "✅ native アセット: 取得 $fetched・skip $skipped（$DEST_BASE）"
