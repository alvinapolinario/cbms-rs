# ---- Build stage -----------------------------------------------------------------------------
FROM rust:1-bookworm AS builder
WORKDIR /app

# Cache dependency compilation separately from app source changes.
COPY Cargo.toml ./
COPY Cargo.lock* ./
RUN mkdir src \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release \
    && rm -rf src

COPY src ./src
RUN touch src/main.rs && cargo build --release

# ---- Runtime stage ----------------------------------------------------------------------------
FROM debian:bookworm-slim

# deb.debian.org is unreachable from the deploy host. Rise mirrors bookworm main;
# its security suite 404s and bookworm-updates is past Valid-Until, which fails apt.
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/debian.sources \
    && printf 'Types: deb\nURIs: http://mirror.rise.ph/debian\nSuites: bookworm\nComponents: main\n' > /etc/apt/sources.list.d/debian.sources \
    && apt-get -o Acquire::Check-Valid-Until=false update \
    && apt-get install -y --no-install-recommends \
       nginx supervisor ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /etc/nginx/sites-enabled/default

COPY --from=builder /app/target/release/sabonglara-rs /usr/local/bin/sabonglara-rs
COPY web /srv/web
COPY docker/nginx.conf /etc/nginx/sites-enabled/default
COPY docker/supervisord.conf /etc/supervisor/conf.d/supervisord.conf

ENV HOST=0.0.0.0
ENV PORT=8080
ENV WEB_ROOT=/srv/web

EXPOSE 80

CMD ["/usr/bin/supervisord", "-n", "-c", "/etc/supervisor/conf.d/supervisord.conf"]
