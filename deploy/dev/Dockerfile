# Development image for remail-server (no TLS; put Caddy in front for real use).
# Build context is the repository root:  docker compose -f deploy/dev/docker-compose.yml build
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build -p remail-server --release \
 && cp target/release/remail-server /usr/local/bin/remail-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /var/lib/remail remail \
 && mkdir -p /etc/remail && chown remail /etc/remail
COPY --from=build /usr/local/bin/remail-server /usr/local/bin/remail-server
COPY deploy/dev/entrypoint.sh /usr/local/bin/remail-entrypoint
USER remail
ENTRYPOINT ["/usr/local/bin/remail-entrypoint"]
CMD ["serve"]
