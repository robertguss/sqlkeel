use crate::analyzer::CheckReport;
use crate::error::{Result, SqlKeelError};
use crate::rules::{RuleMeta, Severity};
use crate::schema::SchemaCatalog;
use owo_colors::OwoColorize;
use std::path::Path;

pub fn print_check_report(report: &CheckReport, color: bool) -> Result<()> {
    let schema_note = if report.schemas_loaded > 0 {
        format!(", {} schema table(s) grounded", report.schemas_loaded)
    } else {
        String::new()
    };

    if report.passed {
        let badge = if color {
            " PASS ".on_green().black().bold().to_string()
        } else {
            "[PASS]".to_string()
        };
        println!(
            "{badge} {} file(s) checked{schema_note} — {} error(s), {} warning(s)",
            report.files_checked, report.error_count, report.warning_count
        );
    } else {
        let badge = if color {
            " FAIL ".on_red().white().bold().to_string()
        } else {
            "[FAIL]".to_string()
        };
        println!(
            "{badge} {} file(s) checked{schema_note} — {} error(s), {} warning(s)",
            report.files_checked, report.error_count, report.warning_count
        );
    }

    for d in &report.diagnostics {
        let sev = match d.severity {
            Severity::Error => {
                if color {
                    "ERROR".red().bold().to_string()
                } else {
                    "ERROR".to_string()
                }
            }
            Severity::Warning => {
                if color {
                    "WARN ".yellow().bold().to_string()
                } else {
                    "WARN ".to_string()
                }
            }
        };
        let loc = format!("{}:{}:{}", d.file, d.line, d.column);
        if color {
            println!("\n  {sev} {}  {}", d.code.bold(), loc.dimmed());
        } else {
            println!("\n  {sev} {}  {loc}", d.code);
        }
        println!("    {}", d.message);
        if color {
            println!("    {} {}", "hint:".cyan().bold(), d.fix_hint);
        } else {
            println!("    hint: {}", d.fix_hint);
        }
    }

    if report.truncated {
        println!("\n  (output truncated; pass `--max-diagnostics 0` to show all)");
    }

    Ok(())
}

pub fn print_rules(rules: &[RuleMeta], color: bool) -> Result<()> {
    println!("Built-in sqlkeel rules ({}):\n", rules.len());
    for r in rules {
        let sev = match r.default_severity {
            Severity::Error => {
                if color {
                    "error".red().bold().to_string()
                } else {
                    "error".to_string()
                }
            }
            Severity::Warning => {
                if color {
                    "warn ".yellow().bold().to_string()
                } else {
                    "warn ".to_string()
                }
            }
        };
        println!("  {:<28} [{:?}] ({sev})  {}", r.code, r.category, r.summary);
    }
    Ok(())
}

pub fn print_schema(catalog: &SchemaCatalog, color: bool) -> Result<()> {
    println!("Loaded {} table(s):\n", catalog.table_count());
    for (key, tbl) in &catalog.tables {
        if color {
            println!("  {}  ({})", key.cyan().bold(), tbl.source.dimmed());
        } else {
            println!("  {key}  ({})", tbl.source);
        }
        for col in tbl.columns.values() {
            let mut tags = Vec::new();
            if col.primary_key {
                tags.push("PK");
            }
            if !col.nullable {
                tags.push("NOT NULL");
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            println!("    - {}: {}{tag_str}", col.name, col.data_type);
        }
    }
    Ok(())
}

pub fn print_skill_installed(installed_path: &Path, color: bool) -> Result<()> {
    let badge = if color {
        " INSTALLED ".on_green().black().bold().to_string()
    } else {
        "[INSTALLED]".to_string()
    };
    println!("{badge} Agent skill written to {}", installed_path.display());
    Ok(())
}

pub fn print_error(err: &SqlKeelError, color: bool) {
    let badge = if color {
        " ERROR ".on_red().white().bold().to_string()
    } else {
        "[ERROR]".to_string()
    };
    eprintln!("{badge} {}: {err}", err.code());
    if let Some(hint) = err.fix_hint() {
        eprintln!("  hint: {hint}");
    }
}
