# grindr-always-online

Rust script that keeps a Grindr account always "online" by sending a `GET /v4/cascade` request at random intervals (2–15 min apart). Powered by [grindr.rs](https://git.opengrind.org/open-grind/grindr.rs).

## Configure

1. Clone the repository or [download a release from Forgejo releases](https://git.hloth.dev/hloth/grindr-always-online/releases).
2. Set your credentials

```sh
# Google OAuth not supported
export GRINDR_EMAIL="m@example.com"
export GRINDR_PASSWORD="yourpassword"

# https://geohash.softeng.co/, must be 12 characters
export GRINDR_GEOHASH=""

export GRINDR_DATA_DIR="/var/lib/grindr-always-online"
```

Or put them into .env and load:

```sh
set -a; source .env; set +a
```

Or use systemd's `EnvironmentFile=`:

```ini
[Unit]
Description=Grindr Always Online
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
DynamicUser=yes
WorkingDirectory=/opt/grindr-always-online
EnvironmentFile=/etc/grindr-always-online/.env
Environment="GRINDR_DATA_DIR=/var/lib/grindr-always-online"
ExecStart=/opt/grindr-always-online/grindr-always-online

Restart=always
RestartSec=30

NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/grindr-always-online

[Install]
WantedBy=multi-user.target
```

## Run

```sh
cargo run --release          # loop forever at random intervals
cargo run --release -- once  # fire a single request and exit
```

The login session and device identity are cached in `session.json` and
`device.json` in the data directory, so it only logs in on the first run.

## Licenseч

[MIT](./LICENSE)

## Donate

[hloth.dev/donate](https://hloth.dev/donate)
