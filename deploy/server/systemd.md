# Vibex Headless Runtime Systemd Unit

Run `vibex-server` directly on a Linux host without Docker. The unit assumes
the binary is installed at `/usr/local/bin/vibex-server` and state lives in
`/var/lib/vibex-server` (the `vibex` system user's home).

Install:

```bash
sudo install -m 644 deploy/server/vibex-server.service /etc/systemd/system/
sudo useradd --system --home /var/lib/vibex-server --shell /usr/sbin/nologin vibex || true
sudo mkdir -p /var/lib/vibex-server
sudo chown vibex:vibex /var/lib/vibex-server
# TLS when VIBEX_TLS_MODE=server_certificate:
#   /etc/vibex-server/tls.crt and /etc/vibex-server/tls.key (owned by vibex)
sudo systemctl daemon-reload
sudo systemctl enable --now vibex-server
```

Configure by editing the `Environment=` lines in the unit or by dropping an
`/etc/default/vibex-server` file referenced through
`EnvironmentFile=/etc/default/vibex-server` (uncomment the line in the unit).

The unit only publishes on loopback by default
(`VIBEX_BIND_ADDR=127.0.0.1:8765`, `VIBEX_DEPLOYMENT_MODE=loopback`). For a
publicly reachable deployment raise `VIBEX_DEPLOYMENT_MODE=public` and choose
a TLS mode; the gateway refuses to start a Public listener without trusted
TLS, and every unauthenticated endpoint is rate-limited per peer.

Read the pairing code after the first start:

```bash
journalctl -u vibex-server -o cat | grep pairing_code=
# or mint a fresh code any time:
sudo -u vibex VIBEX_HOME=/var/lib/vibex-server vibex-server pairing-code \
    --permission full-control
```

Revoke a lost device:

```bash
sudo -u vibex VIBEX_HOME=/var/lib/vibex-server vibex-server revoke DEVICE_ID \
    --reason "lost phone"
```
