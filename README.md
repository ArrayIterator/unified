# Unified Pentagonal Project

**High-performance unified management system.**

One control plane for licensing, downloads, storefront, software and asset
management, and the day-to-day operations around them. One login, one
dashboard, many modules.

> Project status: initial scaffolding. This repository currently holds the
> project documentation and task list. The implementation milestones are
> tracked in [TODO.md](TODO.md).

## What it is

Unified Pentagonal Project is a CRM-like unified management system for:

- **Licensing management** — issue, validate, suspend, and revoke licenses
  for themes, plugins, and software.
- **File downloader** — secure, resumable downloads for licensed files and
  assets.
- **Storefront** — sell themes, plugins, and software, with software and
  asset management behind it.
- **Unified operations** — members and customers, projects and tasks,
  servers and sites, billing, support tickets, and content, in one place.

## Core modules

Core modules ship inside the binary and cannot be removed:

1. **Service Monitor & Management** — health of sites, services, and
   servers; server information; reload with dry-run support.
2. **Security Management** — security checks, policies, and audit trails.
3. **User Management** — users, roles, and permissions.
4. **Modules / Extensions Manager** — list, enable, disable, and configure
   extensions. Extensions are fully integrated and can interconnect.
5. **Content Management** — posts, categories, and pages, with a site
   builder.
6. **Settings** — site-wide settings (details are defined per setting).
7. **SEO Manager** — SEO management for sites and content.
8. **API Manager** — create and manage public and client APIs with rules,
   versioning, and a registry of registered endpoints. This is separate
   from the core API.
9. **Auth Manager** — authentication with submodules: SSO, OAuth server,
   OAuth login, SSO as provider, social login (for example Google and
   Facebook), biometric login, and a JSON Web Token manager. Submodules can
   inject their own settings into the Auth Manager.
10. **Translations / i18n Manager** — dynamic translations and translation
    management.

**Theme Management** is also a core module: add themes through a templating
engine, embed themes internally, or load them dynamically, with strict
checking and screenshot support.

## Stack

- **Backend:** Rust, Actix-web, PostgreSQL (Diesel by default).
- **Frontend:** React (SEO friendly) with a modern Tailwind dashboard UI,
  easy navigation, and Dark / Light mode support.
- **Themes:** Handlebars (HBS) with dynamic loading.
- **Icons:** IcoMoon icon sets, for example Entypo and Material Icons.
- **TLS:** Let's Encrypt ACME, configurable from the dashboard, with SSL
  swapping from the dashboard or CLI, including custom SSL with validation
  and renewal.
- **Listeners:** Unix socket and TCP port listeners, HTTP and HTTPS
  (ports 80 / 443, with validation).
- **Realtime:** WebSocket support where needed.

## API standard

All JSON APIs follow the same shape and standard HTTP status codes.

Success responses contain only the `data` key:

```json
{
  "data": "..."
}
```

Error responses contain only the `error` key:

```json
{
  "error": {
    "message": "...",
    "code": 404,
    "metadata": {}
  }
}
```

The `error` object has exactly three keys: `message`, `code`, and
`metadata`. In debug / development mode only, an optional `trace` key is
added.

## Security

- Passwords are hashed with **Argon2**.
- Sessions are **stateless**, using a signed session tokenizer.
- **TOTP** two-factor authentication.
- **CSRF** protection on state-changing requests.
- **Session flash** messages kept in memory.

## License

Proprietary. See [LICENSE](LICENSE). This repository is private and for the
project team only.
