# Engine benchmark: Chromium (as the sidecar) vs WPE WebKit (GStreamer wpevideosrc).
# Build from the repository root: docker build -f bench/Dockerfile .
# Debian trixie: WPE WebKit 2.48 with the WPE GStreamer plugin; amd64 and arm64.
FROM node:current-trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       chromium-headless-shell \
       gstreamer1.0-wpe gstreamer1.0-tools gstreamer1.0-plugins-base \
       libegl-mesa0 libgles2 libgl1-mesa-dri \
       fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force
COPY src src
COPY bench bench
# A named volume on /results takes over this owner.
RUN mkdir /results && chown 10001 /results

ENV HOME=/tmp \
    VIZRS_BROWSER=/usr/bin/chromium-headless-shell \
    BENCH_RESULTS=/results \
    PORT=7790

USER 10001
EXPOSE 7790
CMD ["node", "bench/bench.mjs"]
