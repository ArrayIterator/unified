# Running Unified

How to run the Unified Pentagonal Project server. Everything below was
verified against the code in this repository (`src/cli/commands.rs`,
`src/core/config.rs`, `src/db/mod.rs`, `config.example.yaml`, `Cargo.toml`).

> Status note (updated 2026-11-22): a Rust toolchain is installed
> (`~/.cargo/bin`, cargo 1.100.0) but is not on `PATH` by default, and
> `cargo check` / `cargo build` have been attempted in the build VM —
> but a full successful check/build has never completed here. The
> 2026-11-22 lean build (`JOBS=1`, `INCREMENTAL=0`, `debuginfo=0`,
> sccache, shared target dir) compiled 321 crates and then failed on a
> real code error — `unified-db-commerce` fails with 785 `E0425`
> errors (`cannot find type ... in module 'affiliates'`,
> `crates/unified-db-commerce/src/models/affiliates.rs`). No migration
> has ever run against a live PostgreSQL, and no endpoint has been
> live-tested. Expect compile errors and report them back; the fixes
> belong in the code, not in this guide.

## 1. Prerequisites

- **Rust 1.85 or newer** (edition 2024; `rust-version = "1.85"` in
  `Cargo.toml`). Install via [rustup](https://rustup.rs/):
  `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **PostgreSQL** server (the project standard is PostgreSQL via Diesel).
  The code also lists `mysql` and `sqlite` drivers in `database_url()`,
  but PostgreSQL is the only driver this project targets.
- **Linux or macOS.** The daemon commands use Unix process/signal
  handling (`nix`); Windows is untested.
- **musl (only for the static release binary):** the
  `x86_64-unknown-linux-musl` target plus `musl-gcc`:
  ```sh
  rustup target add x86_64-unknown-linux-musl
  sudo apt install musl-tools   # Debian/Ubuntu, provides musl-gcc
  ```
  The linker is wired in `.cargo/config.toml` (`linker = "musl-gcc"`).
- Optional: **Redis** (only when `redis.enabled: true` in the config;
  otherwise the filesystem cache in `data/cache` is used).

## 2. Get the code and build it

```sh
git clone https://github.com/ArrayIterator/unified.git
cd unified

# Check first (fast, catches most errors before a full build):
cargo check --workspace

# Then build the release binary (LTO + stripped, see Cargo.toml):
cargo build --release
# Binary: ./target/release/unified
```

Static binary (musl, runs on any x86_64 Linux with no glibc install):

```sh
cargo build --release --target x86_64-unknown-linux-musl
# Binary: ./target/x86_64-unknown-linux-musl/release/unified
```

> musl status: the target, the `musl-gcc` linker (`.cargo/config.toml`)
> and this section are in place, but **a musl build has never been run
> for this tree** (the build VM OOM-kills rustc on the big crates).
> Verify it on a bigger machine or in CI. Known risk: Diesel/Postgres
> links the C library `libpq` (`pq-sys`); the system `libpq` is
> glibc-linked, so if the link step fails on `-lpq`, build `libpq`
> from PostgreSQL source with `musl-gcc` and point `pq-sys` at it
> (`PQ_LIB_DIR` + `PQ_LIB_STATIC=1`).

For a development run without installing, `cargo run -- start` works
(the examples below use the built binary).

## 3. Create the database

```sh
# As the postgres superuser (adjust names to taste):
sudo -u postgres psql -c "CREATE USER unified WITH PASSWORD 'change-me';"
sudo -u postgres psql -c "CREATE DATABASE unified OWNER unified;"
```

No manual migration step: the embedded Diesel migrations
(`crates/unified-core/migrations/`, `2026-11-22-000001`–`2026-11-22-000110`,
111 migrations, embedded in `unified-core` via `embed_migrations!`) run
automatically at server startup whenever a database is configured
(see step 6). If the database is
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

- `cargo check --workspace` is the fast loop; `cargo test` runs the in-module unit
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

### Dashboard scripts are TypeScript

The dashboard JavaScript is written in TypeScript (`assets/js/*.ts`,
strict `tsconfig.json`); there are no `.js` files in the repository.

- Edit a `.ts` file, then type-check it: `npm run check`
  (`tsc --noEmit`; `typescript` is a devDependency — run `npm install`
  once first). The server never runs `tsc`.
- At startup the server transpiles every `.ts` file to JavaScript
  in-memory (OXC, crate `crates/unified-ts` — transpile-only, no JS
  engine) and serves the compiled scripts from RAM under the same
  `/assets/js/<name>.js` URLs; asset fingerprints (`?v=`) come from the
  `.ts` source. A script that fails to transpile is logged at startup
  and skipped (its URL answers 404); the rest still serve.
- Restart the server to pick up script changes (the compiled scripts
  are built once at startup, not watched).
- `cargo test -p unified-ts` runs the transpiler unit tests, including
  transpiling all six dashboard scripts.

### Faster local builds (optional)

- **sccache** (compiler cache, local disk `~/.cache/sccache`, max
  10 GiB). Installed from the Mozilla release tarball — the
  `cargo install sccache` route fails in this VM because `/tmp` is a
  512 MB tmpfs and the `aws-lc-sys` build overflows it (workaround:
  `TMPDIR=$HOME/.cache/tmp cargo install sccache`). Use it with:
  ```sh
  export RUSTC_WRAPPER=sccache
  ```
  sccache cannot cache incremental compilations, so dev-profile builds
  need `CARGO_INCREMENTAL=0` (`cargo check` / `cargo build`); release
  builds already have incremental off. Verify with
  `sccache --show-stats` (cache hits grow on rebuilds). The cache
  survives across runs, so repeat `cargo check` runs only recompile
  changed crates.
- **mold** (fast linker) is wired for this VM only, in
  `~/.cargo/config.toml` (`-C link-arg=-fuse-ld=mold`); it does not
  affect `cargo check` (no linking happens) and is not part of the repo.
- **Disk hygiene**: `cargo sweep --time 30` (run from the project root)
  prunes build artifacts older than 30 days. Note it can only see
  artifacts recorded in `target/debug/.fingerprint`; after interrupted
  `cargo check` runs the target dir can also hold stale
  `debug/incremental/<crate>-*` sessions — keeping only the newest
  session per crate is safe (they are pure cache; the next check just
  recompiles the dropped ones).
- **Lean build on a low-memory VM** (7.7 GiB RAM; the plain dev build
  OOM-kills rustc, which peaks around 5 GiB with debuginfo on). Recipe
  attempted 2026-11-22 (max single `rustc` RSS observed ~3.4 GiB —
  that figure covers all `rustc` processes on the machine, other
  agents' checks included — and available memory fell as low as
  229 MiB while they ran alongside):
  ```sh
  export RUSTC_WRAPPER=sccache
  export CARGO_BUILD_JOBS=1 CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0
  export RUSTFLAGS="-C debuginfo=0"
  export CARGO_TARGET_DIR=~/workspace/unified-target TMPDIR=$HOME/.cache/tmp
  cargo build --offline
  ```
  Notes: an 8 GiB swapfile was tried as a backstop but this container
  cannot enable new swap (`swapon` fails with `Operation not
  permitted`), so the build runs with only the pre-existing 1 GiB
  swap. `cargo build` never runs tests (`cargo test` is separate).
  The 2026-11-22 attempt did not finish: it failed on the
  `unified-db-commerce` E0425 errors (see the status note above), and
  the retry run was terminated by the runtime (SIGTERM) mid-build
  while other agents' per-crate checks kept the machine busy. Run 3
  (2026-11-23, detached via `setsid`, same lean flags, GNU ld
  fallback because mold was not installed at launch — it was
  reinstalled mid-run) got past all db crates and failed in the root
  `unified` bin itself: 29 pre-existing compile errors in 14 files
  (network_tools 9, delivery_notes 5, auth_manager 5, social_marketing
  2, billing 2, referral 2, forums 2, dashboards reseller 1, mod.rs 1;
  E0425/E0432/E0252/E0603 — e.g. private `hash_value` import, missing
  `unified_scripting::register` re-export). Both attempts hit the
  identical 29 errors; peak single `rustc` RSS was only ~2.0 GiB and
  no OOM kill occurred, so RAM is not the blocker — the code is. The
  full binary has still never been produced in this VM; re-run after
  the root-bin fix lands (log
  `~/workspace/unified-check/lean-build-20261123-run3.log`).
  Run 4 (2026-11-23, detached via `setsid`, same lean flags plus
  `RUSTFLAGS="-C debuginfo=0 -C link-arg=-fuse-ld=mold"` — mold
  2.30.0 was installed again, so the mold link-arg was named
  explicitly because env RUSTFLAGS replaces the config rustflags):
  the root-bin fix agent had fixed all 51 diagnosed errors (29
  original + 22 layer-2), and the build got past every dependency and
  was killed while compiling the `unified` bin crate itself — exit
  137 after 1069 s, `cargo build` SIGKILLed by the OOM killer, **zero**
  error diagnostics in the log. Peak single `rustc` RSS 5,878,356 KB
  (~5.7 GiB), minimum available memory 209 MiB. So the bin crate
  alone needs ~5.7 GiB RSS to type-check/link, which does not fit
  this 7.7 GiB VM alongside the OS and sccache — the blocker is RAM
  again, not code. No binary produced; no re-run was attempted (the
  OOM signature was detected, and a retry would hit the same wall).
  The full binary still needs a bigger machine / CI (≥12 GiB) or the
  Colab route (log
  `~/workspace/unified-check/lean-build-20261123-run4.log`).

## 12. Verification

One command runs the full verification suite:

```sh
bash scripts/check.sh
# Also check the Pingora-gated gateway build:
bash scripts/check.sh --features gateway
```

The suite runs, in order:

1. `cargo fmt --check` — formatting must match `rustfmt`; fails on
   any unformatted file.
2. `cargo clippy --all-targets -- -D warnings` — lints all targets;
   any warning fails the run.
3. `cargo check --all-targets` — type-checks all targets (bins,
   tests) without producing a binary.
4. `cargo test` — runs the in-module unit tests.

The script stops at the first failure (`set -euo pipefail`). With
`--features gateway`, clippy and check run a second time with the
gateway feature enabled, so the Pingora-gated code is checked too.

CI runs the same suite on every push and pull request
(`.github/workflows/check.yml`): one job for the default build (all
four steps) and a second job for `--features gateway` (clippy +
check). The workflow uses Rust 1.85, matching `rust-version` in
`Cargo.toml`.
