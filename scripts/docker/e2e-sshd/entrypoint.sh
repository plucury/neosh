#!/usr/bin/env bash
set -euo pipefail

mkdir -p /var/run/sshd /home/e2e/.ssh
if [[ -n "${E2E_AUTHORIZED_KEY:-}" ]]; then
  printf '%s\n' "$E2E_AUTHORIZED_KEY" > /home/e2e/.ssh/authorized_keys
fi
chown -R e2e:e2e /home/e2e/.ssh
chmod 700 /home/e2e/.ssh
if [[ -f /home/e2e/.ssh/authorized_keys ]]; then
  chmod 600 /home/e2e/.ssh/authorized_keys
fi

ssh-keygen -A >/dev/null 2>&1 || true

exec /usr/sbin/sshd -D -e
