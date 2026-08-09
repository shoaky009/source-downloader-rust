FROM debian:bookworm-slim

ARG APP_UID=1000
ARG TARGETARCH
ARG APP_GID=1000

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd \
        --gid ${APP_GID} \
        source-downloader \
    && useradd \
        --uid ${APP_UID} \
        --gid ${APP_GID} \
        --no-create-home \
        --home-dir /nonexistent \
        --shell /usr/sbin/nologin \
        source-downloader \
    && mkdir -p /app/data /app/plugins \
    && chown -R ${APP_UID}:${APP_GID} /app


COPY --chown=${APP_UID}:${APP_GID} \
    container-binaries/${TARGETARCH}/source-downloader-web \
    /app/source-downloader-web


ENV SOURCE_DOWNLOADER_DATA_LOCATION=/app/data \
    SOURCE_DOWNLOADER_PLUGIN_LOCATION=/app/plugins \
    SOURCE_DOWNLOADER_SERVER_HOST=0.0.0.0 \
    SOURCE_DOWNLOADER_SERVER_PORT=8080 \
    RUST_LOG=info


WORKDIR /app

USER ${APP_UID}:${APP_GID}

VOLUME ["/app/data", "/app/plugins"]

EXPOSE 8080


ENTRYPOINT ["/app/source-downloader-web"]