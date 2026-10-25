# Seed of the stack (DOKUMENTACJA §4): the reference instance, the ErsatzTV
# clients and the graphics element, baked in so the stack needs no files on
# the host (Portainer builds it from the repository). amd64 and arm64.
# Build from the repository root: docker build -f seed/Dockerfile .

# The ErsatzTV programs (vizrs-etv-client, vizrs-ffmpeg) run inside the
# ErsatzTV container: static musl binaries, whatever libc that image has. Cross-compiled on the build
# host's platform like the sidecar (../Dockerfile); built on arm64 itself
# (e.g. Portainer on an Ampere A1) the native gcc links.
FROM --platform=$BUILDPLATFORM rust:1.97-bookworm AS clients
ARG TARGETARCH
ARG BUILDARCH
RUN case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-musl > /target ;; \
      arm64) echo aarch64-unknown-linux-musl > /target \
             && if [ "$BUILDARCH" != arm64 ]; then \
                  apt-get update \
                  && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu \
                  && rm -rf /var/lib/apt/lists/*; \
                fi ;; \
      *) echo "unsupported architecture $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && rustup target add "$(cat /target)"
# The native gcc of Debian arm64 has the same name.
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-gnu-gcc
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked --bin vizrs-etv-client --bin vizrs-ffmpeg --target "$(cat /target)" \
    && mkdir /client \
    && cp "target/$(cat /target)/release/vizrs-etv-client" "target/$(cat /target)/release/vizrs-ffmpeg" /client/

FROM busybox:1.37
COPY seed/init.sh /seed/init.sh
COPY seed/vizrs-data /seed/vizrs-data
COPY seed/ersatztv-config /seed/ersatztv-config
COPY seed/media /seed/media
COPY --from=clients /client /seed/client
COPY etv/templates/graphics-elements/script /seed/templates
ENTRYPOINT ["sh", "/seed/init.sh"]
