#!/usr/bin/env bash
# ネイティブティア（gVisor/Firecracker）用ゲスト rootfs を **ビルド時のみ** 生成する。
#
# ソースは `python:3.12-slim`（digest pin）。1 ツリーから 2 つの成果物を出す:
#   - `rootfs/`   ディレクトリ（gVisor の OCI `root.path`・runsc がそのまま使う）
#   - `rootfs.ext4` イメージ（Firecracker のルートドライブ・PR3・`mkfs.ext4 -d` で非特権生成）
#
# 実行時ダウンロードは行わない（PIT-33）。docker export でイメージ層を取り出すだけ。
#
# 使い方:
#   scripts/build-sandbox-rootfs.sh                       # rootfs/ のみ（gVisor 用）
#   SANDBOX_AGENT_BIN=path/to/agent scripts/build-sandbox-rootfs.sh   # ext4 も生成（FC 用・PR3）
#   ROOTFS_OUT=/opt/shiki/sandbox-assets scripts/build-sandbox-rootfs.sh
set -euo pipefail

# python:3.12-slim の pin（再現性・PIT-33）。更新時はこの digest を差し替える。
PIN_IMAGE="python:3.12-slim@sha256:423ed6ab25b1921a477529254bfeeabf5855151dc2c3141699a1bfc852199fbf"

ROOT="$(git rev-parse --show-toplevel 2>/dev/null || { cd "$(dirname "$0")/.." && pwd; })"
OUT="${ROOTFS_OUT:-$ROOT/deploy/sandbox-assets}"
ROOTFS_DIR="$OUT/rootfs"
EXT4_IMG="$OUT/rootfs.ext4"
# ext4 サイズ（FC ワークスペースは別ドライブ。ルートは読取専用で ~400MiB あれば足りる）。
EXT4_SIZE_MIB="${EXT4_SIZE_MIB:-512}"

echo "→ rootfs を $PIN_IMAGE から生成（docker export・実行時 DL 無し）"
mkdir -p "$OUT"
rm -rf "$ROOTFS_DIR"
mkdir -p "$ROOTFS_DIR"

CID="$(docker create "$PIN_IMAGE")"
trap 'docker rm -f "$CID" >/dev/null 2>&1 || true' EXIT
docker export "$CID" | tar -C "$ROOTFS_DIR" -xf -

# ネイティブティアの既定 DNS/hosts を用意（egress 時に orchestrator が resolv.conf を bind 上書きする）。
printf 'nameserver 169.254.0.1\n' > "$ROOTFS_DIR/etc/resolv.conf"
printf '127.0.0.1 localhost\n' > "$ROOTFS_DIR/etc/hosts"

echo "✅ gVisor rootfs: $ROOTFS_DIR ($(du -sh "$ROOTFS_DIR" | cut -f1))"

# Firecracker 用 ext4（PR3）: guest-agent を /sbin/sandbox-init に置いてから mkfs.ext4 -d で非特権生成。
if [ -n "${SANDBOX_AGENT_BIN:-}" ]; then
  echo "→ Firecracker rootfs.ext4 を生成（agent=$SANDBOX_AGENT_BIN）"
  install -D -m 0755 "$SANDBOX_AGENT_BIN" "$ROOTFS_DIR/sbin/sandbox-init"
  rm -f "$EXT4_IMG"
  # mkfs.ext4 -d はディレクトリ内容をそのままイメージに焼く（root/loop 不要）。
  mkfs.ext4 -q -F -L sbxroot -d "$ROOTFS_DIR" "$EXT4_IMG" "${EXT4_SIZE_MIB}M"
  echo "✅ Firecracker rootfs.ext4: $EXT4_IMG"
fi
