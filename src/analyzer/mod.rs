use crate::cli::{DialectArg, FailOnArg};
use crate::error::{Result, SqlKeelError};
use crate::rules::{ALL_RULES, Diagnostic, Severity, Suppressions, is_known_rule};
use crate::schema::{SchemaCatalog, TableSchema, make_dialect, normalize_table_ident};
use ignore::WalkBuilder;
use rayon::prelude::*;
use serde::Serialize;
use sqlparser::ast::{
    AlterColumnOperation, AlterTableOperation, BinaryOperator, ColumnOption, Expr,
    FunctionArg, FunctionArgExpr, FunctionArguments, InTarget, JoinConstraint, JoinOperator,
    ObjectType, Query, Select, SelectItem, SetExpr, Spanned, Statement, TableConstraint,
    TableFactor, TableWithJoins, Value,
};
use sqlparser::parser::Parser;
use sqlparser::tokenizer::Span;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CheckConfig {
    pub dialect: DialectArg,
    pub select: HashSet<String>,
    pub ignore: HashSet<String>,
    pub fail_on: FailOnArg,
    pub max_diagnostics: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub schema_version: &'static str,
    pub command: &'static str,
    pub passed: bool,
    pub files_checked: usize,
    pub schemas_loaded: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub truncated: bool,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn validate_rule_filters(select: &[String], ignore: &[String]) -> Result<(HashSet<String>, HashSet<String>)> {
    let mut sel = HashSet::new();
    for code in select {
        let up = code.trim().to_ascii_uppercase();
        if !up.is_empty() {
            if !is_known_rule(&up) {
                return Err(SqlKeelError::UnknownRule { code: up });
            }
            sel.insert(up);
        }
    }
    let mut ign = HashSet::new();
    for code in ignore {
        let up = code.trim().to_ascii_uppercase();
        if !up.is_empty() {
            if !is_known_rule(&up) {
                return Err(SqlKeelError::UnknownRule { code: up });
            }
            ign.insert(up);
        }
    }
    Ok((sel, ign))
}

pub fn run_check(
    paths: &[String],
    schema: &SchemaCatalog,
    config: &CheckConfig,
) -> Result<CheckReport> {
    let inputs = collect_sql_inputs(paths)?;
    if inputs.is_empty() {
        return Err(SqlKeelError::NoSqlFiles);
    }

    let files_checked = inputs.len();

    let mut per_file: Vec<(String, Vec<Diagnostic>)> = inputs
        .par_iter()
        .map(|(name, sql)| (name.clone(), analyze_sql_str(name, sql, schema, config)))
        .collect();

    // Deterministic ordering by file name, then line/col
    per_file.sort_by(|a, b| a.0.cmp(&b.0));

    let mut all_diags = Vec::new();
    for (_, mut diags) in per_file {
        diags.sort_by_key(|d| (d.line, d.column, d.code));
        all_diags.extend(diags);
    }

    let error_count = all_diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warning_count = all_diags
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();

    let passed = match config.fail_on {
        FailOnArg::Error => error_count == 0,
        FailOnArg::Warning => error_count == 0 && warning_count == 0,
    };

    let truncated = config.max_diagnostics > 0 && all_diags.len() > config.max_diagnostics;
    if truncated {
        all_diags.truncate(config.max_diagnostics);
    }

    Ok(CheckReport {
        schema_version: "1.0",
        command: "check",
        passed,
        files_checked,
        schemas_loaded: schema.table_count(),
        error_count,
        warning_count,
        truncated,
        diagnostics: all_diags,
    })
}

fn collect_sql_inputs(paths: &[String]) -> Result<Vec<(String, String)>> {
    let mut results = Vec::new();
    let mut file_paths: Vec<PathBuf> = Vec::new();

    for raw in paths {
        if raw == "-" {
            let mut buf = String::new();
            io::stdin()
                .read_to_string(&mut buf)
                .map_err(|source| SqlKeelError::IoRead {
                    path: PathBuf::from("-"),
                    source,
                })?;
            results.push(("<stdin>".to_string(), buf));
            continue;
        }
        let p = Path::new(raw);
        if p.is_dir() {
            for entry in WalkBuilder::new(p).standard_filters(true).build().flatten() {
                let ep = entry.path();
                if ep.is_file()
                    && ep
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
                {
                    file_paths.push(ep.to_path_buf());
                }
            }
        } else if p.is_file() {
            file_paths.push(p.to_path_buf());
        } else {
            return Err(SqlKeelError::IoRead {
                path: p.to_path_buf(),
                source: io::Error::new(io::ErrorKind::NotFound, "path does not exist"),
            });
        }
    }

    file_paths.sort();
    file_paths.dedup();

    for fp in file_paths {
        let sql = fs::read_to_string(&fp).map_err(|source| SqlKeelError::IoRead {
            path: fp.clone(),
            source,
        })?;
        results.push((fp.display().to_string(), sql));
    }

    Ok(results)
}

pub fn analyze_sql_str(
    file: &str,
    sql: &str,
    schema: &SchemaCatalog,
    config: &CheckConfig,
) -> Vec<Diagnostic> {
    let suppressions = Suppressions::parse(sql);
    let dialect = make_dialect(config.dialect);

    let mut ctx = AnalyzerCtx {
        file: file.to_string(),
        schema,
        config,
        suppressions,
        diagnostics: Vec::new(),
    };

    let stmts = match Parser::parse_sql(dialect.as_ref(), sql) {
        Ok(s) => s,
        Err(err) => {
            let (line, col) = extract_parse_error_loc(&err.to_string());
            ctx.emit(
                "SK_PARSE_ERROR",
                line,
                col,
                err.to_string(),
                "Fix the SQL syntax error or pass `--dialect` matching your target database."
                    .to_string(),
                None,
            );
            return ctx.diagnostics;
        }
    };

    for stmt in &stmts {
        ctx.analyze_statement(stmt);
    }

    ctx.diagnostics
}

struct AnalyzerCtx<'a> {
    file: String,
    schema: &'a SchemaCatalog,
    config: &'a CheckConfig,
    suppressions: Suppressions,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Default)]
struct FromScope<'a> {
    /// Lowercased alias or bare table name -> resolved schema table (None if CTE/subquery/unknown)
    aliases: HashMap<String, Option<&'a TableSchema>>,
    /// Aliases introduced on the right side of a LEFT/FULL OUTER JOIN
    left_joined_aliases: HashSet<String>,
    /// True if any table in scope is unresolved (CTE, subquery, or unknown table)
    has_opaque_source: bool,
}

impl<'a> AnalyzerCtx<'a> {
    fn emit(
        &mut self,
        code: &'static str,
        line: usize,
        column: usize,
        message: String,
        fix_hint: String,
        suggestion: Option<String>,
    ) {
        let line = line.max(1);
        let column = column.max(1);

        if !self.config.select.is_empty() && !self.config.select.contains(code) {
            return;
        }
        if self.config.ignore.contains(code) {
            return;
        }
        if self.suppressions.is_suppressed(line, code) {
            return;
        }

        let meta = ALL_RULES
            .iter()
            .find(|r| r.code == code)
            .expect("rule code registered in ALL_RULES");

        self.diagnostics.push(Diagnostic {
            code: meta.code,
            category: meta.category,
            severity: meta.default_severity,
            file: self.file.clone(),
            line,
            column,
            message,
            fix_hint,
            suggestion,
        });
    }

    fn span_loc(span: Span) -> (usize, usize) {
        (span.start.line as usize, span.start.column as usize)
    }

    fn analyze_statement(&mut self, stmt: &Statement) {
        let (stmt_line, stmt_col) = Self::span_loc(stmt.span());
        match stmt {
            Statement::Query(query) => {
                let mut ctes = HashSet::new();
                self.analyze_query(query, &mut ctes, false);
            }
            Statement::CreateView(cv) => {
                let mut ctes = HashSet::new();
                self.analyze_query(&cv.query, &mut ctes, false);
            }
            Statement::Insert(ins) => {
                if !self.schema.is_empty() {
                    let raw = ins.table.to_string();
                    self.check_table_exists(&raw, stmt_line, stmt_col);
                }
                if let Some(ref source) = ins.source {
                    let mut ctes = HashSet::new();
                    self.analyze_query(source, &mut ctes, false);
                }
            }
            Statement::Delete(del) => {
                if del.selection.is_none() {
                    self.emit(
                        "SK_DELETE_WITHOUT_WHERE",
                        stmt_line,
                        stmt_col,
                        "DELETE statement has no WHERE clause and will delete every row."
                            .to_string(),
                        "Add a selective WHERE predicate (or use TRUNCATE intentionally with `-- sqlkeel:ignore SK_PG_DESTRUCTIVE_DDL`).".to_string(),
                        None,
                    );
                }
                let mut scope = FromScope::default();
                let ctes = HashSet::new();
                let from_tables = match &del.from {
                    sqlparser::ast::FromTable::WithFromKeyword(t)
                    | sqlparser::ast::FromTable::WithoutKeyword(t) => t,
                };
                for twj in from_tables {
                    self.register_twj(twj, &ctes, &mut scope);
                }
                if let Some(ref sel) = del.selection {
                    self.analyze_expr(sel, &scope, &ctes, &HashSet::new());
                }
            }
            Statement::Update(upd) => {
                if upd.selection.is_none() {
                    self.emit(
                        "SK_UPDATE_WITHOUT_WHERE",
                        stmt_line,
                        stmt_col,
                        "UPDATE statement has no WHERE clause and will modify every row."
                            .to_string(),
                        "Add a WHERE clause to scope the UPDATE safely.".to_string(),
                        None,
                    );
                }
                let mut scope = FromScope::default();
                let ctes = HashSet::new();
                self.register_twj(&upd.table, &ctes, &mut scope);
                if let Some(ref sel) = upd.selection {
                    self.analyze_expr(sel, &scope, &ctes, &HashSet::new());
                }
                for assign in &upd.assignments {
                    self.analyze_expr(&assign.value, &scope, &ctes, &HashSet::new());
                }
            }
            Statement::CreateIndex(ci) => {
                if !ci.concurrently {
                    let idx_name = ci
                        .name
                        .as_ref()
                        .map_or_else(|| "<unnamed>".to_string(), ToString::to_string);
                    self.emit(
                        "SK_PG_CREATE_INDEX_LOCK",
                        stmt_line,
                        stmt_col,
                        format!(
                            "CREATE INDEX '{idx_name}' on '{}' is missing CONCURRENTLY and will block writes in Postgres.",
                            ci.table_name
                        ),
                        "Use `CREATE INDEX CONCURRENTLY ...` outside a transaction block for zero-downtime migrations.".to_string(),
                        Some("CREATE INDEX CONCURRENTLY".to_string()),
                    );
                }
            }
            Statement::Drop {
                object_type,
                names,
                ..
            } => {
                let target_list = names
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                match object_type {
                    ObjectType::Index => {
                        self.emit(
                            "SK_PG_DROP_INDEX_LOCK",
                            stmt_line,
                            stmt_col,
                            format!(
                                "DROP INDEX '{target_list}' without CONCURRENTLY acquires an ACCESS EXCLUSIVE lock in Postgres."
                            ),
                            "Use `DROP INDEX CONCURRENTLY IF EXISTS ...` outside a transaction block.".to_string(),
                            Some("DROP INDEX CONCURRENTLY".to_string()),
                        );
                    }
                    ObjectType::Table => {
                        self.emit(
                            "SK_PG_DESTRUCTIVE_DDL",
                            stmt_line,
                            stmt_col,
                            format!("DROP TABLE '{target_list}' permanently destroys table data."),
                            "Confirm backup/retention requirements before running destructive DDL, or suppress with `-- sqlkeel:ignore SK_PG_DESTRUCTIVE_DDL`.".to_string(),
                            None,
                        );
                    }
                    _ => {}
                }
            }
            Statement::Truncate(tr) => {
                let names = tr
                    .table_names
                    .iter()
                    .map(|t| t.name.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.emit(
                    "SK_PG_DESTRUCTIVE_DDL",
                    stmt_line,
                    stmt_col,
                    format!("TRUNCATE on '{names}' permanently wipes all rows and takes an ACCESS EXCLUSIVE lock."),
                    "Verify TRUNCATE is intentional (e.g. staging table refresh) or suppress with `-- sqlkeel:ignore SK_PG_DESTRUCTIVE_DDL`.".to_string(),
                    None,
                );
            }
            Statement::AlterTable(at) => {
                for op in &at.operations {
                    let (op_line, op_col) = Self::span_loc(op.span());
                    let (l, c) = if op_line > 0 {
                        (op_line, op_col)
                    } else {
                        (stmt_line, stmt_col)
                    };
                    match op {
                        AlterTableOperation::AddColumn { column_def, .. } => {
                            let has_not_null = column_def
                                .options
                                .iter()
                                .any(|o| matches!(o.option, ColumnOption::NotNull));
                            let has_default = column_def
                                .options
                                .iter()
                                .any(|o| matches!(o.option, ColumnOption::Default(_)));
                            if has_not_null && !has_default {
                                self.emit(
                                    "SK_PG_ADD_COLUMN_NOT_NULL",
                                    l,
                                    c,
                                    format!(
                                        "ALTER TABLE '{}' ADD COLUMN '{}' NOT NULL without a DEFAULT fails on non-empty tables.",
                                        at.name, column_def.name
                                    ),
                                    "Add a safe DEFAULT value, or add the column nullable first, backfill rows, then add a NOT VALID CHECK constraint.".to_string(),
                                    None,
                                );
                            }
                        }
                        AlterTableOperation::AlterColumn {
                            column_name,
                            op: AlterColumnOperation::SetDataType { data_type, .. },
                        } => {
                            self.emit(
                                "SK_PG_ALTER_COLUMN_TYPE",
                                l,
                                c,
                                format!(
                                    "ALTER TABLE '{}' ALTER COLUMN '{}' TYPE {} acquires an ACCESS EXCLUSIVE lock and may rewrite the table.",
                                    at.name, column_name, data_type
                                ),
                                "For large production tables, use expand-and-contract (add new column, backfill, swap) instead of rewriting in place.".to_string(),
                                None,
                            );
                        }
                        AlterTableOperation::AddConstraint {
                            constraint,
                            not_valid,
                        } if !not_valid
                            && matches!(
                                constraint,
                                TableConstraint::Check(_) | TableConstraint::ForeignKey(_)
                            ) =>
                        {
                            self.emit(
                                "SK_PG_ADD_CONSTRAINT_LOCK",
                                l,
                                c,
                                format!(
                                    "ALTER TABLE '{}' ADD CONSTRAINT without NOT VALID scans the full table under lock.",
                                    at.name
                                ),
                                "Add `NOT VALID` first, then run `ALTER TABLE ... VALIDATE CONSTRAINT ...` in a separate step so reads/writes aren't blocked.".to_string(),
                                Some("ADD CONSTRAINT ... NOT VALID".to_string()),
                            );
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn analyze_query(&mut self, query: &Query, outer_ctes: &mut HashSet<String>, in_exists: bool) {
        let mut local_ctes = outer_ctes.clone();

        // 1. Analyze CTEs in order and track unused CTEs
        if let Some(ref with) = query.with {
            let mut defined_ctes: Vec<(String, usize, usize)> = Vec::new();
            for cte in &with.cte_tables {
                let cte_name = cte.alias.name.value.to_ascii_lowercase();
                let (line, col) = Self::span_loc(cte.alias.name.span);
                // Recursive CTEs can reference themselves
                if with.recursive {
                    local_ctes.insert(cte_name.clone());
                }
                self.analyze_query(&cte.query, &mut local_ctes, false);
                local_ctes.insert(cte_name.clone());
                defined_ctes.push((cte_name, line, col));
            }

            // Collect all table identifiers referenced in the main query body and later CTEs
            let mut referenced = HashSet::new();
            collect_referenced_tables_in_set_expr(&query.body, &mut referenced);
            for (idx, (cte_name, line, col)) in defined_ctes.iter().enumerate() {
                let mut used_in_later_cte = false;
                for later in &with.cte_tables[(idx + 1)..] {
                    let mut later_refs = HashSet::new();
                    collect_referenced_tables_in_query(&later.query, &mut later_refs);
                    if later_refs.contains(cte_name) {
                        used_in_later_cte = true;
                        break;
                    }
                }
                if !referenced.contains(cte_name) && !used_in_later_cte {
                    self.emit(
                        "SK_UNUSED_CTE",
                        *line,
                        *col,
                        format!("CTE '{cte_name}' is defined in WITH but never referenced."),
                        format!("Remove unused CTE '{cte_name}' or join/select from it in the final query."),
                        None,
                    );
                }
            }
        }

        self.analyze_set_expr(&query.body, &local_ctes, in_exists);
    }

    fn analyze_set_expr(&mut self, set_expr: &SetExpr, ctes: &HashSet<String>, in_exists: bool) {
        match set_expr {
            SetExpr::Select(select) => self.analyze_select(select, ctes, in_exists),
            SetExpr::Query(q) => {
                let mut cloned = ctes.clone();
                self.analyze_query(q, &mut cloned, in_exists);
            }
            SetExpr::SetOperation { left, right, .. } => {
                self.analyze_set_expr(left, ctes, in_exists);
                self.analyze_set_expr(right, ctes, in_exists);
            }
            _ => {}
        }
    }

    fn analyze_select(&mut self, select: &Select, ctes: &HashSet<String>, in_exists: bool) {
        let mut scope = FromScope::default();

        // Implicit comma join check (`FROM a, b`)
        if select.from.len() > 1 {
            let (l, c) = Self::span_loc(select.from[1].span());
            self.emit(
                "SK_CROSS_JOIN",
                l,
                c,
                "Implicit comma-join (`FROM a, b`) produces a Cartesian product unless constrained in WHERE."
                    .to_string(),
                "Rewrite using an explicit `INNER JOIN ... ON ...` (or explicit `CROSS JOIN` with an ignore comment if intentional).".to_string(),
                None,
            );
        }

        for twj in &select.from {
            self.register_twj(twj, ctes, &mut scope);
        }

        // Collect SELECT projection aliases (so GROUP BY / HAVING can reference them without false positives)
        let mut select_aliases: HashSet<String> = HashSet::new();
        for item in &select.projection {
            match item {
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => {
                    if !in_exists {
                        let (l, c) = Self::span_loc(item.span());
                        self.emit(
                            "SK_SELECT_STAR",
                            l,
                            c,
                            "Avoid `SELECT *` in production queries/views — upstream schema additions can silently break downstream models.".to_string(),
                            "List the required columns explicitly so downstream schemas stay deterministic.".to_string(),
                            None,
                        );
                    }
                }
                SelectItem::UnnamedExpr(expr) => {
                    self.analyze_expr(expr, &scope, ctes, &HashSet::new());
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    select_aliases.insert(alias.value.to_ascii_lowercase());
                    self.analyze_expr(expr, &scope, ctes, &HashSet::new());
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    for alias in aliases {
                        select_aliases.insert(alias.value.to_ascii_lowercase());
                    }
                    self.analyze_expr(expr, &scope, ctes, &HashSet::new());
                }
            }
        }

        // Analyze JOIN ON conditions
        for twj in &select.from {
            for join in &twj.joins {
                match &join.join_operator {
                    JoinOperator::Inner(JoinConstraint::On(e))
                    | JoinOperator::Left(JoinConstraint::On(e))
                    | JoinOperator::LeftOuter(JoinConstraint::On(e))
                    | JoinOperator::Right(JoinConstraint::On(e))
                    | JoinOperator::RightOuter(JoinConstraint::On(e))
                    | JoinOperator::FullOuter(JoinConstraint::On(e))
                    | JoinOperator::Join(JoinConstraint::On(e)) => {
                        self.analyze_expr(e, &scope, ctes, &HashSet::new());
                    }
                    _ => {}
                }
            }
        }

        // Analyze WHERE clause (including the LEFT JOIN -> INNER JOIN trap!)
        if let Some(ref selection) = select.selection {
            if !scope.left_joined_aliases.is_empty() {
                self.check_left_join_where_trap(selection, &scope.left_joined_aliases);
            }
            self.analyze_expr(selection, &scope, ctes, &HashSet::new());
        }

        // Analyze GROUP BY / HAVING
        if let sqlparser::ast::GroupByExpr::Expressions(ref exprs, _) = select.group_by {
            for e in exprs {
                self.analyze_expr(e, &scope, ctes, &select_aliases);
            }
        }
        if let Some(ref having) = select.having {
            self.analyze_expr(having, &scope, ctes, &select_aliases);
        }
    }

    fn register_twj(
        &mut self,
        twj: &TableWithJoins,
        ctes: &HashSet<String>,
        scope: &mut FromScope<'a>,
    ) {
        self.register_table_factor(&twj.relation, ctes, scope, false);
        for join in &twj.joins {
            let is_left_side_nullable = matches!(
                join.join_operator,
                JoinOperator::Left(_) | JoinOperator::LeftOuter(_) | JoinOperator::FullOuter(_)
            );
            if matches!(join.join_operator, JoinOperator::CrossJoin(_)) {
                let (l, c) = Self::span_loc(join.relation.span());
                self.emit(
                    "SK_CROSS_JOIN",
                    l,
                    c,
                    "Explicit CROSS JOIN creates a Cartesian product (rows_left * rows_right)."
                        .to_string(),
                    "Verify the CROSS JOIN is intentional (e.g. date spine) or replace with a keyed JOIN.".to_string(),
                    None,
                );
            }
            self.register_table_factor(&join.relation, ctes, scope, is_left_side_nullable);
        }
    }

    fn register_table_factor(
        &mut self,
        factor: &TableFactor,
        ctes: &HashSet<String>,
        scope: &mut FromScope<'a>,
        is_left_joined: bool,
    ) {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let raw = name.to_string();
                let norm = normalize_table_ident(&raw);
                let bare = norm.rsplit_once('.').map_or(norm.as_str(), |(_, b)| b);
                let alias_key = alias
                    .as_ref()
                    .map_or_else(|| bare.to_string(), |a| a.name.value.to_ascii_lowercase());

                if is_left_joined {
                    scope.left_joined_aliases.insert(alias_key.clone());
                }

                if ctes.contains(bare) {
                    scope.aliases.insert(alias_key, None);
                    scope.has_opaque_source = true;
                } else if !self.schema.is_empty() {
                    if let Some(tbl) = self.schema.resolve_table(&raw) {
                        scope.aliases.insert(alias_key, Some(tbl));
                    } else {
                        let (l, c) = Self::span_loc(name.span());
                        self.check_table_exists(&raw, l, c);
                        scope.aliases.insert(alias_key, None);
                        scope.has_opaque_source = true;
                    }
                } else {
                    scope.aliases.insert(alias_key, None);
                    scope.has_opaque_source = true;
                }
            }
            TableFactor::Derived {
                subquery, alias, ..
            } => {
                let mut cloned = ctes.clone();
                self.analyze_query(subquery, &mut cloned, false);
                if let Some(a) = alias {
                    let key = a.name.value.to_ascii_lowercase();
                    if is_left_joined {
                        scope.left_joined_aliases.insert(key.clone());
                    }
                    scope.aliases.insert(key, None);
                }
                scope.has_opaque_source = true;
            }
            TableFactor::NestedJoin {
                table_with_joins, ..
            } => {
                self.register_twj(table_with_joins, ctes, scope);
            }
            _ => {
                scope.has_opaque_source = true;
            }
        }
    }

    fn check_table_exists(&mut self, raw_name: &str, line: usize, col: usize) {
        if self.schema.resolve_table(raw_name).is_none() {
            let suggestion = self.schema.suggest_table(raw_name);
            let hint = match &suggestion {
                Some(s) => format!(
                    "Did you mean '{s}'? Run `sqlkeel schema <path>` to inspect all loaded tables."
                ),
                None => "Verify table spelling or pass all contract/DDL files via `--schema`."
                    .to_string(),
            };
            self.emit(
                "SK_UNKNOWN_TABLE",
                line,
                col,
                format!("Table '{raw_name}' was not found in the loaded schema catalog."),
                hint,
                suggestion,
            );
        }
    }

    /// Detect top-level AND conjuncts in WHERE that reject NULL rows from a LEFT JOINed alias.
    fn check_left_join_where_trap(&mut self, expr: &Expr, left_aliases: &HashSet<String>) {
        match expr {
            Expr::Nested(inner) => self.check_left_join_where_trap(inner, left_aliases),
            Expr::BinaryOp {
                left,
                op: BinaryOperator::And,
                right,
            } => {
                self.check_left_join_where_trap(left, left_aliases);
                self.check_left_join_where_trap(right, left_aliases);
            }
            // OR conjuncts like `(c.score < 70 OR c.enrollment_id IS NULL)` preserve NULL rows — skip if safe!
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Or,
                right,
            } => {
                if expr_contains_is_null_for(left, left_aliases)
                    || expr_contains_is_null_for(right, left_aliases)
                {
                    return;
                }
                // If BOTH sides require non-null on the same left-joined alias (e.g. `c.a = 1 OR c.b = 2`), flag it!
                if let Some((alias, col, span)) = find_null_rejecting_left_ref(expr, left_aliases) {
                    self.emit_left_join_trap(&alias, &col, span);
                }
            }
            _ => {
                if let Some((alias, col, span)) = find_null_rejecting_left_ref(expr, left_aliases) {
                    self.emit_left_join_trap(&alias, &col, span);
                }
            }
        }
    }

    fn emit_left_join_trap(&mut self, alias: &str, col: &str, span: Span) {
        let (l, c) = Self::span_loc(span);
        self.emit(
            "SK_LEFT_JOIN_WHERE_FILTER",
            l,
            c,
            format!(
                "Filtering LEFT JOINed column '{alias}.{col}' in WHERE discards NULL rows and silently turns the LEFT JOIN into an INNER JOIN."
            ),
            format!(
                "Move the condition on '{alias}.{col}' into the `LEFT JOIN ... ON` clause, or guard it with `OR {alias}.{col} IS NULL` / `COALESCE(...)`."
            ),
            None,
        );
    }

    fn analyze_expr(
        &mut self,
        expr: &Expr,
        scope: &FromScope<'a>,
        ctes: &HashSet<String>,
        select_aliases: &HashSet<String>,
    ) {
        match expr {
            Expr::BinaryOp { left, op, right } => {
                // Check `= NULL`, `!= NULL`, `<> NULL`
                if matches!(op, BinaryOperator::Eq | BinaryOperator::NotEq)
                    && (is_null_literal(left) || is_null_literal(right))
                {
                    let (l, c) = Self::span_loc(expr.span());
                    let replacement = if matches!(op, BinaryOperator::Eq) {
                        "IS NULL"
                    } else {
                        "IS NOT NULL"
                    };
                    self.emit(
                        "SK_NULL_COMPARISON",
                        l,
                        c,
                        format!(
                            "Comparing against NULL with `{op}` always evaluates to UNKNOWN (never true)."
                        ),
                        format!("Replace `{op} NULL` with `{replacement}`."),
                        Some(replacement.to_string()),
                    );
                }
                self.analyze_expr(left, scope, ctes, select_aliases);
                self.analyze_expr(right, scope, ctes, select_aliases);
            }
            Expr::In {
                expr: inner,
                target,
                negated,
                ..
            } => {
                self.analyze_expr(inner, scope, ctes, select_aliases);
                match target {
                    InTarget::Subquery(subquery) => {
                        if *negated && !self.is_subquery_provably_non_null(subquery, ctes) {
                            let (l, c) = Self::span_loc(expr.span());
                            self.emit(
                                "SK_NOT_IN_SUBQUERY",
                                l,
                                c,
                                "`NOT IN (SELECT ...)` silently returns 0 rows if the subquery produces even a single NULL.".to_string(),
                                "Rewrite using `NOT EXISTS (SELECT 1 FROM ... WHERE ...)` or add `WHERE <col> IS NOT NULL` inside the subquery.".to_string(),
                                Some("NOT EXISTS (SELECT 1 FROM ...)".to_string()),
                            );
                        }
                        let mut cloned = ctes.clone();
                        self.analyze_query(subquery, &mut cloned, false);
                    }
                    InTarget::List(list) => {
                        for item in list {
                            self.analyze_expr(item, scope, ctes, select_aliases);
                        }
                    }
                    InTarget::Unnest(arr_expr) => {
                        self.analyze_expr(arr_expr, scope, ctes, select_aliases);
                    }
                    InTarget::Table { .. } => {}
                }
            }
            Expr::Exists { subquery, .. } => {
                let mut cloned = ctes.clone();
                self.analyze_query(subquery, &mut cloned, true);
            }
            Expr::Subquery(subquery) => {
                let mut cloned = ctes.clone();
                self.analyze_query(subquery, &mut cloned, false);
            }
            Expr::CompoundIdentifier(idents) if idents.len() >= 2 => {
                if !self.schema.is_empty() {
                    let alias = idents[idents.len() - 2].value.to_ascii_lowercase();
                    let col_ident = &idents[idents.len() - 1];
                    let col_lower = col_ident.value.to_ascii_lowercase();
                    if let Some(Some(tbl)) = scope.aliases.get(&alias)
                        && !tbl.columns.contains_key(&col_lower)
                    {
                        let (l, c) = Self::span_loc(col_ident.span);
                        let suggestion = SchemaCatalog::suggest_column(tbl, &col_lower);
                        let hint = match &suggestion {
                            Some(s) => format!(
                                "Did you mean '{alias}.{s}'? Available columns on '{}': {}",
                                tbl.name,
                                tbl.columns
                                    .values()
                                    .map(|col| col.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            None => format!(
                                "Available columns on '{}': {}",
                                tbl.name,
                                tbl.columns
                                    .values()
                                    .map(|col| col.name.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                        };
                        self.emit(
                            "SK_UNKNOWN_COLUMN",
                            l,
                            c,
                            format!(
                                "Column '{}.{}' does not exist on table '{}' (from {}).",
                                idents[idents.len() - 2].value,
                                col_ident.value,
                                tbl.name,
                                tbl.source
                            ),
                            hint,
                            suggestion,
                        );
                    }
                }
            }
            Expr::Identifier(ident) => {
                if !self.schema.is_empty()
                    && !scope.has_opaque_source
                    && !scope.aliases.is_empty()
                {
                    let col_lower = ident.value.to_ascii_lowercase();
                    if select_aliases.contains(&col_lower)
                        || is_sql_keyword_pseudo_col(&col_lower)
                    {
                        return;
                    }
                    let known_tables: Vec<&TableSchema> = scope
                        .aliases
                        .values()
                        .filter_map(|opt| *opt)
                        .collect();
                    let found = known_tables
                        .iter()
                        .any(|t| t.columns.contains_key(&col_lower));
                    if !found {
                        let (l, c) = Self::span_loc(ident.span);
                        let suggestion = known_tables
                            .iter()
                            .find_map(|t| SchemaCatalog::suggest_column(t, &col_lower));
                        let hint = match &suggestion {
                            Some(s) => format!("Did you mean '{s}'?"),
                            None => "Verify column spelling against your loaded contracts/DDL."
                                .to_string(),
                        };
                        self.emit(
                            "SK_UNKNOWN_COLUMN",
                            l,
                            c,
                            format!(
                                "Column '{}' does not exist on any table in FROM scope.",
                                ident.value
                            ),
                            hint,
                            suggestion,
                        );
                    }
                }
            }
            Expr::Nested(e)
            | Expr::UnaryOp { expr: e, .. }
            | Expr::IsNull(e)
            | Expr::IsNotNull(e)
            | Expr::IsTrue(e)
            | Expr::IsFalse(e)
            | Expr::Cast { expr: e, .. } => {
                self.analyze_expr(e, scope, ctes, select_aliases);
            }
            Expr::Between {
                expr: e, low, high, ..
            } => {
                self.analyze_expr(e, scope, ctes, select_aliases);
                self.analyze_expr(low, scope, ctes, select_aliases);
                self.analyze_expr(high, scope, ctes, select_aliases);
            }
            Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                if let Some(op) = operand {
                    self.analyze_expr(op, scope, ctes, select_aliases);
                }
                for cw in conditions {
                    self.analyze_expr(&cw.condition, scope, ctes, select_aliases);
                    self.analyze_expr(&cw.result, scope, ctes, select_aliases);
                }
                if let Some(el) = else_result {
                    self.analyze_expr(el, scope, ctes, select_aliases);
                }
            }
            Expr::Function(func) => {
                if let FunctionArguments::List(ref arg_list) = func.args {
                    for arg in &arg_list.args {
                        match arg {
                            FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                            | FunctionArg::Named {
                                arg: FunctionArgExpr::Expr(e),
                                ..
                            } => {
                                self.analyze_expr(e, scope, ctes, select_aliases);
                            }
                            _ => {}
                        }
                    }
                }
                if let Some(ref filter) = func.filter {
                    self.analyze_expr(filter, scope, ctes, select_aliases);
                }
            }
            _ => {}
        }
    }

    /// Check if `NOT IN (SELECT col FROM tbl ...)` is provably safe from NULLs.
    fn is_subquery_provably_non_null(&self, query: &Query, ctes: &HashSet<String>) -> bool {
        let SetExpr::Select(select) = &*query.body else {
            return false;
        };
        if select.projection.len() != 1 {
            return false;
        }
        let proj_expr = match &select.projection[0] {
            SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => e,
            _ => return false,
        };

        let proj_col_name = match proj_expr {
            Expr::Identifier(i) => i.value.to_ascii_lowercase(),
            Expr::CompoundIdentifier(ids) => ids
                .last()
                .map_or_else(String::new, |i| i.value.to_ascii_lowercase()),
            _ => return false,
        };

        // 1. Explicit `WHERE <col> IS NOT NULL` in the subquery makes it safe
        if let Some(ref sel) = select.selection
            && expr_asserts_col_not_null(sel, &proj_col_name)
        {
            return true;
        }

        // 2. If schema is loaded and the single non-left-joined table marks this column `nullable == false`
        if !self.schema.is_empty()
            && select.from.len() == 1
            && select.from[0].joins.is_empty()
            && let TableFactor::Table { name, .. } = &select.from[0].relation
        {
            let raw = name.to_string();
            let bare = normalize_table_ident(&raw);
            if !ctes.contains(&bare)
                && let Some(tbl) = self.schema.resolve_table(&raw)
                && let Some(col) = tbl.columns.get(&proj_col_name)
                && !col.nullable
            {
                return true;
            }
        }

        false
    }
}

fn is_null_literal(expr: &Expr) -> bool {
    matches!(expr, Expr::Value(v) if matches!(v.value, Value::Null))
}

fn is_sql_keyword_pseudo_col(s: &str) -> bool {
    matches!(
        s,
        "current_date" | "current_time" | "current_timestamp" | "current_user" | "null" | "true" | "false"
    )
}

fn expr_contains_is_null_for(expr: &Expr, aliases: &HashSet<String>) -> bool {
    match expr {
        Expr::IsNull(inner) => expr_references_any_alias(inner, aliases),
        Expr::Nested(inner) => expr_contains_is_null_for(inner, aliases),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Or | BinaryOperator::And,
            right,
        } => expr_contains_is_null_for(left, aliases) || expr_contains_is_null_for(right, aliases),
        _ => false,
    }
}

fn expr_references_any_alias(expr: &Expr, aliases: &HashSet<String>) -> bool {
    match expr {
        Expr::CompoundIdentifier(ids) if ids.len() >= 2 => {
            aliases.contains(&ids[ids.len() - 2].value.to_ascii_lowercase())
        }
        Expr::Nested(e) => expr_references_any_alias(e, aliases),
        _ => false,
    }
}

/// Returns the first `(alias, col, span)` from `left_aliases` that appears inside a NULL-rejecting predicate.
fn find_null_rejecting_left_ref(
    expr: &Expr,
    left_aliases: &HashSet<String>,
) -> Option<(String, String, Span)> {
    match expr {
        // Explicit NULL checks are safe (anti-join!)
        Expr::IsNull(_) | Expr::IsNotDistinctFrom(_, _) | Expr::IsDistinctFrom(_, _) => None,
        // COALESCE / NULLIF / CASE can handle NULLs intentionally — skip to avoid false positives
        Expr::Function(f) => {
            let fname = f.name.to_string().to_ascii_lowercase();
            if matches!(fname.as_str(), "coalesce" | "ifnull" | "nvl") {
                return None;
            }
            if let FunctionArguments::List(ref list) = f.args {
                for arg in &list.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                    | FunctionArg::Named {
                        arg: FunctionArgExpr::Expr(e),
                        ..
                    } = arg
                        && let Some(hit) = find_null_rejecting_left_ref(e, left_aliases)
                    {
                        return Some(hit);
                    }
                }
            }
            None
        }
        Expr::Case { .. } => None,
        Expr::CompoundIdentifier(ids) if ids.len() >= 2 => {
            let alias = ids[ids.len() - 2].value.to_ascii_lowercase();
            if left_aliases.contains(&alias) {
                let col = ids[ids.len() - 1].value.clone();
                Some((ids[ids.len() - 2].value.clone(), col, expr.span()))
            } else {
                None
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            find_null_rejecting_left_ref(left, left_aliases)
                .or_else(|| find_null_rejecting_left_ref(right, left_aliases))
        }
        Expr::Nested(e)
        | Expr::UnaryOp { expr: e, .. }
        | Expr::IsNotNull(e)
        | Expr::IsTrue(e)
        | Expr::IsFalse(e)
        | Expr::Cast { expr: e, .. } => find_null_rejecting_left_ref(e, left_aliases),
        Expr::Between {
            expr: e, low, high, ..
        } => find_null_rejecting_left_ref(e, left_aliases)
            .or_else(|| find_null_rejecting_left_ref(low, left_aliases))
            .or_else(|| find_null_rejecting_left_ref(high, left_aliases)),
        Expr::In { expr: e, .. } => find_null_rejecting_left_ref(e, left_aliases),
        Expr::Like { expr: e, .. } | Expr::ILike { expr: e, .. } => {
            find_null_rejecting_left_ref(e, left_aliases)
        }
        _ => None,
    }
}

fn expr_asserts_col_not_null(expr: &Expr, col_lower: &str) -> bool {
    match expr {
        Expr::IsNotNull(inner) => match &**inner {
            Expr::Identifier(i) => i.value.eq_ignore_ascii_case(col_lower),
            Expr::CompoundIdentifier(ids) => ids
                .last()
                .is_some_and(|i| i.value.eq_ignore_ascii_case(col_lower)),
            _ => false,
        },
        Expr::Nested(inner) => expr_asserts_col_not_null(inner, col_lower),
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => expr_asserts_col_not_null(left, col_lower) || expr_asserts_col_not_null(right, col_lower),
        _ => false,
    }
}

fn collect_referenced_tables_in_query(query: &Query, out: &mut HashSet<String>) {
    if let Some(ref with) = query.with {
        for cte in &with.cte_tables {
            collect_referenced_tables_in_query(&cte.query, out);
        }
    }
    collect_referenced_tables_in_set_expr(&query.body, out);
}

fn collect_referenced_tables_in_set_expr(set_expr: &SetExpr, out: &mut HashSet<String>) {
    match set_expr {
        SetExpr::Select(sel) => {
            for twj in &sel.from {
                collect_tables_in_twj(twj, out);
            }
            if let Some(ref w) = sel.selection {
                collect_tables_in_expr(w, out);
            }
            for item in &sel.projection {
                match item {
                    SelectItem::UnnamedExpr(e) | SelectItem::ExprWithAlias { expr: e, .. } => {
                        collect_tables_in_expr(e, out);
                    }
                    _ => {}
                }
            }
        }
        SetExpr::Query(q) => collect_referenced_tables_in_query(q, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_referenced_tables_in_set_expr(left, out);
            collect_referenced_tables_in_set_expr(right, out);
        }
        _ => {}
    }
}

fn collect_tables_in_twj(twj: &TableWithJoins, out: &mut HashSet<String>) {
    collect_tables_in_factor(&twj.relation, out);
    for join in &twj.joins {
        collect_tables_in_factor(&join.relation, out);
    }
}

fn collect_tables_in_factor(factor: &TableFactor, out: &mut HashSet<String>) {
    match factor {
        TableFactor::Table { name, .. } => {
            let norm = normalize_table_ident(&name.to_string());
            let bare = norm.rsplit_once('.').map_or(norm.as_str(), |(_, b)| b);
            out.insert(bare.to_string());
        }
        TableFactor::Derived { subquery, .. } => collect_referenced_tables_in_query(subquery, out),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => collect_tables_in_twj(table_with_joins, out),
        _ => {}
    }
}

fn collect_tables_in_expr(expr: &Expr, out: &mut HashSet<String>) {
    match expr {
        Expr::Subquery(q)
        | Expr::Exists { subquery: q, .. }
        | Expr::In {
            target: InTarget::Subquery(q),
            ..
        } => {
            collect_referenced_tables_in_query(q, out);
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_tables_in_expr(left, out);
            collect_tables_in_expr(right, out);
        }
        Expr::Nested(e) | Expr::UnaryOp { expr: e, .. } => collect_tables_in_expr(e, out),
        _ => {}
    }
}

fn extract_parse_error_loc(msg: &str) -> (usize, usize) {
    // sqlparser errors often include `at Line: X, Column: Y`
    let line = msg
        .split("Line:")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(1);
    let col = msg
        .split("Column:")
        .nth(1)
        .and_then(|s| s.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(1);
    (line, col)
}
