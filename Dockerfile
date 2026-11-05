# syntax=docker/dockerfile:1
#
# Cameleer: Apache Camel routes, rebuilt in Rust, as one native binary on distroless.
#
#   docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/my-route.camel.yaml
#
# Build: the builder runs on the build machine's own architecture and cross-compiles for each
# target platform (linux/amd64, linux/arm64) with Debian's cross toolchain, so no emulation is
# needed. The binary links glibc rather than static musl: musl measured 3-17% slower
# (allocation-heavy routes most). Builder and runtime are both Debian 12 (glibc 2.36). Base
# images are pinned by digest (Dependabot keeps them current).
# Runtime: non-root (65532), no shell, works with a read-only root filesystem; logs go to
# stdout; SIGTERM stops routes gracefully.

FROM --platform=$BUILDPLATFORM rust:1.99.1-slim-bookworm@sha256:ed2c95ca377d6571b124b05e75e01207973ded639dce7d5542eb7bcd797e32aa AS build
ARG TARGETARCH
ARG BUILDARCH
# the target's standard library and, when cross-compiling, its linker and glibc (signed Debian
# packages); in their own layer so source changes don't fetch them again
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) target=x86_64-unknown-linux-gnu; gnu=x86-64-linux-gnu; dev=amd64 ;; \
      arm64) target=aarch64-unknown-linux-gnu; gnu=aarch64-linux-gnu; dev=arm64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    rustup target add "$target"; \
    linker=cc; \
    if [ "$TARGETARCH" != "$BUILDARCH" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends "gcc-$gnu" "libc6-dev-$dev-cross"; \
      rm -rf /var/lib/apt/lists/*; \
      linker="$gnu-gcc"; \
    fi; \
    echo "$target" > /target; echo "$linker" > /linker
WORKDIR /src
COPY . .
# reproducible: no build paths in the binary (panic messages), timestamps from the commit
ARG SOURCE_DATE_EPOCH
# component families (cameleer-main cargo features), comma separated; empty = all of them.
# A smaller image for one integration, e.g. --build-arg FEATURES=cloud (core, HTTP and YAML are
# always in; FEATURES=core is only those).
ARG FEATURES=
RUN --mount=type=cache,target=/src/target,id=cameleer-target-$TARGETARCH \
    target=$(cat /target) \
 && export CARGO_TARGET_$(echo "$target" | tr 'a-z-' 'A-Z_')_LINKER=$(cat /linker) \
 && RUSTFLAGS="-C strip=symbols --remap-path-prefix=/src=. --remap-path-prefix=/usr/local/cargo=cargo" \
    cargo build --profile dist --locked -p cameleer-main --bin cameleer --target "$target" \
      ${FEATURES:+--no-default-features --features "$FEATURES"} \
 && install -D "target/$target/dist/cameleer" /out/cameleer

# distroless/cc: glibc, libgcc, CA certificates (/etc/ssl/certs), tzdata, /tmp and a non-root
# user; no shell
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /out/cameleer /usr/local/bin/cameleer
COPY LICENSE NOTICE /usr/share/doc/cameleer/
# one integration per container: no status/log files for `camel ps` (logs go to stdout)
ENV CAMEL_LOCAL_STATUS=false \
    CAMEL_HOME=/tmp/cameleer
LABEL org.opencontainers.image.title="Cameleer" \
      org.opencontainers.image.description="Apache Camel routes, rebuilt in Rust: one native binary, 10 ms startup, about 10 MB of memory." \
      org.opencontainers.image.source="https://github.com/pmsudhi/cameleer" \
      org.opencontainers.image.url="https://pmsudhi.github.io/cameleer/" \
      org.opencontainers.image.documentation="https://github.com/pmsudhi/cameleer/blob/main/docs/DEPLOYMENT.md" \
      org.opencontainers.image.licenses="Apache-2.0"
WORKDIR /routes
EXPOSE 8080
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/cameleer"]
CMD ["--help"]
