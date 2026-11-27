FROM rust:1-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo test --locked --release && cargo build --locked --release

FROM debian:trixie-slim AS upstream
ARG TARGETARCH
ARG CBM_VERSION=v0.12.1
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN set -eux; \
    case "$TARGETARCH" in amd64|arm64) ;; *) exit 1 ;; esac; \
    base="https://github.com/DeusData/codebase-memory-mcp/releases/download/$CBM_VERSION"; \
    asset="codebase-memory-mcp-linux-$TARGETARCH-portable.tar.gz"; \
    cd /tmp; \
    curl -fsSL "$base/checksums.txt" -o checksums.txt; \
    curl -fsSL "$base/$asset" -o "$asset"; \
    grep "  $asset$" checksums.txt | sha256sum -c -; \
    mkdir unpack; tar -xzf "$asset" -C unpack; \
    binary="$(find unpack -type f -name codebase-memory-mcp -print -quit)"; \
    test -n "$binary"; install -m 0755 "$binary" /usr/local/bin/codebase-memory-mcp; \
    curl -fsSL "https://raw.githubusercontent.com/DeusData/codebase-memory-mcp/$CBM_VERSION/LICENSE" -o /tmp/CBM-LICENSE; \
    codebase-memory-mcp --version; rm -rf unpack "$asset" checksums.txt

FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd -g 10001 codebase \
    && useradd -m -u 10001 -g 10001 -s /usr/sbin/nologin codebase \
    && mkdir -p /opt/codebase /data/cbm /repos /usr/share/licenses/codebase-memory-server \
    && chown -R 10001:10001 /data/cbm /repos /home/codebase
COPY --from=build /src/target/release/codebase-memory-server /usr/local/bin/
COPY --from=upstream /usr/local/bin/codebase-memory-mcp /usr/local/bin/
COPY --from=upstream /tmp/CBM-LICENSE /usr/share/licenses/codebase-memory-server/CBM-LICENSE
COPY --chmod=0755 git_askpass.sh /opt/codebase/git_askpass.sh
COPY LICENSE /usr/share/licenses/codebase-memory-server/LICENSE
ENV HOME=/home/codebase CBM_CACHE_DIR=/data/cbm CBM_ALLOWED_ROOT=/repos CBM_WORKERS=2 LISTEN_ADDR=0.0.0.0:8000
USER 10001:10001
EXPOSE 8000
ENTRYPOINT ["codebase-memory-server"]
CMD ["serve"]
