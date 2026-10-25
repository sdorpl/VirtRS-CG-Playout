# VirtRS CG sidecar: Rust binary + headless Chromium (no GPU), amd64 and arm64.
# The binary is built on the build host's platform and cross-compiled for the
# target (pure Rust, no C libraries): no emulation, minutes instead of an hour
# under QEMU. chromium-headless-shell renders /output with about half the CPU
# of the full browser (no browser UI layer); same engine as `chromium`.

FROM --platform=$BUILDPLATFORM rust:1.97-bookworm AS build
ARG TARGETARCH
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /target ;; \
      arm64) echo aarch64-unknown-linux-gnu > /target \
             && dpkg --add-architecture arm64 \
             && apt-get update \
             && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
             && rm -rf /var/lib/apt/lists/* ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$(cat /target)"
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
WORKDIR /src
COPY rust/Cargo.toml rust/Cargo.lock ./
COPY rust/src src
RUN cargo build --release --locked --target "$(cat /target)" \
    && cp "target/$(cat /target)/release/virtrs-cg" /virtrs-cg

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends chromium-headless-shell fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /virtrs-cg /usr/local/bin/virtrs-cg
WORKDIR /app
COPY client client
COPY etv etv

# Chromium needs a writable HOME (crashpad) for the unprivileged user. The
# browser binary itself, not Debian's /usr/bin wrapper: the wrapper runs it as
# a child of sh, so stopping and the parent-death signal would only reach sh.
ENV HOME=/tmp \
    VIZRS_BROWSER=/usr/lib/chromium/chromium-headless-shell \
    VIZRS_BROWSER_NO_SANDBOX=1 \
    LISTEN=0.0.0.0:7700

USER 10001
EXPOSE 7700
CMD ["virtrs-cg"]
