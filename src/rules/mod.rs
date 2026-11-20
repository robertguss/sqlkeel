use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleCategory {
    Semantic,
    Schema,
    Safety,
    Migration,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuleMeta {
    pub code: &'static str,
    pub category: RuleCategory,
    pub default_severity: Severity,
    pub summary: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub category: RuleCategory,
    pub severity: Severity,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub message: String,
    pub fix_hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

pub const ALL_RULES: &[RuleMeta] = &[
    // Semantic analytics bugs
    RuleMeta {
        code: "SK_PARSE_ERROR",
        category: RuleCategory::Semantic,
        default_severity: Severity::Error,
        summary: "SQL syntax error reported by the dialect parser.",
    },
    RuleMeta {
        code: "SK_LEFT_JOIN_WHERE_FILTER",
        category: RuleCategory::Semantic,
        default_severity: Severity::Error,
        summary: "Filtering a LEFT JOIN right-side table in WHERE silently turns it into an INNER JOIN.",
    },
    RuleMeta {
        code: "SK_NOT_IN_SUBQUERY",
        category: RuleCategory::Semantic,
        default_severity: Severity::Error,
        summary: "NOT IN (SELECT ...) silently returns 0 rows if the subquery yields any NULL.",
    },
    RuleMeta {
        code: "SK_NULL_COMPARISON",
        category: RuleCategory::Semantic,
        default_severity: Severity::Error,
        summary: "Comparing with `= NULL`, `!= NULL`, or `<> NULL` always evaluates to UNKNOWN; use IS [NOT] NULL.",
    },
    RuleMeta {
        code: "SK_UNUSED_CTE",
        category: RuleCategory::Semantic,
        default_severity: Severity::Warning,
        summary: "A CTE defined in WITH is never referenced in the query.",
    },
    RuleMeta {
        code: "SK_SELECT_STAR",
        category: RuleCategory::Semantic,
        default_severity: Severity::Warning,
        summary: "SELECT * couples queries/views to upstream column drift; list columns explicitly.",
    },
    RuleMeta {
        code: "SK_CROSS_JOIN",
        category: RuleCategory::Semantic,
        default_severity: Severity::Warning,
        summary: "Implicit comma-join or unguarded CROSS JOIN can explode row counts.",
    },
    // Destructive DML safety
    RuleMeta {
        code: "SK_DELETE_WITHOUT_WHERE",
        category: RuleCategory::Safety,
        default_severity: Severity::Error,
        summary: "DELETE statement without a WHERE clause deletes every row in the table.",
    },
    RuleMeta {
        code: "SK_UPDATE_WITHOUT_WHERE",
        category: RuleCategory::Safety,
        default_severity: Severity::Error,
        summary: "UPDATE statement without a WHERE clause mutates every row in the table.",
    },
    // Schema-grounded hallucination detection (--schema)
    RuleMeta {
        code: "SK_UNKNOWN_TABLE",
        category: RuleCategory::Schema,
        default_severity: Severity::Error,
        summary: "Table referenced in SQL does not exist in the loaded ODCS/TOML/DDL schema catalog.",
    },
    RuleMeta {
        code: "SK_UNKNOWN_COLUMN",
        category: RuleCategory::Schema,
        default_severity: Severity::Error,
        summary: "Column referenced in SQL does not exist on the resolved table in the schema catalog.",
    },
    // PostgreSQL zero-downtime migration safety
    RuleMeta {
        code: "SK_PG_CREATE_INDEX_LOCK",
        category: RuleCategory::Migration,
        default_severity: Severity::Error,
        summary: "CREATE INDEX without CONCURRENTLY locks writes on the Postgres table.",
    },
    RuleMeta {
        code: "SK_PG_DROP_INDEX_LOCK",
        category: RuleCategory::Migration,
        default_severity: Severity::Warning,
        summary: "DROP INDEX without CONCURRENTLY acquires an ACCESS EXCLUSIVE lock in Postgres.",
    },
    RuleMeta {
        code: "SK_PG_ADD_COLUMN_NOT_NULL",
        category: RuleCategory::Migration,
        default_severity: Severity::Error,
        summary: "ALTER TABLE ADD COLUMN ... NOT NULL without a DEFAULT fails on non-empty tables.",
    },
    RuleMeta {
        code: "SK_PG_ALTER_COLUMN_TYPE",
        category: RuleCategory::Migration,
        default_severity: Severity::Warning,
        summary: "ALTER COLUMN TYPE rewrites the table under an ACCESS EXCLUSIVE lock in Postgres.",
    },
    RuleMeta {
        code: "SK_PG_ADD_CONSTRAINT_LOCK",
        category: RuleCategory::Migration,
        default_severity: Severity::Warning,
        summary: "ADD CONSTRAINT (CHECK / FOREIGN KEY) without NOT VALID scans the whole table under lock.",
    },
    RuleMeta {
        code: "SK_PG_DESTRUCTIVE_DDL",
        category: RuleCategory::Migration,
        default_severity: Severity::Warning,
        summary: "DROP TABLE or TRUNCATE permanently destroys data; review explicitly before running.",
    },
];

pub fn is_known_rule(code: &str) -> bool {
    ALL_RULES.iter().any(|r| r.code.eq_ignore_ascii_case(code))
}

/// Inline comment suppressions: `-- sqlkeel:ignore` or `-- sqlkeel:ignore SK_SELECT_STAR, SK_UNUSED_CTE`.
/// Applies to the same line AND the immediately following line.
#[derive(Debug, Default)]
pub struct Suppressions {
    /// 1-based line number -> None means suppress all rules; Some(set) means suppress listed rule codes.
    by_line: HashMap<usize, Option<HashSet<String>>>,
}

impl Suppressions {
    pub fn parse(sql: &str) -> Self {
        let mut by_line: HashMap<usize, Option<HashSet<String>>> = HashMap::new();
        for (idx, line) in sql.lines().enumerate() {
            let line_num = idx + 1;
            let lower = line.to_ascii_lowercase();
            let Some(pos) = lower.find("sqlkeel:ignore") else {
                continue;
            };
            let rest = line[pos + "sqlkeel:ignore".len()..].trim();
            let codes: HashSet<String> = rest
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.to_ascii_uppercase().starts_with("SK_"))
                .map(|s| s.to_ascii_uppercase())
                .collect();

            let entry = if codes.is_empty() { None } else { Some(codes) };
            // Suppress on this line and the next line (for comments placed directly above a statement)
            merge_suppression(&mut by_line, line_num, entry.clone());
            merge_suppression(&mut by_line, line_num + 1, entry);
        }
        Self { by_line }
    }

    pub fn is_suppressed(&self, line: usize, code: &str) -> bool {
        match self.by_line.get(&line) {
            None => false,
            Some(None) => true,
            Some(Some(set)) => set.contains(&code.to_ascii_uppercase()),
        }
    }
}

fn merge_suppression(
    map: &mut HashMap<usize, Option<HashSet<String>>>,
    line: usize,
    incoming: Option<HashSet<String>>,
) {
    match (map.get_mut(&line), incoming) {
        (Some(existing), None) => {
            *existing = None;
        }
        (Some(Some(existing_set)), Some(new_set)) => {
            existing_set.extend(new_set);
        }
        (Some(None), Some(_)) => {}
        (None, inc) => {
            map.insert(line, inc);
        }
    }
}
