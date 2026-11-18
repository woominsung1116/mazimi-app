# Forms job image: the network-free python/node steps of the form-template pipeline
# (scripts/forms/auto/steps.sh, choose/fetch/verdicts, slots.py, judge.py) and the app's own
# fill code (apps/mobile/lib/formFill) that fill.cjs drives with fake values.
#   docker build -f scripts/forms/server/Dockerfile -t mazimi-forms-job:1 .
# Every input is pinned: base images by digest, python wheels by hash (requirements.txt,
# --require-hashes, binary only), node packages by package-lock.json (npm ci --ignore-scripts).
# The image holds no secret and runs as an unprivileged user; the server runner starts it with
# no network (fetch.py alone gets egress), a read-only root and fresh output directories.
FROM node:22-slim@sha256:de170b39439eb0e5a3444ad7d60b015614a381e10fecae2e30ec9e735122f38d AS node
WORKDIR /deps
COPY scripts/forms/server/node/package.json scripts/forms/server/node/package-lock.json ./
RUN npm ci --omit=optional --omit=peer --ignore-scripts --no-audit --no-fund

FROM python:3.12-slim@sha256:3d5ed973e45820f5ba5e46bd065bd88b3a504ff0724d85980dcd05eab361fcf4
# poppler-utils: pdftoppm, the second renderer (scripts/forms/auto/xcheck.py).
RUN apt-get update && apt-get install -y --no-install-recommends libstdc++6 poppler-utils && rm -rf /var/lib/apt/lists/*
COPY --from=node /usr/local/bin/node /usr/local/bin/node
COPY scripts/forms/server/requirements.txt /tmp/requirements.txt
RUN pip install --no-cache-dir --no-deps --require-hashes --only-binary :all: -r /tmp/requirements.txt && rm /tmp/requirements.txt
WORKDIR /opt/repo
COPY apps/mobile/package.json apps/mobile/package.json
COPY apps/mobile/lib/formFill apps/mobile/lib/formFill
COPY apps/mobile/assets/fonts/NanumGothic-Regular.ttf apps/mobile/assets/fonts/NanumGothic-Regular.ttf
COPY --from=node /deps/node_modules apps/mobile/node_modules
COPY scripts/forms scripts/forms
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1 MPLCONFIGDIR=/tmp
# The forms user (scripts/forms/server/state.sh): no host account shares its uid or gid.
RUN groupadd -g 1999 mazimi-forms && useradd -u 1999 -g 1999 -m job
USER job
WORKDIR /work
