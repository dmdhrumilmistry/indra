# Users and roles

Out of the box the console is protected by one access token: `indra serve` prints a URL with it,
and whoever has that URL can do everything. That suits one person on their own machine. For a
team, give each person an account with a role.

## Turn it on

Add the first user. It is always an admin:

```bash
indra user add alice              # asks for the password twice (10+ characters)
```

or, in the console, **Settings > Users > Add the first user**. From then on:

- the console asks people to **sign in** with a name and password;
- `indra serve` prints a plain URL (no token in it);
- the access token no longer signs anyone in, from a URL or a cookie. It stays an admin key for
  tools on this machine (`indra attach`, `indra agents`, scripts) as `Authorization: Bearer`.

## Roles

| Role | Can |
|---|---|
| **viewer** | read everything except settings, secrets and the terminal: agents, runs, findings, workflows, the board, knowledge, logs. The console shows them read-only. |
| **operator** | everything a viewer can, and start, steer, approve, stop and pivot agents; run workflows (a PR review, a security review, your own); triage findings, record them into memory, publish and post reviews; run the board; pull models. |
| **admin** | everything, including settings (`indra.toml`), secrets, users, the browser terminal, editing agent prompts, installing from marketplaces, enrolling machines and the maintainer tools. |

The server checks the role on every API call; hiding a button in the page is only a
convenience. An operator who tries an admin action gets `403` and a message saying which role
it needs.

## Managing users

| In the console (admins) | On the machine |
|---|---|
| **Settings > Users**: add, change a role, set a password, remove | `indra user add NAME --role viewer\|operator\|admin` |
| | `indra user list` |
| | `indra user passwd NAME` |
| | `indra user role NAME ROLE` |
| | `indra user rm NAME` |

Anyone signed in can change their own password from the header bar (**Password**; the current one
is asked again). Setting a password or removing a user ends their sessions at once; a role change
applies to their open sessions immediately. The last admin cannot be removed or demoted.

`indra user` works on the data folder directly, so it also recovers a console whose only admin
forgot their password.

## How it is kept safe

- **Passwords** are stored as Argon2id hashes in `users.json` in the data folder, readable by the
  owner only. They are never logged.
- **Sessions** are random 256-bit tokens in an `HttpOnly`, `SameSite=Strict` cookie (`Secure`
  when the console is reached over HTTPS, directly or behind a proxy that sets
  `X-Forwarded-Proto`). A session ends after 8 hours without use, after 7 days in all, on sign
  out, or when indra restarts.
- **Guessing** is slowed: five wrong passwords for a name lock it for five minutes, and an
  unknown name takes as long to refuse as a known one.
- **Every change is logged**: each state-changing request writes a line to
  `logs/audit.jsonl` in the data folder with the time, user, role, method, path and result.
- Everything else in the [security model](../../tools/indra/README.md#security-model) still
  applies: loopback by default, no DNS rebinding, same-origin JSON only, the terminal opt-in.

To reach a console from other machines, put it behind HTTPS (a reverse proxy) and turn users on;
`--allow-remote-config` is still needed before anyone can edit settings over the network.
