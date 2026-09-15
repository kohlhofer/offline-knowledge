# The appliance image: one static-ish binary on Debian 13 slim.
#
#   container build -t offline-knowledge .
#   container run -it --rm -v "$PWD/data:/data" offline-knowledge
#
#   # the web UI, published to the host (verified: -p [host-ip:]host-port:container-port):
#   container run -d --rm -p 127.0.0.1:8080:8080 -v "$PWD/data:/data" offline-knowledge \
#     serve --bind 0.0.0.0:8080
#
#   # the MCP server, over stdio via `container exec` rather than a published port:
#   container exec -i <container> ok mcp
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
EXPOSE 8080
ENTRYPOINT ["ok"]
CMD ["tui"]
