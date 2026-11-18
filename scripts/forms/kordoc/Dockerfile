# kordoc table-IR job image: parse only (ir.mjs), never fill, no MCP server, no OCR models.
#   docker build -t mazimi-kordoc:1 scripts/forms/kordoc
#   scripts/forms/hwp/convert.sh kordoc <raw dir> <out dir> <file>...
# kordoc is pinned to an exact version with integrity hashes in package-lock.json and is
# installed without optional (onnxruntime, pdfjs, sharp) or peer (puppeteer) dependencies and
# without install scripts. Bump only by hand, at most monthly, to a version at least 14 days
# old, after re-running the kordoc spike measurements (see .reports/kordoc-spike.md §4).
# node:22-slim, multi-arch index digest (arm64 for the Oracle host, amd64 for CI).
FROM node:22-slim@sha256:de170b39439eb0e5a3444ad7d60b015614a381e10fecae2e30ec9e735122f38d

ENV NODE_ENV=production KORDOC_OFFLINE=1 NPM_CONFIG_UPDATE_NOTIFIER=false NPM_CONFIG_FUND=false NPM_CONFIG_AUDIT=false
WORKDIR /opt/kordoc
COPY package.json package-lock.json ./
RUN npm ci --omit=optional --omit=peer --ignore-scripts \
 && test "$(node -p 'require("./node_modules/kordoc/package.json").version')" = "4.24.7" \
 && npm cache clean --force \
 && rm -rf /root/.npm
COPY ir.mjs ./

# The base image's unprivileged user; the host gives it a fresh output dir per run.
USER node
ENTRYPOINT ["node", "/opt/kordoc/ir.mjs"]
