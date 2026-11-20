use crate::cli::DialectArg;
use crate::error::{Result, SqlKeelError};
use ignore::WalkBuilder;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sqlparser::ast::{ColumnOption, Statement};
use sqlparser::dialect::{
    BigQueryDialect, Dialect, DuckDbDialect, GenericDialect, PostgreSqlDialect, SQLiteDialect,
    SnowflakeDialect,
};
use sqlparser::parser::Parser;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_name: Option<String>,
    pub source: String,
    pub columns: IndexMap<String, ColumnSchema>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SchemaCatalog {
    /// Keyed by lowercased unqualified table name (and optionally `schema.table`).
    pub tables: IndexMap<String, TableSchema>,
    /// Lookup map from lowercased `schema.table` or `table` -> canonical key in `tables`.
    #[serde(skip)]
    lookup: IndexMap<String, String>,
}

impl SchemaCatalog {
    pub fn is_empty(&self) -> bool {
        self.tables.is_empty()
    }

    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    pub fn insert_table(&mut self, table: TableSchema) {
        let bare = table.name.to_ascii_lowercase();
        let canonical = match &table.schema_name {
            Some(s) if !s.is_empty() => format!("{}.{}", s.to_ascii_lowercase(), bare),
            _ => bare.clone(),
        };
        self.lookup.insert(canonical.clone(), canonical.clone());
        // Always register bare name too so `FROM populi_students` resolves `staging.populi_students`
        self.lookup.entry(bare).or_insert_with(|| canonical.clone());
        self.tables.insert(canonical, table);
    }

    pub fn resolve_table(&self, raw_name: &str) -> Option<&TableSchema> {
        let clean = normalize_table_ident(raw_name);
        if let Some(key) = self.lookup.get(&clean) {
            return self.tables.get(key);
        }
        // Try bare table name after last dot (e.g. `public.users` -> `users`)
        if let Some((_, bare)) = clean.rsplit_once('.')
            && let Some(key) = self.lookup.get(bare)
        {
            return self.tables.get(key);
        }
        None
    }

    pub fn suggest_table(&self, target: &str) -> Option<String> {
        let needle = normalize_table_ident(target);
        let needle_bare = needle.rsplit_once('.').map_or(needle.as_str(), |(_, b)| b);
        let candidates: Vec<&str> = self
            .tables
            .values()
            .map(|t| t.name.as_str())
            .collect();
        suggest_closest(needle_bare, &candidates)
    }

    pub fn suggest_column(table: &TableSchema, col: &str) -> Option<String> {
        let needle = col.to_ascii_lowercase();
        let candidates: Vec<&str> = table.columns.keys().map(String::as_str).collect();
        suggest_closest(&needle, &candidates)
    }

    pub fn load_from_paths(paths: &[PathBuf], dialect: DialectArg) -> Result<Self> {
        let mut catalog = Self::default();
        let mut files: Vec<PathBuf> = Vec::new();

        for p in paths {
            if p.is_dir() {
                for entry in WalkBuilder::new(p).standard_filters(true).build().flatten() {
                    let ep = entry.path();
                    if ep.is_file() && is_schema_file(ep) {
                        files.push(ep.to_path_buf());
                    }
                }
            } else if p.is_file() {
                files.push(p.clone());
            } else {
                return Err(SqlKeelError::IoRead {
                    path: p.clone(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "schema path does not exist",
                    ),
                });
            }
        }

        files.sort();
        for f in files {
            catalog.load_file(&f, dialect)?;
        }

        Ok(catalog)
    }

    fn load_file(&mut self, path: &Path, dialect: DialectArg) -> Result<()> {
        let content = fs::read_to_string(path).map_err(|source| SqlKeelError::IoRead {
            path: path.to_path_buf(),
            source,
        })?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let src = path.display().to_string();

        match ext.as_str() {
            "yaml" | "yml" => self.load_odcs_yaml(&content, &src),
            "toml" => self.load_datakeel_toml(&content, &src),
            "sql" => self.load_sql_ddl(&content, &src, dialect),
            _ => Err(SqlKeelError::SchemaParse {
                path: src,
                detail: format!("unsupported schema file extension '.{ext}'"),
            }),
        }
    }

    fn load_odcs_yaml(&mut self, content: &str, src: &str) -> Result<()> {
        let doc: OdcsDoc = serde_yaml_ng::from_str(content).map_err(|e| {
            SqlKeelError::SchemaParse {
                path: src.to_string(),
                detail: e.to_string(),
            }
        })?;
        for obj in doc.schema {
            let raw_name = obj.physical_name.unwrap_or(obj.name);
            let (schema_name, table_name) = split_schema_table(&raw_name);
            let mut columns = IndexMap::new();
            for prop in obj.properties {
                let col_key = prop.name.to_ascii_lowercase();
                let nullable = !(prop.required || prop.primary_key);
                columns.insert(
                    col_key,
                    ColumnSchema {
                        name: prop.name,
                        data_type: prop.physical_type.unwrap_or(prop.logical_type),
                        nullable,
                        primary_key: prop.primary_key,
                    },
                );
            }
            self.insert_table(TableSchema {
                name: table_name,
                schema_name,
                source: src.to_string(),
                columns,
            });
        }
        Ok(())
    }

    fn load_datakeel_toml(&mut self, content: &str, src: &str) -> Result<()> {
        let doc: TomlContractDoc =
            toml::from_str(content).map_err(|e| SqlKeelError::SchemaParse {
                path: src.to_string(),
                detail: e.to_string(),
            })?;
        let raw_name = doc.contract.table.unwrap_or(doc.contract.name);
        let (schema_name, table_name) = split_schema_table(&raw_name);
        let mut columns = IndexMap::new();
        for (col_name, col) in doc.columns {
            let col_key = col_name.to_ascii_lowercase();
            let nullable = !(col.required || col.primary_key);
            columns.insert(
                col_key,
                ColumnSchema {
                    name: col_name,
                    data_type: col.physical_type.unwrap_or(col.logical_type),
                    nullable,
                    primary_key: col.primary_key,
                },
            );
        }
        self.insert_table(TableSchema {
            name: table_name,
            schema_name,
            source: src.to_string(),
            columns,
        });
        Ok(())
    }

    fn load_sql_ddl(&mut self, content: &str, src: &str, dialect_arg: DialectArg) -> Result<()> {
        let dialect = make_dialect(dialect_arg);
        let stmts = Parser::parse_sql(dialect.as_ref(), content).map_err(|e| {
            SqlKeelError::SchemaParse {
                path: src.to_string(),
                detail: e.to_string(),
            }
        })?;

        for stmt in stmts {
            if let Statement::CreateTable(ct) = stmt {
                let raw_name = ct.name.to_string();
                let (schema_name, table_name) = split_schema_table(&raw_name);
                let mut columns = IndexMap::new();

                // Collect table-level primary key columns
                let mut table_pks: Vec<String> = Vec::new();
                for constraint in &ct.constraints {
                    if let sqlparser::ast::TableConstraint::PrimaryKey(pk) = constraint {
                        for col in &pk.columns {
                            table_pks.push(
                                col.column
                                    .expr
                                    .to_string()
                                    .trim_matches('"')
                                    .to_ascii_lowercase(),
                            );
                        }
                    }
                }

                for col_def in &ct.columns {
                    let col_name = col_def.name.value.clone();
                    let col_key = col_name.to_ascii_lowercase();
                    let mut not_null = false;
                    let mut is_pk = table_pks.contains(&col_key);
                    for opt in &col_def.options {
                        match &opt.option {
                            ColumnOption::NotNull => not_null = true,
                            ColumnOption::PrimaryKey(_) => {
                                is_pk = true;
                                not_null = true;
                            }
                            _ => {}
                        }
                    }
                    columns.insert(
                        col_key,
                        ColumnSchema {
                            name: col_name,
                            data_type: col_def.data_type.to_string(),
                            nullable: !(not_null || is_pk),
                            primary_key: is_pk,
                        },
                    );
                }

                self.insert_table(TableSchema {
                    name: table_name,
                    schema_name,
                    source: src.to_string(),
                    columns,
                });
            }
        }
        Ok(())
    }
}

pub fn make_dialect(dialect: DialectArg) -> Box<dyn Dialect> {
    match dialect {
        DialectArg::Postgres => Box::new(PostgreSqlDialect {}),
        DialectArg::Generic => Box::new(GenericDialect {}),
        DialectArg::Snowflake => Box::new(SnowflakeDialect {}),
        DialectArg::Bigquery => Box::new(BigQueryDialect {}),
        DialectArg::Sqlite => Box::new(SQLiteDialect {}),
        DialectArg::Duckdb => Box::new(DuckDbDialect {}),
    }
}

fn is_schema_file(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(ext.as_str(), "yaml" | "yml" | "toml" | "sql")
}

fn split_schema_table(raw: &str) -> (Option<String>, String) {
    let parts: Vec<String> = raw
        .split('.')
        .map(|p| p.trim().trim_matches('"').trim_matches('`').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    match parts.len() {
        0 => (None, String::new()),
        1 => (None, parts[0].clone()),
        _ => (
            Some(parts[parts.len() - 2].clone()),
            parts[parts.len() - 1].clone(),
        ),
    }
}

pub fn normalize_table_ident(raw: &str) -> String {
    raw.split('.')
        .map(|p| {
            p.trim()
                .trim_matches('"')
                .trim_matches('`')
                .to_ascii_lowercase()
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn suggest_closest(needle: &str, candidates: &[&str]) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    let max_dist = (needle.len() / 2).clamp(1, 3);
    for &cand in candidates {
        let cl = cand.to_ascii_lowercase();
        let dist = levenshtein(needle, &cl);
        if dist <= max_dist && best.is_none_or(|(d, _)| dist < d) {
            best = Some((dist, cand));
        }
    }
    if let Some((_, s)) = best {
        return Some(s.to_string());
    }

    // Secondary heuristic for LLM hallucinations like `gpa_score` -> `gpa` or `student_email` -> `email`:
    // match when a candidate equals a snake_case token of the needle (or vice versa).
    let needle_tokens: Vec<&str> = needle.split('_').filter(|t| !t.is_empty()).collect();
    let mut token_best: Option<(usize, &str)> = None;
    for &cand in candidates {
        let cl = cand.to_ascii_lowercase();
        let cand_tokens: Vec<&str> = cl.split('_').filter(|t| !t.is_empty()).collect();
        if needle_tokens.contains(&cl.as_str()) || cand_tokens.contains(&needle) {
            let diff = needle.len().abs_diff(cl.len());
            if token_best.is_none_or(|(d, _)| diff < d) {
                token_best = Some((diff, cand));
            }
        }
    }
    token_best.map(|(_, s)| s.to_string())
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let (m, n) = (a_chars.len(), b_chars.len());
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

// Lightweight ODCS v3 YAML & datakeel TOML deserializers
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OdcsDoc {
    #[serde(default)]
    schema: Vec<OdcsObj>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OdcsObj {
    name: String,
    physical_name: Option<String>,
    #[serde(default)]
    properties: Vec<OdcsProp>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OdcsProp {
    name: String,
    #[serde(default = "default_str_type")]
    logical_type: String,
    physical_type: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    primary_key: bool,
}

fn default_str_type() -> String {
    "string".to_string()
}

#[derive(Debug, Deserialize)]
struct TomlContractDoc {
    contract: TomlContractHeader,
    #[serde(default)]
    columns: IndexMap<String, TomlCol>,
}

#[derive(Debug, Deserialize)]
struct TomlContractHeader {
    name: String,
    table: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlCol {
    #[serde(rename = "type", default = "default_str_type")]
    logical_type: String,
    physical_type: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    primary_key: bool,
}
