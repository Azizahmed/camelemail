# remail-server for hosting platforms that build from the repository root
# (CranL). The image is built by GitHub Actions from
# deploy/server/build.Dockerfile (.github/workflows/server-image.yml), which
# also rewrites the FROM line below to each new image digest, so the platform
# only pulls a ready image and never compiles Rust. Don't edit the FROM line
# by hand. Runtime settings and the master key: deploy/server/README.md.
FROM ghcr.io/azizahmed/remail-server@sha256:078a171dca7338a31ae26b3867057ba904b64470df88699161d6376757b862c0
