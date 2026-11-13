# Severity & CVSS rubric

Severity communicates **business impact if exploited**, independent of how easy it was to find.
Assign `severity` on every finding; the verifier additionally computes a CVSS v3.1 vector/score.

## Severity bands

| Severity | Meaning | Typical examples |
|---|---|---|
| **critical** | Unauthenticated or trivially-authenticated path to full compromise, or mass data exposure. | Pre-auth RCE, SQLi dumping the whole DB, auth bypass, SSRF to cloud metadata -> credential theft, insecure deserialization RCE. |
| **high** | Serious impact but needs some precondition (auth, specific role, user interaction). | IDOR exposing other users' data, stored XSS in an authenticated app, path traversal reading arbitrary files, privilege escalation. |
| **medium** | Real but limited impact, or requires significant preconditions. | Reflected XSS needing a crafted link, CSRF on a meaningful action, weak crypto for non-critical data, open redirect. |
| **low** | Minor exposure or defense-in-depth gap with narrow impact. | Verbose errors, missing security headers with no direct exploit, low-entropy tokens with mitigations. |
| **info** | No direct exploit; hardening or hygiene. | Outdated-but-unreachable dependency, informational disclosure of framework version. |

## Chaining bumps severity

A chain's severity is the **combined** impact, usually one band above its strongest member (see chains.md).
E.g. a medium open-redirect + a low token-leak that together yield account takeover -> the CHAIN is critical.

## CVSS v3.1 (verifier)

Compute a vector and score. Anchor the common metrics:
- `AV` Network (N) for remote HTTP-reachable; Local (L) for CLI/file-based.
- `PR` None (N) for pre-auth; Low (L) for any-authenticated-user; High (H) for admin-only.
- `UI` None (N) for server-side (SQLi/SSRF); Required (R) for XSS/CSRF/open-redirect needing a click.
- `S` Changed (C) when the vuln crosses a trust boundary (SSRF hitting internal services, XSS stealing another origin's data).
- `C/I/A` High (H) for full read/write/DoS; Low/None otherwise.

Report both `cvss.score` (numeric) and `cvss.vector`. If exploitability is only statically inferred
(status `needs-runtime`), keep the CVSS but note the assumption in `verification.rationale`.

## Confidence (0-100) vs severity

Confidence = how sure we are the vuln is **real and reachable**; severity = how bad it is **if real**.
They are independent. A confirmed low is `confidence:95, severity:low`; a speculative critical is
`confidence:40, severity:critical, status:needs-runtime`. Never inflate confidence to match severity.
