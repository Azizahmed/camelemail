# remail-server for hosting platforms that build from the repository root
# (CranL). The image is built by GitHub Actions from
# deploy/server/build.Dockerfile (.github/workflows/server-image.yml), which
# also rewrites the FROM line below to each new image digest, so the platform
# only pulls a ready image and never compiles Rust. Don't edit the FROM line
# by hand. Runtime settings and the master key: deploy/server/README.md.
FROM ghcr.io/azizahmed/remail-server@sha256:c64f27cf9c4c3309a79f56507ea6aa8cf5182123caa70a35c11e7dd79a818a82
