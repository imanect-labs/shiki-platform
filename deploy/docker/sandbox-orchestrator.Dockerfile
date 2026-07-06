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
    cargo build --release --bin shiki-sandbox-orchestrator && \
    cp target/release/shiki-sandbox-orchestrator /usr/local/bin/ && \
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

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --system --create-home --uid 10001 sandbox
COPY --from=builder /usr/local/bin/shiki-sandbox-orchestrator /usr/local/bin/
COPY --from=builder /usr/local/bin/secure-exec-sidecar /usr/local/bin/
COPY --from=commands-builder /opt/shiki/commands /opt/shiki/commands
USER sandbox
ENV SECURE_EXEC_SIDECAR_BIN=/usr/local/bin/secure-exec-sidecar
ENV SANDBOX__LISTEN=0.0.0.0:50000
ENV SANDBOX__COMMANDS_DIR=/opt/shiki/commands
EXPOSE 50000
ENTRYPOINT ["shiki-sandbox-orchestrator"]
