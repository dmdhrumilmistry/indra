# Attack-surface enumeration checklist (recon)

sh-recon uses this to populate `entry_points`, `trust_boundaries`, and `dangerous_sinks` in
`codebase-map.json`. The goal is to hand hunters a precise list of where untrusted input enters and
where dangerous operations happen, so they trace source->sink instead of scanning blind.

## 1. Input entry points (sources)
- **HTTP routes / controllers**: every route decorator/registration. Record method, path, handler, and
  whether auth middleware guards it (`auth_required`). Frameworks: Express/Koa, Flask/FastAPI/Django,
  Spring `@RequestMapping`, Rails routes, Gin/Echo, Laravel routes, ASP.NET controllers.
- **GraphQL / gRPC / WebSocket** resolvers and handlers.
- **CLI args, env vars, config files** read at runtime.
- **Message consumers**: queue/topic/cron/webhook handlers.
- **File & upload intake**; deserialization endpoints; template rendering with user data.
- **Inter-service calls** that forward client-controlled data.

## 2. Trust boundaries
- Unauthenticated vs authenticated surface; user vs admin; tenant A vs tenant B (multi-tenancy).
- Client-supplied identifiers used for lookups (IDOR risk).
- Server-to-internal-service and server-to-cloud-metadata paths (SSRF risk).
- Anywhere data crosses from "attacker-influenced" to "trusted" without validation.

## 3. Dangerous sinks (per class - hand these to the matching hunter)
- **sqli**: raw query builders, string-formatted SQL, ORM `.raw()`/`.extra()`.
- **injection**: `exec`/`system`/`spawn`/`eval`, template `render_string`, LDAP filters, NoSQL `$where`.
- **path-traversal / file-upload**: `open`/`readFile`/`sendFile`/`os.path.join` with user paths; upload handlers.
- **ssrf**: outbound HTTP clients (`requests`, `fetch`, `http.get`, `urllib`) with user URLs.
- **xss**: template output without escaping, `innerHTML`, `dangerouslySetInnerHTML`, `v-html`.
- **deserialization**: `pickle`, `yaml.load`, `Marshal`, `ObjectInputStream`, `unserialize`.
- **xxe**: XML parsers with external entities enabled.
- **crypto**: `md5`/`sha1` for passwords, `Math.random` for tokens, hardcoded IV/keys, ECB mode.
- **auth/access-control**: session/JWT verification, permission checks, ownership checks.
- **secrets**: hardcoded keys/passwords/tokens in source and config.

## 4. Framework-specific defaults to note
- Auto-escaping templates (Jinja2/React/Rails) - reduces XSS risk except in raw sinks.
- ORM parameterization defaults - reduces SQLi except in raw APIs.
- Built-in CSRF protection (Django, Rails, Spring Security) - note if disabled.
Record these in `recon.md` so hunters calibrate false positives.
