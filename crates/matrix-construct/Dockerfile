FROM rust:1.91-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY crates ./crates
RUN cargo build --release -p matrix-construct

FROM debian:bookworm-slim
RUN useradd --system --uid 1001 matrix
COPY --from=builder /app/target/release/matrix-construct /usr/local/bin/matrix-construct
USER matrix
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/matrix-construct"]
