# One self-contained image for tempo: the native `comp-host` binary + the composed
# component + the built SPA. No wasmCloud, no providers, no lattice — comp-host
# serves wasi:http, the SPA, and wasi:keyvalue (Redis) in a single process.
#
# Prereqs (produce the two prebuilt artifacts, once):
#   just compose-tempo        # -> components/target/tempo_domain.composed.wasm
#   just build-tempo-ui       # -> examples/tempo/dist
# Build (context = the comp/ repo root):
#   docker build -f examples/tempo/Dockerfile -t tempo .
# Run (Redis/Valkey URL at runtime — rediss:// for managed, TLS):
#   docker run -p 8080:8080 -e REDIS_URL='rediss://default:pw@host:25061' tempo
#
# comp-host is a normal Rust binary; only it is compiled here. The .wasm is
# architecture-independent and the dist is static, so both are just copied in.
FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY host ./host
RUN cd host && cargo build --release --bin comp-host

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/host/target/release/comp-host /usr/local/bin/comp-host
COPY components/target/tempo_domain.composed.wasm /app/tempo.wasm
COPY examples/tempo/dist /app/dist
# The ONLY runtime input tempo needs is REDIS_URL (rediss://… for managed TLS).
# The session-signing secret lives in the KV store (Redis), so sessions survive
# restarts on their own — no key env needed. (VET_* is the host's legacy env
# prefix; tempo uses none of the vet-specific knobs.)
ENV ADDR=0.0.0.0:8080 KV=redis
EXPOSE 8080
CMD ["/bin/sh", "-lc", "exec comp-host --component /app/tempo.wasm --addr ${ADDR} --kv ${KV} --redis-url \"${REDIS_URL}\" --static-dir /app/dist"]
