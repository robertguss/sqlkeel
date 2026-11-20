use crate::error::{Result, SqlKeelError};
use std::fs;
use std::path::{Path, PathBuf};

pub const SKILL_MD: &str = r#"---
name: sqlkeel
description: Statically verify SQL queries, views, analytics models, and Postgres migrations for silent semantic bugs (LEFT JOIN+WHERE traps, NOT IN nulls, = NULL), schema-grounded hallucinated tables/columns (using ODCS v3 YAML, datakeel TOML, or DDL), and zero-downtime Postgres migration locks. Run after editing any `.sql` file before executing against a database.
---

# SqlKeel Agent Manual

## Identity & Scope

`sqlkeel` is a single-binary Rust static analyzer for SQL and PostgreSQL. It pairs directly with `datakeel` by reading **ODCS v3 YAML**, **`datakeel.toml`**, and **SQL DDL** files to catch hallucinated tables/columns, silent analytics bugs, destructive DML, and table-locking Postgres migrations in milliseconds—without needing a live database connection.

## Output & Exit Code Contract

- **TTY-aware dual mode:** Automatically outputs compact single-line JSON (`--format agent`) when piped/non-TTY and human-readable output in a TTY. Pass `--format agent` or `--format json` explicitly if needed.
- **Strict stream separation:** `stdout` contains ONLY the structured report. `stderr` receives runtime/config errors (`ErrorEnvelope` JSON when non-TTY).
- **Zero interactivity:** Never prompts for input.
- **Exit codes:**
  - `0` — Clean pass (no findings at or above `--fail-on`)
  - `1` — One or more SQL bugs, schema hallucinations, or migration hazards found
  - `2` — CLI usage, IO, unknown rule, or schema parse error

## Core Command Matrix

| Goal | Command |
| :--- | :--- |
| Check a SQL folder grounded in `datakeel` contracts | `sqlkeel check sql/ --schema contracts/ --format agent` |
| Check a migration file strictly (fail on warnings too) | `sqlkeel check migrations/004_idx.sql --fail-on warning --format agent` |
| Check a query piped over stdin against DDL/contracts | `echo "SELECT ..." \| sqlkeel check - --schema contracts/ --format agent` |
| Inspect all known tables & columns before writing SQL | `sqlkeel schema contracts/ migrations/001_init.sql --format agent` |
| List every built-in `SK_*` rule code | `sqlkeel rules --format agent` |
| Suppress a single intentional rule inline | `-- sqlkeel:ignore SK_SELECT_STAR` (same line or line above) |

## Agent Decision Tree for SQL & Migrations

1. **Before writing new SQL views or risk-scoring queries:**
   - Run `sqlkeel schema contracts/ --format agent` to load exact table names, column names, types, and nullability in one compact payload.
2. **After writing or modifying any `.sql` file:**
   - Run `sqlkeel check <path> --schema contracts/ --format agent`.
   - If exit code is `1`, inspect each diagnostic's `code`, `file:line:column`, `message`, `fix_hint`, and `suggestion`:
     - `SK_LEFT_JOIN_WHERE_FILTER`: Move the right-side filter into `LEFT JOIN ... ON`, or guard with `OR <alias>.<col> IS NULL`.
     - `SK_NOT_IN_SUBQUERY`: Rewrite to `NOT EXISTS (...)` or add `WHERE <col> IS NOT NULL`.
     - `SK_UNKNOWN_TABLE` / `SK_UNKNOWN_COLUMN`: Apply the fuzzy `suggestion` or verify against `sqlkeel schema`.
     - `SK_PG_CREATE_INDEX_LOCK` / `SK_PG_ADD_COLUMN_NOT_NULL`: Apply zero-downtime Postgres migration patterns.
   - Re-run until exit code `0`.

## Gotchas & Anti-Patterns

- **Always pass `--schema`** when contracts or DDL exist in the repo—without `--schema`, `SK_UNKNOWN_TABLE` and `SK_UNKNOWN_COLUMN` are skipped, and `SK_NOT_IN_SUBQUERY` cannot verify non-null columns automatically.
- **Do not** suppress `SK_LEFT_JOIN_WHERE_FILTER` or `SK_NOT_IN_SUBQUERY` unless you have mathematically verified the query cannot silently drop rows.
- Use `--dialect snowflake|bigquery|sqlite|duckdb|generic` if analyzing non-Postgres SQL.
"#;

pub fn install_skill(target: &Path) -> Result<PathBuf> {
    let dest = if target
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("md"))
    {
        target.to_path_buf()
    } else {
        target.join("SKILL.md")
    };

    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| SqlKeelError::IoWrite {
            path: dest.clone(),
            source,
        })?;
    }

    fs::write(&dest, SKILL_MD).map_err(|source| SqlKeelError::IoWrite {
        path: dest.clone(),
        source,
    })?;

    Ok(dest)
}
