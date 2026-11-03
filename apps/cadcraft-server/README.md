# cadcraft-server

The session server for shared drawings: it orders everyone's edits, passes on presence (cursors,
selections, block locks), keeps cloud files, and serves the web app.

```sh
cadcraft-server --listen 127.0.0.1:9010 --data /var/lib/cadcraft --web dist/web
```

- `--listen ADDR`: where it listens (default `127.0.0.1:9010`).
- `--data DIR`: keep cloud files there (`DIR/<name>.cadsession`, saved every 2 s and when the last
  person leaves). Without it, files live only while someone has them open.
- `--web DIR`: serve the web app built with trunk, so `http://host/s/<name>` opens a file in a
  browser. `http://host/` lists the files with DWG and DXF downloads.

Desktop apps connect with File ▸ Open from Cloud… (or `CADCRAFT_CLOUD=host:port`).

## Encryption (`https://` and `wss://`)

The server speaks plain HTTP and WebSockets. On the internet, run it behind a proxy that adds TLS,
and keep it listening on `127.0.0.1` only. With [Caddy](https://caddyserver.com), which gets a
certificate by itself:

```
cad.example.com {
    reverse_proxy 127.0.0.1:9010
}
```

With nginx, pass the WebSocket upgrade through:

```
location / {
    proxy_pass http://127.0.0.1:9010;
    proxy_http_version 1.1;
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection "upgrade";
    proxy_read_timeout 1h;
}
```

People then use `cad.example.com` (or `https://cad.example.com`) as the server address; the apps
connect over `wss://`.

There are no accounts yet: anyone who can reach the server sees every file.
