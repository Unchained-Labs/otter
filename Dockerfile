FROM rust:1.86-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.toml
COPY otter-core/Cargo.toml otter-core/Cargo.toml
COPY otter-server/Cargo.toml otter-server/Cargo.toml
COPY otter-worker/Cargo.toml otter-worker/Cargo.toml
COPY vendor vendor
COPY otter-core/src otter-core/src
COPY otter-server/src otter-server/src
COPY otter-worker/src otter-worker/src
COPY otter-core/migrations otter-core/migrations

ARG BIN_NAME
RUN cargo build --release --bin otter-server --bin otter-worker

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        coreutils \
        curl \
        dnsutils \
        findutils \
        gawk \
        git \
        gnupg \
        iproute2 \
        iputils-ping \
        jq \
        less \
        lsb-release \
        lsof \
        make \
        nano \
        net-tools \
        openssh-client \
        procps \
        psmisc \
        python3 \
        python3-pip \
        python3-venv \
        rsync \
        tar \
        tree \
        unzip \
        vim-tiny \
        wget \
        zip && \
    install -m 0755 -d /etc/apt/keyrings && \
    curl -fsSL https://download.docker.com/linux/debian/gpg -o /etc/apt/keyrings/docker.asc && \
    chmod a+r /etc/apt/keyrings/docker.asc && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian \
    $(. /etc/os-release && echo \"$VERSION_CODENAME\") stable" > /etc/apt/sources.list.d/docker.list && \
    apt-get update && \
    apt-get install -y --no-install-recommends \
        docker-ce-cli \
        docker-buildx-plugin \
        docker-compose-plugin && \
    python3 -m pip install --no-cache-dir --break-system-packages psutil && \
    rm -rf /var/lib/apt/lists/*

RUN curl -LsSf https://mistral.ai/vibe/install.sh | bash && \
    ln -sf /root/.local/bin/vibe /usr/local/bin/vibe

WORKDIR /srv/otter
COPY --from=builder /app/otter-core/migrations /srv/otter/migrations
COPY --from=builder /app/target/release/otter-server /usr/local/bin/otter-server
COPY --from=builder /app/target/release/otter-worker /usr/local/bin/otter-worker

ENTRYPOINT ["/usr/local/bin/otter-server"]
