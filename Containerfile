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
#   # what loaded, what did not:
#   container exec -i <container> ok collections
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
# Every imported *.zim directly inside /data is a collection. The default is
# pinned rather than left to load order: without OK_COLLECTION the first
# filename wins, so a ZIM dropped into /data that sorts before the pinned one
# would take over the home page, the brand and every unqualified request.
# OK_COLLECTION=<label> is how you change it, and `ok collections` prints the
# labels on the mount.
ENV LANG=C.UTF-8 \
    TERM=xterm-256color \
    OK_ZIM=/data \
    OK_COLLECTION=wikipedia
WORKDIR /data
EXPOSE 8080
ENTRYPOINT ["ok"]
CMD ["tui"]
