# syntax=docker/dockerfile:1
#
# Cameleer as a container: one static binary (no dependencies, all ~230 components, about 26 MB)
# on distroless. It starts in about 10 ms and routes in about 10 MB of memory.
#
#   docker run --rm -v "$PWD:/routes" ghcr.io/pmsudhi/cameleer run /routes/my-route.camel.yaml
#
# The build cross-compiles on the build machine's own architecture to a static musl binary
# for each target platform (linux/amd64, linux/arm64), linked by Rust's bundled rust-lld, so
# no emulation is needed. Base images are pinned by digest.

FROM --platform=$BUILDPLATFORM rust:1.99.1-slim-bookworm@sha256:ed2c95ca377d6571b124b05e75e01207973ded639dce7d5542eb7bcd797e32aa AS build
ARG TARGETARCH
WORKDIR /src
COPY . .
RUN case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-musl ;; \
      arm64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac \
 && rustup target add "$target" \
 && RUSTFLAGS="-C linker=rust-lld -C target-feature=+crt-static -C strip=symbols" \
    cargo build --release --locked -p cameleer-main --bin cameleer --target "$target" \
 && mkdir /out && cp "target/$target/release/cameleer" /out/cameleer

# distroless/static: CA certificates (/etc/ssl/certs), tzdata and a non-root user, no shell
FROM gcr.io/distroless/static-debian12:nonroot@sha256:afa5c872c891853ca7fcf1f12c3edb23f7eeef36189728842dd51042ff57f7ab
COPY --from=build /out/cameleer /usr/local/bin/cameleer
WORKDIR /routes
ENTRYPOINT ["/usr/local/bin/cameleer"]
CMD ["--help"]
