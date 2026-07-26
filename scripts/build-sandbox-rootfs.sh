#!/usr/bin/env bash
# ネイティブティア（gVisor/Firecracker）用ゲスト rootfs を **ビルド時のみ** 生成する。
#
# ソースは `python:3.12-slim`（digest pin）＋ **numpy/pandas 同梱**（#346・design §4.6 前提条件。
# code_interpreter の宣伝どおり native ティアでも `import numpy` が動くこと）。再現性は
# digest pin × wheel ハッシュ全固定（`--require-hashes`・deploy/sandbox-assets/rootfs-requirements.txt）
# の二層で閉じる。1 ツリーから 2 つの成果物を出す:
#   - `rootfs/`   ディレクトリ（gVisor の OCI `root.path`・runsc がそのまま使う）
#   - `rootfs.ext4` イメージ（Firecracker のルートドライブ・PR3・`mkfs.ext4 -d` で非特権生成）
#
# 実行時ダウンロードは行わない（PIT-33）。pip install はビルド時（docker build 内）のみ。
# サイズは stdout と `deploy/sandbox-assets/rootfs-size.txt` に記録する（イメージ増分の追跡・#346）。
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
REQUIREMENTS="$ROOT/deploy/sandbox-assets/rootfs-requirements.txt"
# ext4 サイズ（FC ワークスペースは別ドライブ。numpy/pandas 同梱で ~700MiB のため 1024MiB）。
EXT4_SIZE_MIB="${EXT4_SIZE_MIB:-1024}"

echo "→ rootfs を $PIN_IMAGE から生成（numpy/pandas 同梱・--require-hashes・実行時 DL 無し）"
mkdir -p "$OUT"
rm -rf "$ROOTFS_DIR"
mkdir -p "$ROOTFS_DIR"

# pip install はビルド時の docker build 内のみ（ハッシュ全固定・ビルドキャッシュが効く）。
IMAGE_TAG="shiki-sandbox-rootfs:local"
docker build -q -t "$IMAGE_TAG" -f - "$ROOT/deploy/sandbox-assets" <<EOF
FROM $PIN_IMAGE
COPY rootfs-requirements.txt /tmp/rootfs-requirements.txt
RUN pip install --no-cache-dir --require-hashes --only-binary=:all: \
        -r /tmp/rootfs-requirements.txt \
    && rm /tmp/rootfs-requirements.txt
EOF

CID="$(docker create "$IMAGE_TAG")"
trap 'docker rm -f "$CID" >/dev/null 2>&1 || true' EXIT
docker export "$CID" | tar -C "$ROOTFS_DIR" -xf -

# ネイティブティアの既定 DNS/hosts を用意（egress 時に orchestrator が resolv.conf を bind 上書きする）。
printf 'nameserver 169.254.0.1\n' > "$ROOTFS_DIR/etc/resolv.conf"
printf '127.0.0.1 localhost\n' > "$ROOTFS_DIR/etc/hosts"

# サイズを記録する（#346「イメージサイズの増分を計測して記録」）。
SIZE_HUMAN="$(du -sh "$ROOTFS_DIR" | cut -f1)"
SIZE_KB="$(du -sk "$ROOTFS_DIR" | cut -f1)"
{
  echo "# scripts/build-sandbox-rootfs.sh が生成（rootfs サイズの追跡・#346）"
  echo "pin_image=$PIN_IMAGE"
  echo "packages=$(grep -E '^[a-zA-Z]' "$REQUIREMENTS" | cut -d' ' -f1 | tr '\n' ' ')"
  echo "rootfs_size=$SIZE_HUMAN (${SIZE_KB} KiB)"
} > "$OUT/rootfs-size.txt"
echo "✅ gVisor rootfs: $ROOTFS_DIR ($SIZE_HUMAN・rootfs-size.txt に記録)"

# Firecracker 用 ext4（PR3）: guest-agent を /sbin/sandbox-init に置いてから mkfs.ext4 -d で非特権生成。
if [ -n "${SANDBOX_AGENT_BIN:-}" ]; then
  echo "→ Firecracker rootfs.ext4 を生成（agent=$SANDBOX_AGENT_BIN）"
  install -D -m 0755 "$SANDBOX_AGENT_BIN" "$ROOTFS_DIR/sbin/sandbox-init"
  rm -f "$EXT4_IMG"
  # mkfs.ext4 -d はディレクトリ内容をそのままイメージに焼く（root/loop 不要）。
  mkfs.ext4 -q -F -L sbxroot -d "$ROOTFS_DIR" "$EXT4_IMG" "${EXT4_SIZE_MIB}M"
  echo "✅ Firecracker rootfs.ext4: $EXT4_IMG"
fi
