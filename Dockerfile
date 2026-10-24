# VirtRS CG sidecar: Node + headless Chromium (no GPU), amd64 and arm64.
# chromium-headless-shell renders /output with about half the CPU of the full
# browser (no browser UI layer); same engine and version as `chromium`.
FROM node:current-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends chromium-headless-shell fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --omit=dev && npm cache clean --force
COPY bin bin
COPY src src
COPY client client
COPY etv etv

# Chromium needs a writable HOME (crashpad) for the unprivileged user.
ENV HOME=/tmp \
    VIZRS_BROWSER=/usr/bin/chromium-headless-shell \
    VIZRS_BROWSER_NO_SANDBOX=1 \
    LISTEN=0.0.0.0:7700

USER 10001
EXPOSE 7700
CMD ["node", "bin/virtrs-cg.js"]
