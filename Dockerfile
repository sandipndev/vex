FROM rust:1 AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN cargo build --workspace --release

FROM debian:trixie-slim
COPY --from=builder /build/target/release/vexd /usr/local/bin/vexd
EXPOSE 7422 7423
ENTRYPOINT ["vexd"]
CMD ["start"]
