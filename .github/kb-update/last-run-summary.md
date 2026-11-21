# Last knowledge-base auto-update - summary

Run date: 2026-11-21

## Classes changed

- **`sh-kb-csrf`** (`SKILL.md`): added a "gaps that defeat protected routes" section (login CSRF, SameSite
  limits including the Lax POST window, sibling subdomains and client-side CSRF, forgeable naive
  double-submit, tokens in URLs), Fetch Metadata and CORS-misconfiguration false-positive notes, and
  updated mitigation (signed session-bound double-submit, `__Host-` prefix, `Sec-Fetch-Site`).
- **`sh-kb-open-redirect`** (`SKILL.md`): added related sinks (unvalidated forwards, PHP missing `exit`),
  a stricter validation false-positive filter (parsed components, reject userinfo, redirect to exact
  validated URL), and mitigation preferring ID-to-URL maps and local-URL helpers.

## Sources consulted

- https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html
- https://cheatsheetseries.owasp.org/cheatsheets/Unvalidated_Redirects_and_Forwards_Cheat_Sheet.html

## Prompt injection / low-quality content

None encountered.
