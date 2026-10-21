# Production image for remail-server (the desktop app isn't built here), for
# hosting platforms that build from the repository. It sits at the repository
# root because platforms use the Dockerfile's folder as the build context.
# TLS is the platform's job. See deploy/server/README.md.
#
# The master key never comes from the image: set the REMAIL_MASTER_KEY secret
# (base64 of the key file) or mount a key file at REMAIL_MASTER_KEY_FILE.
# The server refuses to start with a key that doesn't match its database.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
# Thin LTO: much less memory and time than the desktop app's full LTO.
ENV CARGO_PROFILE_RELEASE_LTO=thin \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
    CARGO_TERM_COLOR=never
COPY . .
RUN cargo build -p remail-server --release --locked \
 && cp target/release/remail-server /usr/local/bin/remail-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --home /var/lib/remail remail
COPY --from=build /usr/local/bin/remail-server /usr/local/bin/remail-server
USER remail
ENV REMAIL_MODE=hosted \
    REMAIL_TRUST_PROXY=1 \
    PORT=8080 \
    RUST_LOG=info,sqlx=warn
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/remail-server"]
CMD ["serve"]
