# Taro Enterprise API (serves the Customer Portal and the Provider Console too).
# docker build -t taro-enterprise -f enterprise/Dockerfile enterprise
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p taro-enterprise

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates tini && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /srv/taro taro
WORKDIR /srv/taro
COPY --from=build /src/target/release/taro-enterprise /usr/local/bin/taro-enterprise
COPY apps/customer-portal apps/customer-portal
COPY apps/provider-console apps/provider-console
COPY packages/design-tokens packages/design-tokens
ENV TARO_WEB_DIR=/srv/taro TARO_LISTEN=0.0.0.0:8080 TARO_OPS_LISTEN=127.0.0.1:8081
USER taro
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /healthz HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q ok <&3"]
ENTRYPOINT ["tini", "--", "taro-enterprise"]
CMD ["serve"]
