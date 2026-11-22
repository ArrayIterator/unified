# Running Unified

How to run the Unified Pentagonal Project server. Everything below was
verified against the code in this repository (`src/cli/commands.rs`,
`src/core/config.rs`, `src/db/mod.rs`, `config.example.yaml`, `Cargo.toml`).

> Status note: at the time of writing, this tree has never been compiled
> (`cargo check` / `cargo build` never ran here — no Rust toolchain in the
> build environment), no migration has ever run against a live PostgreSQL,
> and no endpoint has been live-tested. Expect first-run compile errors
> and report them back; the fixes belong in the code, not in this guide.

## 1. Prerequisites

- **Rust 1.85 or newer** (edition 2024; `rust-version = "1.85"` in
  `Cargo.toml`). Install via [rustup](https://rustup.rs/):
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **PostgreSQL** server (the project standard is PostgreSQL via Diesel).
  The code also lists `mysql` and `sqlite` drivers in `database_url()`,
  but PostgreSQL is the only driver this project targets.
- **Linux or macOS.** The daemon commands use Unix process/signal
  handling (`nix`); Windows is untested.
- Optional: **Redis** (only when `redis.enabled: true` in the config;
  otherwise the filesystem cache in `data/cache` is used).

## 2. Get the code and build it

```sh
git clone https://github.com/ArrayIterator/unified.git
cd unified

# Check first (fast, catches most errors before a full build):
cargo check

# Then build the release binary (LTO + stripped, see Cargo.toml):
cargo build --release
# Binary: ./target/release/unified
```

For a development run without installing, `cargo run -- start` works
(the examples below use the built binary).

## 3. Create the database

```sh
# As the postgres superuser (adjust names to taste):
sudo -u postgres psql -c "CREATE USER unified WITH PASSWORD 'change-me';"
sudo -u postgres psql -c "CREATE DATABASE unified OWNER unified;"
```

No manual migration step: the embedded Diesel migrations
(`migrations/000001`–`000107`) run automatically at server startup
whenever a database is configured (see step 6). If the database is
unreachable, the server still starts, but every database-backed
feature stays unavailable — check the startup log for
"Database connected and migrations applied."

## 4. Configure

```sh
# Writes config.example.yaml to config.yaml (does nothing when the
# file already exists):
./target/release/unified init
```

Or copy it yourself: `cp config.example.yaml config.yaml`. Then edit
at least these sections:

```yaml
database:
  host: "localhost"
  port: 5432
  username: "unified"
  password: "change-me"
  database: "unified"

app:
  # Production rules enforced by validation: secret_key must be at
  # least 32 characters, salt_key at least 16, and they must differ.
  secret_key: "change-this-to-a-random-secret-string"
  salt_key: "change-this-to-a-random-salt-string"
  tcp:
    - "127.0.0.1:8080"   # HTTP listen address (default)
  ssl:
    listen:
      - "127.0.0.1:8443" # TLS listen address (default)
```

Config file resolution: `-f` / `--config <path>`, else the
`$UNIFIED_CONFIG_FILE` environment variable, else `config.yaml` in the
working directory. All values are optional — missing values fall back
to built-in defaults (see `config.example.yaml`).

## 5. Test the configuration (before starting)

```sh
./target/release/unified -t
# With an explicit file:
./target/release/unified -t -f config.yaml
```

Exit 0 means valid; any problem is listed and the exit code is 1.
`reload` and `start` refuse to run with an invalid configuration.

## 6. Start the server

Foreground (logs on stdout, Ctrl-C stops it):

```sh
./target/release/unified start
```

As a background daemon:

```sh
./target/release/unified start --daemonize
```

On startup the server registers all 137 modules, runs the pending
migrations, and listens on the configured addresses (defaults
`127.0.0.1:8080` HTTP and `127.0.0.1:8443` TLS).

## 7. Open it

- User dashboard: <http://127.0.0.1:8080/dashboard>
- Admin dashboard: <http://127.0.0.1:8080/admin>
- Login / register: <http://127.0.0.1:8080/login>,
  <http://127.0.0.1:8080/register>

Route paths are configurable in the `routes:` section of `config.yaml`
(login, register, dashboard, admin, status page); the previous path
keeps working as a 301 redirect to the new one.

**There is no default admin account seeded in the code.** Create the
first account on the register page. Roles and capabilities are managed
through the Role Manager (`/admin`).

## 8. Daemon commands

```sh
./target/release/unified status       # show daemon status
./target/release/unified stop         # stop the running daemon
./target/release/unified restart      # restart the daemon
./target/release/unified reload       # validate config, then reload (refused when invalid)
./target/release/unified pause        # pause the running daemon
./target/release/unified resume       # resume a paused daemon
./target/release/unified maintenance  # show maintenance state
./target/release/unified maintenance on   # enable maintenance mode
./target/release/unified maintenance off  # disable maintenance mode
./target/release/unified monitor      # open the live monitor dashboard (ratatui TUI)
./target/release/unified --monitor    # same as the monitor subcommand
```

`server <start|stop|reload|restart|status>` aliases exist for the same
commands (e.g. `unified server status`). Modules may inject extra
subcommands at runtime; a name that conflicts with a built-in command
is ignored with a warning.

## 9. Optional features

- **Pingora gateway proxy** (reverse proxy in front of the app). Build
  with the feature and enable it in config:
  ```sh
  cargo build --release --features gateway
  ```
  ```yaml
  gateway:
    enabled: true
    listen:
      - "127.0.0.1:8081"
  ```
  With the feature off, the binary links no Pingora code. The `0.4`
  pin in `Cargo.toml` is unverified against the live Pingora API — see
  the comment in `Cargo.toml` before enabling it in a release build.
- **Redis cache**: set `redis.enabled: true` and the connection fields
  in `config.yaml`. `cache.failover_enabled: true` (default) falls back
  to the filesystem cache when Redis fails at runtime.
- **ACME / Let's Encrypt**: set `acme.enabled: true` and the
  `directory_url` / `contacts`. Keep `/.well-known/acme-challenge/`
  reachable (it is exempt from rate limiting by default).
- **GeoIP**: place a MaxMind database at `data/geo/GeoLite2-City.mmdb`
  or set `licenses.maxmind` so it can be downloaded; settings-defined
  IP overrides win over the database by default
  (`geo.settings_override: true`).

## 10. Troubleshooting

| Symptom | Check |
|---|---|
| `Unified daemon is already running (PID …)` | `unified stop`, then `unified start` again. |
| `Cannot load configuration …` | File missing? Run `unified init` first. Wrong path? Use `-f <path>` or `$UNIFIED_CONFIG_FILE`. |
| `Configuration … is invalid:` | Run `unified -t` — it lists every problem. Common: `secret_key` too short in production, `salt_key` equal to `secret_key`. |
| Server starts but DB features are missing | Startup log says the database could not be reached. Check `database:` in `config.yaml` and that PostgreSQL accepts the user/password. Migrations only run when a connection exists. |
| Port already in use | Change `app.tcp` (and `app.ssl.listen`) in `config.yaml`, then `unified reload`. |
| `unified reload` refused | Config invalid — fix it first (`unified -t` lists the problems). Reload never applies a broken config. |

## 11. Development notes

- `cargo check` is the fast loop; `cargo test` runs the in-module unit
  tests (e.g. Luhn vectors in `cards`, reference-ID vectors in
  `core/reference_id`).
- The API contract: success responses are only `{"data": ...}`;
  errors are only `{"error": {"message", "code", "metadata"}}` plus an
  optional `trace` in debug/development mode.
- Multi-tenant data lives in one PostgreSQL schema per tenant
  (`tenant_<uuidv7 hex>`), resolved from the subdomain; the control
  plane (tenants, subscriptions, domains, billing, licensing) lives in
  the `public` schema. Destroying a tenant is `DROP SCHEMA … CASCADE`
  after an export — see `src/modules/tenant_destroy.rs`.
