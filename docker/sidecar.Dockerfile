# VirtRS CG sidecar: Node + headless Chromium (no GPU). amd64 and arm64.
FROM node:22-bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends chromium fonts-dejavu-core fonts-noto-core \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY package.json ./
COPY bin ./bin
COPY src ./src

ENV VIZRS_BROWSER=/usr/bin/chromium \
    LISTEN=0.0.0.0:7700
USER node
EXPOSE 7700
CMD ["node", "bin/virtrs-cg.js"]
