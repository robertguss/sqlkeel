use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "sqlkeel",
    version,
    about = "Blazing-fast, agent-first (DX + AX) Rust SQL semantic bug, schema-grounding, and Postgres migration safety analyzer.",
    long_about = None
)]
pub struct Cli {
    /// Output format: auto (human in TTY, agent when piped), human, agent, or json
    #[arg(long, global = true, value_enum, default_value_t = OutputFormatArg::Auto)]
    pub format: OutputFormatArg,

    /// Disable ANSI colors in human output
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormatArg {
    Auto,
    Human,
    Agent,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DialectArg {
    Postgres,
    Generic,
    Snowflake,
    Bigquery,
    Sqlite,
    Duckdb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FailOnArg {
    Error,
    Warning,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Analyze one or more SQL files, directories, or stdin ('-') for semantic bugs, schema errors, and migration hazards
    Check {
        /// SQL file(s), directory(ies), or '-' for stdin
        #[arg(required = true)]
        paths: Vec<String>,

        /// Schema source(s): ODCS v3 YAML, datakeel TOML, SQL DDL file, or a directory containing them (repeatable)
        #[arg(long = "schema", value_name = "PATH")]
        schemas: Vec<PathBuf>,

        /// SQL dialect for parsing
        #[arg(long, value_enum, default_value_t = DialectArg::Postgres)]
        dialect: DialectArg,

        /// Only run specific rule code(s) (repeatable or comma-separated)
        #[arg(long, value_delimiter = ',')]
        select: Vec<String>,

        /// Ignore specific rule code(s) (repeatable or comma-separated)
        #[arg(long, value_delimiter = ',')]
        ignore: Vec<String>,

        /// Minimum severity that causes exit code 1 (default: error)
        #[arg(long, value_enum, default_value_t = FailOnArg::Error)]
        fail_on: FailOnArg,

        /// Maximum diagnostics to display per file in agent mode (0 = unlimited)
        #[arg(long, default_value_t = 50)]
        max_diagnostics: usize,
    },

    /// Build zero-warehouse table & column-level SQL lineage DAGs and blast-radius impact reports
    Lineage {
        /// SQL file(s), directory(ies), or '-' for stdin (supports plain SQL views and dbt/Jinja models)
        #[arg(required = true)]
        paths: Vec<String>,

        /// Optional schema source(s) (ODCS v3 YAML, datakeel TOML, SQL DDL) to expand SELECT * and canonicalize names
        #[arg(long = "schema", value_name = "PATH")]
        schemas: Vec<PathBuf>,

        /// SQL dialect for parsing
        #[arg(long, value_enum, default_value_t = DialectArg::Postgres)]
        dialect: DialectArg,

        /// Compute transitive downstream blast radius for a table (e.g. `populi_students`)
        #[arg(long)]
        target: Option<String>,

        /// Compute transitive downstream column blast radius (e.g. `populi_students.gpa`)
        #[arg(long)]
        column: Option<String>,

        /// Output a Mermaid flowchart LR diagram of the table lineage DAG
        #[arg(long)]
        mermaid: bool,
    },

    /// List all built-in SQL semantic, schema, safety, and migration rules
    Rules,

    /// Load and inspect resolved table/column schemas from ODCS v3 YAML, datakeel TOML, or SQL DDL
    Schema {
        /// Schema file(s) or directory(ies) to load
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        /// SQL dialect used when parsing `.sql` DDL files
        #[arg(long, value_enum, default_value_t = DialectArg::Postgres)]
        dialect: DialectArg,
    },

    /// Print or install the bundled SKILL.md agent manual (for Claude Code, Antigravity, Codex, Cursor)
    Skill {
        /// Install SKILL.md into a directory (e.g. .agents/skills/sqlkeel)
        #[arg(long)]
        install: Option<PathBuf>,
    },
}
