#!/usr/bin/env bash
# Install or update the CADCraft cloud server on an Ubuntu box that already runs Caddy.
# Run as root from a folder holding: cadcraft-server (the x86_64-unknown-linux-musl build),
# web/ (trunk's dist/web), cadcraft-server.service and cadcraft.caddy.
# It deletes nothing: the web app is replaced folder by folder, the old one kept as web.old.
set -euo pipefail
cd "$(dirname "$0")"

id cadcraft >/dev/null 2>&1 || useradd --system --home-dir /var/lib/cadcraft --shell /usr/sbin/nologin cadcraft
install -d -o root -g root -m 755 /opt/cadcraft
install -d -o cadcraft -g cadcraft -m 750 /var/lib/cadcraft
install -o root -g root -m 755 cadcraft-server /opt/cadcraft/cadcraft-server.new
mv -f /opt/cadcraft/cadcraft-server.new /opt/cadcraft/cadcraft-server
if [ -d /opt/cadcraft/web ]; then
    rm -rf /opt/cadcraft/web.old
    mv /opt/cadcraft/web /opt/cadcraft/web.old
fi
cp -r web /opt/cadcraft/web
chown -R root:root /opt/cadcraft/web

install -o root -g root -m 644 cadcraft-server.service /etc/systemd/system/cadcraft-server.service
systemctl daemon-reload
systemctl enable cadcraft-server
systemctl restart cadcraft-server
sleep 1
curl -fsS http://127.0.0.1:9010/ >/dev/null && echo "cadcraft-server: up on 127.0.0.1:9010"

# Caddy: the site file, imported once from the main Caddyfile (backed up first).
install -o root -g root -m 644 cadcraft.caddy /etc/caddy/conf.d/cadcraft.caddy
if ! grep -q 'conf.d/cadcraft.caddy' /etc/caddy/Caddyfile; then
    cp /etc/caddy/Caddyfile "/etc/caddy/Caddyfile.bak-$(date -u +%Y%m%d-%H%M%S)"
    sed -i '1i import /etc/caddy/conf.d/cadcraft.caddy' /etc/caddy/Caddyfile
fi
caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
systemctl reload caddy
echo "caddy: reloaded with cad.dotai.co.in"
