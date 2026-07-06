#!/usr/bin/env bash
# secure-exec フォークを再 vendor する（上流の特定 commit からサブセットを取り込む）。
# 所有フォークなので上流追従は任意（docs/sandbox/fork-policy.md）。必要な修正を取り込むときに使う。
#
# 使い方: scripts/update-secure-exec.sh <upstream-commit-sha>
#   1. 上流を一時 clone → 指定 commit を checkout
#   2. vendor 対象サブセット（Rust クレート・registry/native・registry/software・LICENSE・toolchain）を同期
#   3. patches/*.patch を順に適用
#   4. UPSTREAM の commit を更新
#   5. 動作確認（secure-exec-client のビルド）
set -euo pipefail

SHA="${1:-}"
if [ -z "$SHA" ]; then
  echo "usage: $0 <upstream-commit-sha>" >&2
  exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
DST="$ROOT/vendor/secure-exec"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "→ 上流 clone（$SHA）"
git clone --quiet https://github.com/rivet-dev/secure-exec "$TMP/src"
git -C "$TMP/src" checkout --quiet "$SHA"

CRATES=(bridge kernel vfs secure-exec-vfs build-support sidecar-protocol \
        sidecar-core execution secure-exec-client sidecar vm-config v8-runtime)

echo "→ Rust クレート同期（sidecar-browser / native-baseline は除外）"
for c in "${CRATES[@]}"; do
  rm -rf "$DST/crates/$c"
  cp -a "$TMP/src/crates/$c" "$DST/crates/"
done

echo "→ registry / LICENSE / toolchain 同期"
rm -rf "$DST/registry/native" "$DST/registry/software"
cp -a "$TMP/src/registry/native" "$DST/registry/"
cp -a "$TMP/src/registry/software" "$DST/registry/"
cp -a "$TMP/src/LICENSE" "$DST/"
cp -a "$TMP/src/rust-toolchain.toml" "$DST/"

echo "→ patches 適用"
if compgen -G "$DST/patches/*.patch" >/dev/null; then
  for p in "$DST/patches"/*.patch; do
    echo "   apply $(basename "$p")"
    git -C "$ROOT" apply --directory="vendor/secure-exec" "$p"
  done
fi

echo "→ UPSTREAM の commit を更新"
sed -i "s/^commit:.*/commit:     $SHA/" "$DST/UPSTREAM"
sed -i "s/^vendored:.*/vendored:   $(date +%Y-%m-%d)/" "$DST/UPSTREAM"

echo "→ ビルド確認（secure-exec-client）"
( cd "$DST" && cargo build -p secure-exec-client )

echo "✅ 再 vendor 完了。asset-manifest.sha256 の Pyodide バージョンも必要に応じ更新すること。"
