# A GitHub Actions runner for ci.yml's heavy jobs, on picur (arm64, OrbStack). Its home is a
# volume, so rustup and cargo stay warm between runs; every target/ is in one volume all runners
# share, /ci-target. See install.sh.
FROM ubuntu:24.04

ARG RUNNER_VERSION=2.338.1
ARG NATS_VERSION=v2.10.22

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev clang libclang-dev cmake \
      ca-certificates curl git jq unzip zstd python3 sudo \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL "https://github.com/nats-io/nats-server/releases/download/${NATS_VERSION}/nats-server-${NATS_VERSION}-linux-arm64.tar.gz" \
      | tar xz --strip-components=1 -C /usr/local/bin "nats-server-${NATS_VERSION}-linux-arm64/nats-server"

RUN useradd -m -s /bin/bash runner && echo 'runner ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/runner \
    && chown runner /usr/local/bin \
    && mkdir /ci-target && chown runner /ci-target

# Outside the home volume, so a new image brings a new runner.
RUN mkdir /opt/runner && cd /opt/runner \
    && curl -fsSL "https://github.com/actions/runner/releases/download/v${RUNNER_VERSION}/actions-runner-linux-arm64-${RUNNER_VERSION}.tar.gz" | tar xz \
    && ./bin/installdependencies.sh && chown -R runner /opt/runner

COPY --chmod=755 entrypoint.sh /entrypoint.sh
ENV PATH=/home/runner/.cargo/bin:$PATH
USER runner
WORKDIR /home/runner
ENTRYPOINT ["/entrypoint.sh"]
