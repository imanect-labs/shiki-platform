# sandbox-orchestrator（非特権プロセス）＋ secure-exec-sidecar バイナリ＋Pyodide アセット。
# build context はリポジトリルート（deploy/compose/docker-compose.yml から指定）。
#
# 注意: sidecar は V8（rusty_v8 130）をリンクするため build が重い。Pyodide 一式は
# scripts/fetch-sandbox-assets.sh が asset-manifest.sha256 に基づき検証付きで取得する（実行時取得禁止・PIT-33）。
# ゲストコマンドスイート（ls/curl/wget 等）は scripts/build-sandbox-commands.sh で別途ビルドし
# software として同梱する（nightly + wasm32-wasip1・重いため本 Dockerfile では任意ステップ）。

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

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --system --create-home --uid 10001 sandbox
COPY --from=builder /usr/local/bin/shiki-sandbox-orchestrator /usr/local/bin/
COPY --from=builder /usr/local/bin/secure-exec-sidecar /usr/local/bin/
USER sandbox
ENV SECURE_EXEC_SIDECAR_BIN=/usr/local/bin/secure-exec-sidecar
ENV SANDBOX__LISTEN=0.0.0.0:50000
EXPOSE 50000
ENTRYPOINT ["shiki-sandbox-orchestrator"]
