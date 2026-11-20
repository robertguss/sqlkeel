use crate::analyzer::collect_sql_inputs;
use crate::cli::DialectArg;
use crate::error::{Result, SqlKeelError};
use crate::preprocess::preprocess_sql;
use crate::schema::{SchemaCatalog, make_dialect, normalize_table_ident};
use indexmap::{IndexMap, IndexSet};
use rayon::prelude::*;
use serde::Serialize;
use sqlparser::ast::{
    Expr, FunctionArg, FunctionArgExpr, FunctionArguments, InTarget, Query, Select, SelectItem,
    SetExpr, Spanned, Statement, TableFactor, TableWithJoins,
};
use sqlparser::parser::Parser;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ColumnRef {
    pub table: String,
    pub column: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnLineage {
    pub name: String,
    pub expression: String,
    pub line: usize,
    pub sources: Vec<ColumnRef>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelLineage {
    pub model: String,
    pub file: String,
    pub had_jinja: bool,
    pub upstream_tables: Vec<String>,
    pub columns: Vec<ColumnLineage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImpactHit {
    pub model: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_column: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub hop_depth: usize,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlastRadius {
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    pub impacted_models: usize,
    pub impacted_columns: usize,
    pub hits: Vec<ImpactHit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineageReport {
    pub schema_version: &'static str,
    pub command: &'static str,
    pub files_analyzed: usize,
    pub models_count: usize,
    pub edge_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blast_radius: Option<BlastRadius>,
    pub models: Vec<ModelLineage>,
    pub edges: Vec<LineageEdge>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
}

pub fn analyze_lineage(
    paths: &[String],
    schema: &SchemaCatalog,
    dialect: DialectArg,
    target_table: Option<&str>,
    target_column: Option<&str>,
) -> Result<LineageReport> {
    let inputs = collect_sql_inputs(paths)?;
    if inputs.is_empty() {
        return Err(SqlKeelError::NoSqlFiles);
    }

    let files_analyzed = inputs.len();
    let mut models: Vec<ModelLineage> = inputs
        .par_iter()
        .flat_map(|(file, sql)| extract_file_lineage(file, sql, schema, dialect))
        .collect();

    models.sort_by(|a, b| a.model.cmp(&b.model).then_with(|| a.file.cmp(&b.file)));

    // Build deduplicated table-level DAG edges (upstream -> downstream model)
    let mut edge_set: IndexSet<LineageEdge> = IndexSet::new();
    for m in &models {
        for up in &m.upstream_tables {
            edge_set.insert(LineageEdge {
                from: up.clone(),
                to: m.model.clone(),
            });
        }
    }
    let edges: Vec<LineageEdge> = edge_set.into_iter().collect();

    let blast_radius = if let Some(col_spec) = target_column {
        Some(compute_column_blast_radius(&models, col_spec)?)
    } else {
        target_table.map(|tbl_spec| compute_table_blast_radius(&models, tbl_spec))
    };

    Ok(LineageReport {
        schema_version: "1.0",
        command: "lineage",
        files_analyzed,
        models_count: models.len(),
        edge_count: edges.len(),
        blast_radius,
        models,
        edges,
    })
}

pub fn render_mermaid(report: &LineageReport) -> String {
    let mut out = String::from("flowchart LR\n");
    let mut nodes: IndexSet<String> = IndexSet::new();
    for edge in &report.edges {
        nodes.insert(edge.from.clone());
        nodes.insert(edge.to.clone());
    }
    for m in &report.models {
        nodes.insert(m.model.clone());
    }
    for n in &nodes {
        let id = mermaid_id(n);
        out.push_str(&format!("    {id}[\"{n}\"]\n"));
    }
    for edge in &report.edges {
        out.push_str(&format!(
            "    {} --> {}\n",
            mermaid_id(&edge.from),
            mermaid_id(&edge.to)
        ));
    }
    out
}

fn mermaid_id(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn extract_file_lineage(
    file: &str,
    sql: &str,
    schema: &SchemaCatalog,
    dialect_arg: DialectArg,
) -> Vec<ModelLineage> {
    let default_model = derive_model_name(file);
    let (processed, had_jinja) = preprocess_sql(sql, &default_model);
    let dialect = make_dialect(dialect_arg);

    let stmts = match Parser::parse_sql(dialect.as_ref(), &processed) {
        Ok(s) => s,
        Err(e) => {
            return vec![ModelLineage {
                model: default_model,
                file: file.to_string(),
                had_jinja,
                upstream_tables: vec![],
                columns: vec![],
                parse_error: Some(e.to_string()),
            }];
        }
    };

    let mut out = Vec::new();
    for stmt in &stmts {
        match stmt {
            Statement::CreateView(cv) => {
                let name = normalize_table_ident(&cv.name.to_string());
                out.push(extract_query_lineage(
                    name, file, had_jinja, &cv.query, schema,
                ));
            }
            Statement::CreateTable(ct) => {
                if let Some(ref query) = ct.query {
                    let name = normalize_table_ident(&ct.name.to_string());
                    out.push(extract_query_lineage(
                        name, file, had_jinja, query, schema,
                    ));
                }
            }
            Statement::Insert(ins) => {
                if let Some(ref query) = ins.source {
                    let name = normalize_table_ident(&ins.table.to_string());
                    out.push(extract_query_lineage(
                        name, file, had_jinja, query, schema,
                    ));
                }
            }
            Statement::Query(query) => {
                out.push(extract_query_lineage(
                    default_model.clone(),
                    file,
                    had_jinja,
                    query,
                    schema,
                ));
            }
            _ => {}
        }
    }

    out
}

/// Maps CTE name -> (output_col_lower -> Vec<ColumnRef> resolved all the way to physical tables)
type CteLineageMap = HashMap<String, IndexMap<String, Vec<ColumnRef>>>;

fn extract_query_lineage(
    model: String,
    file: &str,
    had_jinja: bool,
    query: &Query,
    schema: &SchemaCatalog,
) -> ModelLineage {
    let mut upstream_tables: IndexSet<String> = IndexSet::new();
    let mut cte_map: CteLineageMap = HashMap::new();

    if let Some(ref with) = query.with {
        for cte in &with.cte_tables {
            let cte_name = cte.alias.name.value.to_ascii_lowercase();
            let cte_cols = resolve_query_columns(
                &cte.query,
                &cte_map,
                schema,
                &mut upstream_tables,
            );
            let mut col_lookup: IndexMap<String, Vec<ColumnRef>> = IndexMap::new();
            for col in cte_cols {
                col_lookup.insert(col.name.to_ascii_lowercase(), col.sources);
            }
            cte_map.insert(cte_name, col_lookup);
        }
    }

    let columns = resolve_set_expr_columns(&query.body, &cte_map, schema, &mut upstream_tables);

    // Exclude self-references if any
    upstream_tables.shift_remove(&model);

    ModelLineage {
        model,
        file: file.to_string(),
        had_jinja,
        upstream_tables: upstream_tables.into_iter().collect(),
        columns,
        parse_error: None,
    }
}

fn resolve_query_columns(
    query: &Query,
    outer_ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    upstream_tables: &mut IndexSet<String>,
) -> Vec<ColumnLineage> {
    let mut local_ctes = outer_ctes.clone();
    if let Some(ref with) = query.with {
        for cte in &with.cte_tables {
            let cte_name = cte.alias.name.value.to_ascii_lowercase();
            let cols = resolve_query_columns(&cte.query, &local_ctes, schema, upstream_tables);
            let mut map = IndexMap::new();
            for c in cols {
                map.insert(c.name.to_ascii_lowercase(), c.sources);
            }
            local_ctes.insert(cte_name, map);
        }
    }
    resolve_set_expr_columns(&query.body, &local_ctes, schema, upstream_tables)
}

fn resolve_set_expr_columns(
    set_expr: &SetExpr,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    upstream_tables: &mut IndexSet<String>,
) -> Vec<ColumnLineage> {
    match set_expr {
        SetExpr::Select(select) => resolve_select_columns(select, ctes, schema, upstream_tables),
        SetExpr::Query(q) => resolve_query_columns(q, ctes, schema, upstream_tables),
        SetExpr::SetOperation { left, right, .. } => {
            let mut left_cols = resolve_set_expr_columns(left, ctes, schema, upstream_tables);
            let right_cols = resolve_set_expr_columns(right, ctes, schema, upstream_tables);
            // Union combines column sources positionally
            for (l, r) in left_cols.iter_mut().zip(right_cols) {
                for src in r.sources {
                    if !l.sources.contains(&src) {
                        l.sources.push(src);
                    }
                }
            }
            left_cols
        }
        _ => Vec::new(),
    }
}

enum SourceBinding {
    PhysicalTable { canonical_name: String },
    Cte { name: String },
    Subquery { cols: IndexMap<String, Vec<ColumnRef>> },
}

fn resolve_select_columns(
    select: &Select,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    upstream_tables: &mut IndexSet<String>,
) -> Vec<ColumnLineage> {
    let mut bindings: IndexMap<String, SourceBinding> = IndexMap::new();

    for twj in &select.from {
        bind_twj(twj, ctes, schema, upstream_tables, &mut bindings);
    }

    // Also harvest upstream tables from WHERE subqueries (for table-level lineage accuracy!)
    if let Some(ref sel) = select.selection {
        harvest_expr_tables(sel, ctes, schema, upstream_tables);
    }

    let mut out = Vec::new();
    for item in &select.projection {
        let line = item.span().start.line.max(1) as usize;
        match item {
            SelectItem::UnnamedExpr(expr) => {
                harvest_expr_tables(expr, ctes, schema, upstream_tables);
                let name = infer_expr_output_name(expr);
                let sources = resolve_expr_column_refs(expr, &bindings, ctes, schema);
                out.push(ColumnLineage {
                    name,
                    expression: expr.to_string(),
                    line,
                    sources,
                });
            }
            SelectItem::ExprWithAlias { expr, alias } => {
                harvest_expr_tables(expr, ctes, schema, upstream_tables);
                let sources = resolve_expr_column_refs(expr, &bindings, ctes, schema);
                out.push(ColumnLineage {
                    name: alias.value.clone(),
                    expression: expr.to_string(),
                    line,
                    sources,
                });
            }
            SelectItem::ExprWithAliases { expr, aliases } => {
                harvest_expr_tables(expr, ctes, schema, upstream_tables);
                let sources = resolve_expr_column_refs(expr, &bindings, ctes, schema);
                for alias in aliases {
                    out.push(ColumnLineage {
                        name: alias.value.clone(),
                        expression: expr.to_string(),
                        line,
                        sources: sources.clone(),
                    });
                }
            }
            SelectItem::Wildcard(_) => {
                expand_wildcard(None, line, &bindings, ctes, schema, &mut out);
            }
            SelectItem::QualifiedWildcard(kind, _) => {
                let alias = kind.to_string().trim_end_matches(".*").to_ascii_lowercase();
                expand_wildcard(Some(&alias), line, &bindings, ctes, schema, &mut out);
            }
        }
    }

    out
}

fn expand_wildcard(
    only_alias: Option<&str>,
    line: usize,
    bindings: &IndexMap<String, SourceBinding>,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    out: &mut Vec<ColumnLineage>,
) {
    let mut expanded_any = false;
    for (alias, binding) in bindings {
        if let Some(target) = only_alias
            && alias != target
        {
            continue;
        }
        match binding {
            SourceBinding::Cte { name } => {
                if let Some(cte_cols) = ctes.get(name) {
                    for (col_name, srcs) in cte_cols {
                        out.push(ColumnLineage {
                            name: col_name.clone(),
                            expression: format!("{alias}.{col_name}"),
                            line,
                            sources: srcs.clone(),
                        });
                        expanded_any = true;
                    }
                }
            }
            SourceBinding::Subquery { cols } => {
                for (col_name, srcs) in cols {
                    out.push(ColumnLineage {
                        name: col_name.clone(),
                        expression: format!("{alias}.{col_name}"),
                        line,
                        sources: srcs.clone(),
                    });
                    expanded_any = true;
                }
            }
            SourceBinding::PhysicalTable { canonical_name } => {
                if let Some(tbl) = schema.resolve_table(canonical_name) {
                    for col in tbl.columns.values() {
                        out.push(ColumnLineage {
                            name: col.name.clone(),
                            expression: format!("{alias}.{}", col.name),
                            line,
                            sources: vec![ColumnRef {
                                table: canonical_name.clone(),
                                column: col.name.to_ascii_lowercase(),
                            }],
                        });
                        expanded_any = true;
                    }
                }
            }
        }
    }

    if !expanded_any {
        out.push(ColumnLineage {
            name: "*".to_string(),
            expression: only_alias.map_or_else(|| "*".to_string(), |a| format!("{a}.*")),
            line,
            sources: Vec::new(),
        });
    }
}

fn bind_twj(
    twj: &TableWithJoins,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    upstream_tables: &mut IndexSet<String>,
    bindings: &mut IndexMap<String, SourceBinding>,
) {
    bind_factor(&twj.relation, ctes, schema, upstream_tables, bindings);
    for join in &twj.joins {
        bind_factor(&join.relation, ctes, schema, upstream_tables, bindings);
    }
}

fn bind_factor(
    factor: &TableFactor,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    upstream_tables: &mut IndexSet<String>,
    bindings: &mut IndexMap<String, SourceBinding>,
) {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            let norm = normalize_table_ident(&name.to_string());
            let bare = norm.rsplit_once('.').map_or(norm.as_str(), |(_, b)| b);
            let alias_key = alias
                .as_ref()
                .map_or_else(|| bare.to_string(), |a| a.name.value.to_ascii_lowercase());

            if ctes.contains_key(bare) {
                bindings.insert(
                    alias_key,
                    SourceBinding::Cte {
                        name: bare.to_string(),
                    },
                );
            } else {
                // Normalize against schema if known
                let canonical = schema
                    .resolve_table(&norm)
                    .map(|t| match &t.schema_name {
                        Some(s) => format!("{}.{}", s.to_ascii_lowercase(), t.name.to_ascii_lowercase()),
                        None => t.name.to_ascii_lowercase(),
                    })
                    .unwrap_or(norm);
                upstream_tables.insert(canonical.clone());
                bindings.insert(
                    alias_key,
                    SourceBinding::PhysicalTable {
                        canonical_name: canonical,
                    },
                );
            }
        }
        TableFactor::Derived {
            subquery, alias, ..
        } => {
            let sub_cols = resolve_query_columns(subquery, ctes, schema, upstream_tables);
            let mut map = IndexMap::new();
            for c in sub_cols {
                map.insert(c.name.to_ascii_lowercase(), c.sources);
            }
            let alias_key = alias
                .as_ref()
                .map_or_else(|| "__subquery__".to_string(), |a| a.name.value.to_ascii_lowercase());
            bindings.insert(alias_key, SourceBinding::Subquery { cols: map });
        }
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => bind_twj(table_with_joins, ctes, schema, upstream_tables, bindings),
        _ => {}
    }
}

fn resolve_expr_column_refs(
    expr: &Expr,
    bindings: &IndexMap<String, SourceBinding>,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
) -> Vec<ColumnRef> {
    let mut set: IndexSet<ColumnRef> = IndexSet::new();
    collect_expr_column_refs(expr, bindings, ctes, schema, &mut set);
    set.into_iter().collect()
}

fn collect_expr_column_refs(
    expr: &Expr,
    bindings: &IndexMap<String, SourceBinding>,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    out: &mut IndexSet<ColumnRef>,
) {
    match expr {
        Expr::CompoundIdentifier(ids) if ids.len() >= 2 => {
            let alias = ids[ids.len() - 2].value.to_ascii_lowercase();
            let col = ids[ids.len() - 1].value.to_ascii_lowercase();
            if let Some(binding) = bindings.get(&alias) {
                resolve_from_binding(binding, &col, ctes, out);
            } else {
                out.insert(ColumnRef {
                    table: alias,
                    column: col,
                });
            }
        }
        Expr::Identifier(ident) => {
            let col = ident.value.to_ascii_lowercase();
            if matches!(
                col.as_str(),
                "current_date" | "current_time" | "current_timestamp" | "null" | "true" | "false"
            ) {
                return;
            }
            // Try exact resolution against CTEs / schema tables / single binding
            let mut matched = false;
            for binding in bindings.values() {
                match binding {
                    SourceBinding::Cte { name } => {
                        if let Some(cols) = ctes.get(name)
                            && let Some(srcs) = cols.get(&col)
                        {
                            for s in srcs {
                                out.insert(s.clone());
                            }
                            matched = true;
                        }
                    }
                    SourceBinding::Subquery { cols } => {
                        if let Some(srcs) = cols.get(&col) {
                            for s in srcs {
                                out.insert(s.clone());
                            }
                            matched = true;
                        }
                    }
                    SourceBinding::PhysicalTable { canonical_name } => {
                        if let Some(tbl) = schema.resolve_table(canonical_name)
                            && tbl.columns.contains_key(&col)
                        {
                            out.insert(ColumnRef {
                                table: canonical_name.clone(),
                                column: col.clone(),
                            });
                            matched = true;
                        }
                    }
                }
            }
            if !matched
                && bindings.len() == 1
                && let Some((_, only)) = bindings.first()
            {
                resolve_from_binding(only, &col, ctes, out);
            }
        }
        Expr::BinaryOp { left, right, .. } => {
            collect_expr_column_refs(left, bindings, ctes, schema, out);
            collect_expr_column_refs(right, bindings, ctes, schema, out);
        }
        Expr::Nested(e)
        | Expr::UnaryOp { expr: e, .. }
        | Expr::Cast { expr: e, .. }
        | Expr::IsNull(e)
        | Expr::IsNotNull(e) => collect_expr_column_refs(e, bindings, ctes, schema, out),
        Expr::Between {
            expr: e, low, high, ..
        } => {
            collect_expr_column_refs(e, bindings, ctes, schema, out);
            collect_expr_column_refs(low, bindings, ctes, schema, out);
            collect_expr_column_refs(high, bindings, ctes, schema, out);
        }
        Expr::Case {
            operand,
            conditions,
            else_result,
            ..
        } => {
            if let Some(op) = operand {
                collect_expr_column_refs(op, bindings, ctes, schema, out);
            }
            for cw in conditions {
                collect_expr_column_refs(&cw.condition, bindings, ctes, schema, out);
                collect_expr_column_refs(&cw.result, bindings, ctes, schema, out);
            }
            if let Some(el) = else_result {
                collect_expr_column_refs(el, bindings, ctes, schema, out);
            }
        }
        Expr::Function(func) => {
            if let FunctionArguments::List(ref list) = func.args {
                for arg in &list.args {
                    if let FunctionArg::Unnamed(FunctionArgExpr::Expr(e))
                    | FunctionArg::Named {
                        arg: FunctionArgExpr::Expr(e),
                        ..
                    } = arg
                    {
                        collect_expr_column_refs(e, bindings, ctes, schema, out);
                    }
                }
            }
            if let Some(ref filter) = func.filter {
                collect_expr_column_refs(filter, bindings, ctes, schema, out);
            }
        }
        _ => {}
    }
}

fn resolve_from_binding(
    binding: &SourceBinding,
    col: &str,
    ctes: &CteLineageMap,
    out: &mut IndexSet<ColumnRef>,
) {
    match binding {
        SourceBinding::PhysicalTable { canonical_name } => {
            out.insert(ColumnRef {
                table: canonical_name.clone(),
                column: col.to_string(),
            });
        }
        SourceBinding::Cte { name } => {
            if let Some(cte_cols) = ctes.get(name)
                && let Some(srcs) = cte_cols.get(col)
            {
                for s in srcs {
                    out.insert(s.clone());
                }
            }
        }
        SourceBinding::Subquery { cols } => {
            if let Some(srcs) = cols.get(col) {
                for s in srcs {
                    out.insert(s.clone());
                }
            }
        }
    }
}

fn harvest_expr_tables(
    expr: &Expr,
    ctes: &CteLineageMap,
    schema: &SchemaCatalog,
    upstream_tables: &mut IndexSet<String>,
) {
    match expr {
        Expr::Subquery(q)
        | Expr::Exists { subquery: q, .. }
        | Expr::In {
            target: InTarget::Subquery(q),
            ..
        } => {
            let _ = resolve_query_columns(q, ctes, schema, upstream_tables);
        }
        Expr::BinaryOp { left, right, .. } => {
            harvest_expr_tables(left, ctes, schema, upstream_tables);
            harvest_expr_tables(right, ctes, schema, upstream_tables);
        }
        Expr::Nested(e) | Expr::UnaryOp { expr: e, .. } => {
            harvest_expr_tables(e, ctes, schema, upstream_tables);
        }
        _ => {}
    }
}

fn infer_expr_output_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(i) => i.value.clone(),
        Expr::CompoundIdentifier(ids) => ids
            .last()
            .map_or_else(|| expr.to_string(), |i| i.value.clone()),
        Expr::Function(f) => f.name.to_string().to_ascii_lowercase(),
        _ => expr.to_string(),
    }
}

fn derive_model_name(file: &str) -> String {
    if file == "<stdin>" {
        return "stdin_query".to_string();
    }
    Path::new(file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("query")
        .to_ascii_lowercase()
}

fn table_matches(candidate: &str, needle: &str) -> bool {
    let c = normalize_table_ident(candidate);
    let n = normalize_table_ident(needle);
    if c == n {
        return true;
    }
    let c_bare = c.rsplit_once('.').map_or(c.as_str(), |(_, b)| b);
    let n_bare = n.rsplit_once('.').map_or(n.as_str(), |(_, b)| b);
    c_bare == n_bare
}

fn compute_table_blast_radius(models: &[ModelLineage], target: &str) -> BlastRadius {
    let norm_target = normalize_table_ident(target);
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<(String, usize, Vec<String>)> = VecDeque::new();
    queue.push_back((norm_target.clone(), 0, vec![norm_target.clone()]));
    visited.insert(norm_target.clone());

    let mut hits: Vec<ImpactHit> = Vec::new();

    while let Some((curr, depth, path)) = queue.pop_front() {
        for m in models {
            let depends = m.upstream_tables.iter().any(|u| table_matches(u, &curr));
            if depends && !visited.contains(&m.model) {
                visited.insert(m.model.clone());
                let mut next_path = path.clone();
                next_path.push(m.model.clone());
                hits.push(ImpactHit {
                    model: m.model.clone(),
                    file: m.file.clone(),
                    output_column: None,
                    line: None,
                    hop_depth: depth + 1,
                    path: next_path.clone(),
                });
                queue.push_back((m.model.clone(), depth + 1, next_path));
            }
        }
    }

    let impacted_models = hits.len();
    BlastRadius {
        target: norm_target,
        column: None,
        impacted_models,
        impacted_columns: 0,
        hits,
    }
}

fn compute_column_blast_radius(models: &[ModelLineage], col_spec: &str) -> Result<BlastRadius> {
    let (raw_table, raw_col) = col_spec.rsplit_once('.').ok_or_else(|| {
        SqlKeelError::General(format!(
            "Invalid `--column` value '{col_spec}': expected `table.column` (e.g. `populi_students.gpa`)"
        ))
    })?;
    let target_table = normalize_table_ident(raw_table);
    let target_col = raw_col.trim().to_ascii_lowercase();
    if target_table.is_empty() || target_col.is_empty() {
        return Err(SqlKeelError::General(format!(
            "Invalid `--column` value '{col_spec}': expected `table.column`"
        )));
    }

    let mut visited: HashSet<(String, String)> = HashSet::new();
    let mut queue: VecDeque<(String, String, usize, Vec<String>)> = VecDeque::new();
    let seed = format!("{target_table}.{target_col}");
    visited.insert((target_table.clone(), target_col.clone()));
    queue.push_back((target_table.clone(), target_col.clone(), 0, vec![seed]));

    let mut hits: Vec<ImpactHit> = Vec::new();
    let mut impacted_models: HashSet<String> = HashSet::new();

    while let Some((curr_tbl, curr_col, depth, path)) = queue.pop_front() {
        for m in models {
            for out_col in &m.columns {
                let uses_col = out_col
                    .sources
                    .iter()
                    .any(|s| table_matches(&s.table, &curr_tbl) && s.column.eq_ignore_ascii_case(&curr_col));
                if uses_col {
                    let out_col_lower = out_col.name.to_ascii_lowercase();
                    let key = (m.model.clone(), out_col_lower.clone());
                    if visited.insert(key) {
                        let mut next_path = path.clone();
                        next_path.push(format!("{}.{}", m.model, out_col.name));
                        impacted_models.insert(m.model.clone());
                        hits.push(ImpactHit {
                            model: m.model.clone(),
                            file: m.file.clone(),
                            output_column: Some(out_col.name.clone()),
                            line: Some(out_col.line),
                            hop_depth: depth + 1,
                            path: next_path.clone(),
                        });
                        queue.push_back((m.model.clone(), out_col_lower, depth + 1, next_path));
                    }
                }
            }
        }
    }

    Ok(BlastRadius {
        target: target_table,
        column: Some(target_col),
        impacted_models: impacted_models.len(),
        impacted_columns: hits.len(),
        hits,
    })
}
