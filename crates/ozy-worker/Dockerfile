# Optional OZY worker: Tesseract OCR + headless Chromium, both driven from Rust.
# Build from the repository root:
#   docker build -f crates/ozy-worker/Dockerfile -t ozy-worker .
#   docker run --rm -p 8080:8080 -e OZY_WORKER_SECRET=<32+ random chars> ozy-worker
FROM rust:1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends libtesseract-dev libleptonica-dev clang pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN cargo build --release -p ozy-worker --features ocr,browser

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends chromium tesseract-ocr tesseract-ocr-eng \
    ca-certificates fonts-liberation fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 --create-home ozy
COPY --from=build /src/target/release/ozy-worker /usr/local/bin/ozy-worker
USER ozy
# The container is the isolation boundary, so Chromium's own sandbox is disabled inside it.
# Run with a read-only root filesystem, no added capabilities and a memory limit (>= 1 GiB).
ENV BIND=0.0.0.0:8080 \
    CHROME_PATH=/usr/bin/chromium \
    OZY_CHROME_NO_SANDBOX=1 \
    OZY_WORKER_CONCURRENCY=2 \
    RUST_LOG=info
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["/bin/sh", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080"]
CMD ["ozy-worker"]
