FROM debian:trixie-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /app/data /app/plugins \
    && chown -R 1000:1000 /app

COPY --chmod=755 \
    --chown=1000:1000 \
    source-downloader-web \
    /app/source-downloader-web


ENV SOURCE_DOWNLOADER_DATA_LOCATION=/app/data \
    SOURCE_DOWNLOADER_PLUGIN_LOCATION=/app/plugins \
    SOURCE_DOWNLOADER_SERVER_HOST=0.0.0.0 \
    SOURCE_DOWNLOADER_SERVER_PORT=8080 \
    RUST_LOG=info


WORKDIR /app

USER 1000:1000

VOLUME ["/app/data", "/app/plugins"]

EXPOSE 8080

ENTRYPOINT ["/app/source-downloader-web"]