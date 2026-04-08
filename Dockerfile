FROM node:22-bookworm-slim

ARG CODEX_VERSION=latest

ENV NODE_ENV=production \
    HOST=0.0.0.0 \
    PORT=3000 \
    CODEX_HOME=/codex-home \
    MAX_SESSIONS=8 \
    SESSION_TTL_MS=1800000 \
    SESSION_SWEEP_INTERVAL_MS=60000

WORKDIR /app

RUN npm install -g @openai/codex@${CODEX_VERSION} \
    && mkdir -p /codex-home \
    && codex --version

COPY package.json README.md ./
COPY public ./public
COPY src ./src

EXPOSE 3000

CMD ["node", "src/server.mjs"]
