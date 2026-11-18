# HWP/HWPX -> PDF converters, built and run locally only (never on the
# production host). Two engines, pinned by checksum:
#   h2o   LibreOffice headless + H2Orestart import filter
#   rhwp  rhwp CLI (native HWP layout engine, byte-reproducible PDF)
#   docker build -t mazimi-hwp2pdf:2 scripts/forms/hwp
#   scripts/forms/hwp/convert.sh <engine> <raw dir> <out dir> <file>...
FROM debian:trixie-slim@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f

ARG H2O_VERSION=0.7.14
ARG H2O_SHA256=cbea23bc37861361bbc534bc0675e5bc67b36f712072490f82a9bf410d7c04d8
ARG RHWP_VERSION=0.8.6
ARG RHWP_SHA256_ARM64=1288aa5609a67574a6b1372397bef9a8941fda9ad16b1224342512f915ae016e
ARG RHWP_SHA256_AMD64=458de22a6b9b86088dfdcd59552d4e8fab5362a64578c64761fa28df9be45e9a
ARG TARGETARCH

RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      libreoffice-writer-nogui libreoffice-java-common default-jre-headless \
      fonts-nanum fonts-nanum-extra fonts-noto-cjk fonts-unfonts-core \
      fonts-liberation fonts-crosextra-carlito fonts-dejavu-core python3-fonttools \
      ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

# Hangul office font names -> Nanum copies with an HWP-like line box.
COPY tighten_fonts.py local.conf /opt/hwp/
RUN python3 /opt/hwp/tighten_fonts.py /usr/share/fonts/truetype/nanum /usr/local/share/fonts/hwp \
 && cp /opt/hwp/local.conf /etc/fonts/local.conf \
 && fc-cache -f

RUN curl -fsSL -o /tmp/H2Orestart.oxt \
      "https://github.com/ebandal/H2Orestart/releases/download/v${H2O_VERSION}/H2Orestart.oxt" \
 && echo "${H2O_SHA256}  /tmp/H2Orestart.oxt" | sha256sum -c - \
 && unopkg add --shared /tmp/H2Orestart.oxt \
 && rm /tmp/H2Orestart.oxt

RUN case "${TARGETARCH}" in \
      arm64) arch=aarch64; sum="${RHWP_SHA256_ARM64}" ;; \
      amd64) arch=x86_64;  sum="${RHWP_SHA256_AMD64}" ;; \
      *) echo "unsupported arch ${TARGETARCH}" >&2; exit 1 ;; \
    esac \
 && curl -fsSL -o /tmp/rhwp.tar.gz \
      "https://github.com/edwardkim/rhwp/releases/download/v${RHWP_VERSION}/rhwp-v${RHWP_VERSION}-linux-${arch}.tar.gz" \
 && echo "${sum}  /tmp/rhwp.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/rhwp.tar.gz -C /opt \
 && ln -s /opt/rhwp/rhwp /usr/local/bin/rhwp \
 && rm /tmp/rhwp.tar.gz

RUN useradd -m conv
USER conv
WORKDIR /work
