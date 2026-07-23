# sandbox-orchestrator（非特権プロセス）＋ secure-exec-sidecar バイナリ＋Pyodide アセット
# ＋ゲストコマンドスイート（software package.tar 群）。
# build context はリポジトリルート（deploy/compose/docker-compose.yml から指定）。
#
# 注意: sidecar は V8（rusty_v8 130）をリンクするため build が重い。Pyodide 一式は
# scripts/fetch-sandbox-assets.sh が asset-manifest.sha256 に基づき検証付きで取得する（実行時取得禁止・PIT-33）。
# ゲストコマンドスイート（ls/grep 等）は scripts/build-sandbox-commands.sh が nightly +
# wasm32-wasip1（-Z build-std）でビルドする（commands-builder ステージ・キャッシュ前提の重いビルド）。
# BUILD_COMMANDS=0 でスキップできる（software 無しイメージ・code_interpreter は動く）。

FROM rust:1.96-bookworm AS builder
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl python3 clang cmake && rm -rf /var/lib/apt/lists/*
COPY . .
# Pyodide アセットを検証付きで取得（vendor/secure-exec/crates/execution/assets/pyodide へ配置）。
RUN bash scripts/fetch-sandbox-assets.sh
# orchestrator（shiki workspace）と sidecar（vendor workspace）をそれぞれビルド。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin shiki-sandbox-orchestrator --bin shiki-netns-holder && \
    cp target/release/shiki-sandbox-orchestrator /usr/local/bin/ && \
    cp target/release/shiki-netns-holder /usr/local/bin/ && \
    (cd vendor/secure-exec && cargo build --release --bin secure-exec-sidecar) && \
    cp vendor/secure-exec/target/release/secure-exec-sidecar /usr/local/bin/

# ゲストコマンドスイート（wasm32-wasip1・nightly＋build-std）。orchestrator 本体と独立に
# キャッシュされる重いステージ。rustup が rust-toolchain.toml の nightly を自動取得する。
FROM rust:1.96-bookworm AS commands-builder
ARG BUILD_COMMANDS=1
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
    git curl python3 clang cmake && rm -rf /var/lib/apt/lists/*
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/vendor/secure-exec/registry/native/target \
    mkdir -p /opt/shiki/commands && \
    if [ "$BUILD_COMMANDS" = "1" ]; then \
      # BUILD_C=1: curl/wget 等の C ポートも含める（wasi-sdk はビルド時取得・PIT-33 準拠）。
      BUILD_C=1 COMMANDS_OUT=/opt/shiki/commands bash scripts/build-sandbox-commands.sh; \
    else \
      echo "BUILD_COMMANDS=0: ゲストコマンド同梱をスキップ"; \
    fi

# native ティア用アセット（runsc・#346）: manifest 検証付きでビルド時に取得する
# （実行時 DL 無し・PIT-33・wasm 側 fetch-sandbox-assets.sh と対称）。
FROM debian:bookworm-slim AS native-assets
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY scripts/fetch-native-assets.sh scripts/
COPY deploy/sandbox-assets/native-manifest.sha256 deploy/sandbox-assets/
RUN bash scripts/fetch-native-assets.sh

# native rootfs（gVisor 用・numpy/pandas 同梱・#346）: docker export を使わず、pin イメージへ
# --require-hashes で焼いた層をそのまま最終ステージへ COPY する（digest pin × wheel ハッシュの二層）。
FROM python:3.12-slim@sha256:423ed6ab25b1921a477529254bfeeabf5855151dc2c3141699a1bfc852199fbf AS rootfs-src
COPY deploy/sandbox-assets/rootfs-requirements.txt /tmp/rootfs-requirements.txt
RUN pip install --no-cache-dir --require-hashes --only-binary=:all: \
        -r /tmp/rootfs-requirements.txt \
    && rm /tmp/rootfs-requirements.txt \
    && printf 'nameserver 169.254.0.1\n' > /etc/resolv.conf.sandbox \
    && printf '127.0.0.1 localhost\n' > /etc/hosts.sandbox

FROM debian:bookworm-slim
# iproute2: egress netns holder が `ip` でゲートウェイ IF を構成する。
# e2fsprogs: Firecracker ワークスペース ext4 の非特権生成（PR3）。
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates iproute2 e2fsprogs && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --system --create-home --uid 10001 sandbox
COPY --from=builder /usr/local/bin/shiki-sandbox-orchestrator /usr/local/bin/
COPY --from=builder /usr/local/bin/shiki-netns-holder /usr/local/bin/
COPY --from=builder /usr/local/bin/secure-exec-sidecar /usr/local/bin/
COPY --from=commands-builder /opt/shiki/commands /opt/shiki/commands
# gVisor 既定ティアの前提アセットをイメージへ同梱する（#346・ボリューム上書きも可）。
COPY --from=native-assets /app/deploy/sandbox-assets/bin/runsc /opt/shiki/sandbox-assets/bin/runsc
COPY --from=rootfs-src / /opt/shiki/sandbox-assets/rootfs
RUN mv /opt/shiki/sandbox-assets/rootfs/etc/resolv.conf.sandbox /opt/shiki/sandbox-assets/rootfs/etc/resolv.conf && \
    mv /opt/shiki/sandbox-assets/rootfs/etc/hosts.sandbox /opt/shiki/sandbox-assets/rootfs/etc/hosts && \
    # ネイティブティアの状態ディレクトリを sandbox ユーザーで作成可能に（compose は tmpfs を
    # マウントするが、素の docker run でも起動時 create_dir_all が失敗しないように）。
    mkdir -p /run/sandbox/gvisor /run/sandbox/firecracker && \
    chown -R sandbox:sandbox /run/sandbox
USER sandbox
ENV SECURE_EXEC_SIDECAR_BIN=/usr/local/bin/secure-exec-sidecar
ENV SANDBOX__LISTEN=0.0.0.0:50000
ENV SANDBOX__COMMANDS_DIR=/opt/shiki/commands
ENV SANDBOX__NETNS_HOLDER_BIN=/usr/local/bin/shiki-netns-holder
# gVisor（runsc）/rootfs はイメージ同梱（#346・実行時 DL 無し・PIT-33）。開発ホストで
# 差し替えたい場合のみ deploy/sandbox-assets のボリュームで上書きする。
ENV SANDBOX__GVISOR__RUNSC_BIN=/opt/shiki/sandbox-assets/bin/runsc
ENV SANDBOX__GVISOR__ROOTFS_DIR=/opt/shiki/sandbox-assets/rootfs
ENV SANDBOX__GVISOR__STATE_DIR=/run/sandbox/gvisor
# Firecracker（VM 級隔離・KVM 前提）。有効化には SANDBOX__FIRECRACKER__ENABLED=1 と
# /dev/kvm の device 付与が要る（compose 既定では無効・KVM 非搭載でも起動できるように）。
ENV SANDBOX__FIRECRACKER__BIN=/opt/shiki/sandbox-assets/bin/firecracker
ENV SANDBOX__FIRECRACKER__KERNEL=/opt/shiki/sandbox-assets/vmlinux.bin
ENV SANDBOX__FIRECRACKER__ROOTFS=/opt/shiki/sandbox-assets/rootfs.ext4
ENV SANDBOX__FIRECRACKER__STATE_DIR=/run/sandbox/firecracker
EXPOSE 50000
ENTRYPOINT ["shiki-sandbox-orchestrator"]
