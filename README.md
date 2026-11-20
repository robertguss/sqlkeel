# sqlkeel

**Blazing-fast, agent-first (DX + AX) Rust SQL semantic bug, schema-grounding, and Postgres migration safety analyzer.**

Sibling to [`datakeel`](https://github.com/robertguss/datakeel): **`datakeel` guards your data; `sqlkeel` guards your SQL queries and migrations.**

## Why `sqlkeel`?

Existing SQL tools split formatting (`sqruff` / `sqlfluff`) from DDL lock linting (`squawk`), and neither reads your **ODCS v3 / `datakeel` contracts** to catch AI-hallucinated columns or silent analytics bugs.

`sqlkeel` brings all three into a single millisecond Rust binary built for **both humans (DX) and AI coding agents (AX)**:

- **Silent analytics bug detection:**
  - `SK_LEFT_JOIN_WHERE_FILTER` — catches `WHERE` filters on `LEFT JOIN`ed tables that silently drop unmatched rows (while allowing safe anti-joins like `WHERE c.id IS NULL` or `OR c.id IS NULL`).
  - `SK_NOT_IN_SUBQUERY` — catches `NOT IN (SELECT ...)` null traps (and automatically knows when a column is proven `NOT NULL` from your contract/DDL!).
  - `SK_NULL_COMPARISON`, `SK_UNUSED_CTE`, `SK_SELECT_STAR`, `SK_CROSS_JOIN`.
- **Schema-grounded hallucination detection (`--schema`):**
  - Loads **ODCS v3 YAML**, **`datakeel.toml`**, and **SQL DDL** files (or entire directories) in one flag.
  - Catches `SK_UNKNOWN_TABLE` and `SK_UNKNOWN_COLUMN` statically without a live DB, with **fuzzy Levenshtein "did you mean?" suggestions** (`s.gpa_score` → `s.gpa`).
- **PostgreSQL zero-downtime migration & DML safety:**
  - `SK_PG_CREATE_INDEX_LOCK`, `SK_PG_DROP_INDEX_LOCK`, `SK_PG_ADD_COLUMN_NOT_NULL`, `SK_PG_ALTER_COLUMN_TYPE`, `SK_PG_ADD_CONSTRAINT_LOCK`, `SK_PG_DESTRUCTIVE_DDL`, `SK_DELETE_WITHOUT_WHERE`, `SK_UPDATE_WITHOUT_WHERE`.
- **First-class AX (Agent Experience):**
  - TTY-aware dual mode (`auto`/`human`/`agent`/`json`), strict `stdout`/`stderr` split, exact `file:line:column` spans, actionable `fix_hint`s, exit codes `0`/`1`/`2`, and bundled `SKILL.md` (`sqlkeel skill --install .agents/skills/sqlkeel`).

## Installation

```bash
# Rust / Cargo
cargo install --git https://github.com/robertguss/sqlkeel

# Python / uv (once published to PyPI)
uvx sqlkeel --help
uv tool install sqlkeel
```

## Quickstart

```bash
# 1. Check SQL queries grounded against datakeel ODCS/TOML contracts
sqlkeel check examples/seminary-sql/queries/ \
  --schema examples/seminary-sql/contracts/

# 2. Check Postgres migrations strictly (fail on warnings too)
sqlkeel check examples/seminary-sql/migrations/ --fail-on warning

# 3. Inspect all known tables & columns in one compact payload
sqlkeel schema examples/seminary-sql/contracts/

# 4. Install the built-in AI Agent Skill into your repo
sqlkeel skill --install .agents/skills/sqlkeel
```

## Exit Codes

| Code | Meaning |
| :--- | :--- |
| `0` | Clean pass |
| `1` | One or more SQL findings at or above `--fail-on` |
| `2` | CLI usage, IO, unknown rule, or schema parse error |

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE).
