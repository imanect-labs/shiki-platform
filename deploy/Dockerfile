# shiki-server マルチステージビルド。
# build context はリポジトリルート（deploy/compose/docker-compose.yml から指定）。

FROM rust:1.96-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin shiki-server && \
    cp /app/target/release/shiki-server /usr/local/bin/shiki-server

# CSV クエリの隔離 DuckDB ランナー（Task 11P.7・workspace 除外クレート・別 target）。
# メインとは別ビルドで、bundled DuckDB を含む非特権バイナリを作る。
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/crates/tabular/runner/target \
    cargo build --release --manifest-path crates/tabular/runner/Cargo.toml && \
    cp /app/crates/tabular/runner/target/release/shiki-tabular-runner /usr/local/bin/shiki-tabular-runner

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/shiki-server /usr/local/bin/shiki-server
# 隔離ランナーを PATH に置く（SHIKI__TABULAR__RUNNER_PATH の既定 `shiki-tabular-runner`）。
COPY --from=builder /usr/local/bin/shiki-tabular-runner /usr/local/bin/shiki-tabular-runner
EXPOSE 8080
ENTRYPOINT ["shiki-server"]
