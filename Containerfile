# The appliance image: one static-ish binary on Debian 13 slim.
#
#   container build -t offline-knowledge .
#   container run -it --rm -v "$PWD/data:/data" offline-knowledge
#
# The ZIM file and its .okx index live in the mounted /data directory.

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p ok \
 && cp target/release/ok /usr/local/bin/ok

FROM debian:trixie-slim
COPY --from=build /usr/local/bin/ok /usr/local/bin/ok
ENV LANG=C.UTF-8 \
    TERM=xterm-256color \
    OK_ZIM=/data/wikipedia_en_top_nopic_2026-06.zim
WORKDIR /data
ENTRYPOINT ["ok"]
CMD ["tui"]
