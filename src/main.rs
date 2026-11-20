use clap::Parser;
use sqlkeel::analyzer::{self, CheckConfig};
use sqlkeel::cli::{Cli, Commands};
use sqlkeel::error::{EXIT_ERROR, EXIT_OK, EXIT_VIOLATION, Result};
use sqlkeel::lineage;
use sqlkeel::output::{self, ResolvedFormat};
use sqlkeel::rules::ALL_RULES;
use sqlkeel::schema::SchemaCatalog;
use sqlkeel::skill;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let format = output::resolve_format(cli.format);
    let use_color = output::should_use_color(cli.no_color, format);

    match run(cli, format, use_color) {
        Ok(code) => ExitCode::from(code as u8),
        Err(err) => {
            output::emit_error(&err, format, use_color);
            ExitCode::from(EXIT_ERROR as u8)
        }
    }
}

fn run(cli: Cli, format: ResolvedFormat, use_color: bool) -> Result<i32> {
    match cli.command {
        Commands::Check {
            paths,
            schemas,
            dialect,
            select,
            ignore,
            fail_on,
            max_diagnostics,
        } => {
            let (select_set, ignore_set) = analyzer::validate_rule_filters(&select, &ignore)?;
            let catalog = if schemas.is_empty() {
                SchemaCatalog::default()
            } else {
                SchemaCatalog::load_from_paths(&schemas, dialect)?
            };

            let report = analyzer::run_check(
                &paths,
                &catalog,
                &CheckConfig {
                    dialect,
                    select: select_set,
                    ignore: ignore_set,
                    fail_on,
                    max_diagnostics,
                },
            )?;

            output::emit_check_report(&report, format, use_color)?;
            if report.passed {
                Ok(EXIT_OK)
            } else {
                Ok(EXIT_VIOLATION)
            }
        }

        Commands::Lineage {
            paths,
            schemas,
            dialect,
            target,
            column,
            mermaid,
        } => {
            let catalog = if schemas.is_empty() {
                SchemaCatalog::default()
            } else {
                SchemaCatalog::load_from_paths(&schemas, dialect)?
            };
            let report = lineage::analyze_lineage(
                &paths,
                &catalog,
                dialect,
                target.as_deref(),
                column.as_deref(),
            )?;
            if mermaid {
                print!("{}", lineage::render_mermaid(&report));
            } else {
                output::emit_lineage_report(&report, format, use_color)?;
            }
            Ok(EXIT_OK)
        }

        Commands::Rules => {
            output::emit_rules(ALL_RULES, format, use_color)?;
            Ok(EXIT_OK)
        }

        Commands::Schema { paths, dialect } => {
            let catalog = SchemaCatalog::load_from_paths(&paths, dialect)?;
            output::emit_schema(&catalog, format, use_color)?;
            Ok(EXIT_OK)
        }

        Commands::Skill { install } => {
            if let Some(dir) = install {
                let installed_path = skill::install_skill(&dir)?;
                output::emit_skill_installed(&installed_path, format, use_color)?;
            } else {
                print!("{}", skill::SKILL_MD);
            }
            Ok(EXIT_OK)
        }
    }
}
