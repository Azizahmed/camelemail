# remail-server for hosting platforms that build from the repository root
# (CranL). The image is built by GitHub Actions from
# deploy/server/build.Dockerfile (.github/workflows/server-image.yml), which
# also rewrites the FROM line below to each new image digest, so the platform
# only pulls a ready image and never compiles Rust. Don't edit the FROM line
# by hand. Runtime settings and the master key: deploy/server/README.md.
FROM ghcr.io/azizahmed/remail-server@sha256:a820076122e6071301cd858a0595ff592b2555a05e0064500bf7420d474621e5
