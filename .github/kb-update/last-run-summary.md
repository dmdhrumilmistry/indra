# Last knowledge-base auto-update - summary

Run date: 2026-11-27

## Classes changed

- **`sh-kb-csrf`** (`SKILL.md`): added SameSite=Lax gaps (state-changing GET, method override, ~2-minute
  Lax-by-default window), naive double-submit cookie-injection bypass, and client-side CSRF to the payload
  list. Tightened false-positive filters (exact-origin CORS, `Sec-Fetch-Site`/Origin checks, CAPTCHA is not a
  control). Mitigation now recommends signed double-submit, `__Host-` cookie prefix, Fetch Metadata, and
  session re-creation at login.
- **`sh-kb-open-redirect`** (`SKILL.md`): added detection checks (redirecting to raw instead of validated
  input, PHP code continuing after `header("Location")`), the ASP.NET Core local-redirect helper as a
  false-positive filter, and mitigation on opaque-id mapping, parsed-component comparison, rejecting
  userinfo, and authorization checks for forwards.

## Sources consulted

- https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html
- https://cheatsheetseries.owasp.org/cheatsheets/Unvalidated_Redirects_and_Forwards_Cheat_Sheet.html

## Prompt injection / low-quality content

None encountered.
