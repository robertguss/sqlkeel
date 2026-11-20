use crate::analyzer::CheckReport;
use crate::error::{Result, SqlKeelError};
use crate::lineage::LineageReport;
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

pub fn print_lineage_report(report: &LineageReport, color: bool) -> Result<()> {
    if let Some(ref br) = report.blast_radius {
        let label = match &br.column {
            Some(c) => format!("{}.{c}", br.target),
            None => br.target.clone(),
        };
        let badge = if color {
            " BLAST RADIUS ".on_yellow().black().bold().to_string()
        } else {
            "[BLAST RADIUS]".to_string()
        };
        println!(
            "{badge} {label} -> {} downstream model(s), {} impacted column(s) across {} SQL file(s)",
            br.impacted_models, br.impacted_columns, report.files_analyzed
        );
        if br.hits.is_empty() {
            println!("\n  No downstream dependencies found for '{label}'.");
        }
        for hit in &br.hits {
            let loc = hit
                .line
                .map_or_else(|| hit.file.clone(), |l| format!("{}:{l}", hit.file));
            let col_part = hit
                .output_column
                .as_deref()
                .map(|c| format!(".{c}"))
                .unwrap_or_default();
            println!(
                "\n  [hop {}] {}{col_part}  ({loc})",
                hit.hop_depth, hit.model
            );
            println!("    path: {}", hit.path.join(" -> "));
        }
        return Ok(());
    }

    println!(
        "Lineage: {} model(s), {} edge(s) across {} SQL file(s)\n",
        report.models_count, report.edge_count, report.files_analyzed
    );
    for m in &report.models {
        let jinja_tag = if m.had_jinja { " [jinja]" } else { "" };
        if color {
            println!("  {}{jinja_tag}  ({})", m.model.cyan().bold(), m.file.dimmed());
        } else {
            println!("  {}{jinja_tag}  ({})", m.model, m.file);
        }
        if let Some(ref err) = m.parse_error {
            println!("    parse_error: {err}");
            continue;
        }
        if !m.upstream_tables.is_empty() {
            println!("    upstream: {}", m.upstream_tables.join(", "));
        }
        for col in &m.columns {
            let srcs: Vec<String> = col
                .sources
                .iter()
                .map(|s| format!("{}.{}", s.table, s.column))
                .collect();
            if srcs.is_empty() {
                println!("    - {}  <- ({})", col.name, col.expression);
            } else {
                println!("    - {}  <- {}", col.name, srcs.join(", "));
            }
        }
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
