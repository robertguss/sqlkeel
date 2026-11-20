use crate::analyzer::CheckReport;
use crate::error::{Result, SqlKeelError};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct AgentCheckEnvelope<'a> {
    schema_version: &'static str,
    command: &'static str,
    ok: bool,
    summary: String,
    files_checked: usize,
    schemas_loaded: usize,
    error_count: usize,
    warning_count: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    diagnostics: &'a Vec<crate::rules::Diagnostic>,
}

pub fn print_check_report(report: &CheckReport) -> Result<()> {
    let summary = if report.passed {
        format!(
            "PASS: checked {} SQL file(s) ({} schema tables grounded) — {} error(s), {} warning(s).",
            report.files_checked, report.schemas_loaded, report.error_count, report.warning_count
        )
    } else {
        format!(
            "FAIL: found {} error(s) and {} warning(s) across {} SQL file(s).",
            report.error_count, report.warning_count, report.files_checked
        )
    };

    let env = AgentCheckEnvelope {
        schema_version: "1.0",
        command: "check",
        ok: report.passed,
        summary,
        files_checked: report.files_checked,
        schemas_loaded: report.schemas_loaded,
        error_count: report.error_count,
        warning_count: report.warning_count,
        truncated: report.truncated,
        diagnostics: &report.diagnostics,
    };

    let json = serde_json::to_string(&env)
        .map_err(|e| SqlKeelError::General(format!("JSON serialization error: {e}")))?;
    println!("{json}");
    Ok(())
}
