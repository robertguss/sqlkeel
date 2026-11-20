pub mod agent;
pub mod human;

use crate::analyzer::CheckReport;
use crate::cli::OutputFormatArg;
use crate::error::{Result, SqlKeelError};
use crate::lineage::LineageReport;
use crate::rules::RuleMeta;
use crate::schema::SchemaCatalog;
use std::io::IsTerminal;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedFormat {
    Human,
    Agent,
    Json,
}

pub fn resolve_format(arg: OutputFormatArg) -> ResolvedFormat {
    match arg {
        OutputFormatArg::Human => ResolvedFormat::Human,
        OutputFormatArg::Agent => ResolvedFormat::Agent,
        OutputFormatArg::Json => ResolvedFormat::Json,
        OutputFormatArg::Auto => {
            if std::io::stdout().is_terminal() {
                ResolvedFormat::Human
            } else {
                ResolvedFormat::Agent
            }
        }
    }
}

pub fn should_use_color(no_color_flag: bool, format: ResolvedFormat) -> bool {
    if no_color_flag || format != ResolvedFormat::Human {
        return false;
    }
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    std::io::stdout().is_terminal()
}

pub fn emit_check_report(
    report: &CheckReport,
    format: ResolvedFormat,
    use_color: bool,
) -> Result<()> {
    match format {
        ResolvedFormat::Human => human::print_check_report(report, use_color),
        ResolvedFormat::Agent => agent::print_check_report(report),
        ResolvedFormat::Json => print_json(report),
    }
}

pub fn emit_lineage_report(
    report: &LineageReport,
    format: ResolvedFormat,
    use_color: bool,
) -> Result<()> {
    match format {
        ResolvedFormat::Human => human::print_lineage_report(report, use_color),
        ResolvedFormat::Agent => agent::print_lineage_report(report),
        ResolvedFormat::Json => print_json(report),
    }
}

pub fn emit_rules(rules: &[RuleMeta], format: ResolvedFormat, use_color: bool) -> Result<()> {
    match format {
        ResolvedFormat::Human => human::print_rules(rules, use_color),
        ResolvedFormat::Agent | ResolvedFormat::Json => print_json(&serde_json::json!({
            "schema_version": "1.0",
            "command": "rules",
            "count": rules.len(),
            "rules": rules,
        })),
    }
}

pub fn emit_schema(
    catalog: &SchemaCatalog,
    format: ResolvedFormat,
    use_color: bool,
) -> Result<()> {
    match format {
        ResolvedFormat::Human => human::print_schema(catalog, use_color),
        ResolvedFormat::Agent | ResolvedFormat::Json => print_json(&serde_json::json!({
            "schema_version": "1.0",
            "command": "schema",
            "table_count": catalog.table_count(),
            "tables": catalog.tables,
        })),
    }
}

pub fn emit_skill_installed(
    installed_path: &Path,
    format: ResolvedFormat,
    use_color: bool,
) -> Result<()> {
    match format {
        ResolvedFormat::Human => human::print_skill_installed(installed_path, use_color),
        ResolvedFormat::Agent | ResolvedFormat::Json => print_json(&serde_json::json!({
            "schema_version": "1.0",
            "command": "skill",
            "ok": true,
            "installed_path": installed_path.display().to_string(),
        })),
    }
}

pub fn emit_error(err: &SqlKeelError, format: ResolvedFormat, use_color: bool) {
    match format {
        ResolvedFormat::Human => human::print_error(err, use_color),
        ResolvedFormat::Agent | ResolvedFormat::Json => {
            let env = err.to_envelope();
            if let Ok(json) = serde_json::to_string_pretty(&env) {
                eprintln!("{json}");
            } else {
                eprintln!("{{\"ok\":false,\"error\":{{\"code\":\"{}\"}}}}", err.code());
            }
        }
    }
}

fn print_json<T: serde::Serialize>(val: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(val)
        .map_err(|e| SqlKeelError::General(format!("JSON serialization error: {e}")))?;
    println!("{s}");
    Ok(())
}
