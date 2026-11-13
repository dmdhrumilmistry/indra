# KB update run summary

## Changes
- `sh-kb-csrf/SKILL.md`: added Fetch Metadata / strict Origin check as a valid defense, and a "residual-risk checks" section (SameSite registrable-domain scope, Lax two-minute POST window, login CSRF, client-side CSRF, tokens in URLs).
- `sh-kb-open-redirect/SKILL.md`: added component-based URL validation false-positive filter, and a "Forwards" section (server-side forward as access-control bypass, PHP `header()` without `exit`).

## Sources consulted
- https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html
- https://cheatsheetseries.owasp.org/cheatsheets/Unvalidated_Redirects_and_Forwards_Cheat_Sheet.html

## Prompt injection / low-quality content
None encountered.
