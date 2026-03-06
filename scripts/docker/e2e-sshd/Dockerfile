FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends openssh-server ca-certificates bash \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash e2e \
    && mkdir -p /var/run/sshd /home/e2e/.ssh \
    && chown -R e2e:e2e /home/e2e/.ssh \
    && echo "e2e:e2e" | chpasswd

COPY sshd_config /etc/ssh/sshd_config
COPY entrypoint.sh /entrypoint.sh

RUN chmod +x /entrypoint.sh

EXPOSE 22

ENTRYPOINT ["/entrypoint.sh"]
