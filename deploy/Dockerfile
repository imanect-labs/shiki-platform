# shiki-server マルチステージビルド。
# build context はリポジトリルート（deploy/compose/docker-compose.yml から指定）。

FROM rust:1.96-bookworm AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin shiki-server && \
    cp /app/target/release/shiki-server /usr/local/bin/shiki-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/shiki-server /usr/local/bin/shiki-server
EXPOSE 8080
ENTRYPOINT ["shiki-server"]
