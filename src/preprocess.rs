use std::borrow::Cow;

/// Line-preserving Jinja / dbt preprocessor so `sqlparser` can analyze dbt models
/// and Jinja-templated SQL without needing `dbt compile` or a live warehouse,
/// while keeping every `file:line` diagnostic span 100% accurate.
pub fn preprocess_sql<'a>(sql: &'a str, model_name: &str) -> (Cow<'a, str>, bool) {
    if !sql.contains("{{") && !sql.contains("{%") && !sql.contains("{#") {
        return (Cow::Borrowed(sql), false);
    }

    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut out = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        if i + 1 < len && bytes[i] == b'{' {
            match bytes[i + 1] {
                // Jinja comment: {# ... #}
                b'#' => {
                    if let Some(end_rel) = sql[i + 2..].find("#}") {
                        let chunk = &sql[i..i + 2 + end_rel + 2];
                        push_preserving_newlines(&mut out, chunk);
                        i += chunk.len();
                        continue;
                    }
                }
                // Jinja statement block: {% ... %}
                b'%' => {
                    if let Some(end_rel) = sql[i + 2..].find("%}") {
                        let chunk = &sql[i..i + 2 + end_rel + 2];
                        push_preserving_newlines(&mut out, chunk);
                        i += chunk.len();
                        continue;
                    }
                }
                // Jinja expression: {{ ... }}
                b'{' => {
                    if let Some(end_rel) = sql[i + 2..].find("}}") {
                        let inner = sql[i + 2..i + 2 + end_rel].trim();
                        let chunk = &sql[i..i + 2 + end_rel + 2];
                        let replacement = translate_jinja_expr(inner, model_name);
                        out.push_str(&replacement);
                        // Preserve any newlines that existed inside a multi-line {{ ... }} block
                        for ch in chunk.chars() {
                            if ch == '\n' {
                                out.push('\n');
                            }
                        }
                        i += chunk.len();
                        continue;
                    }
                }
                _ => {}
            }
        }

        // Advance by one UTF-8 char
        let ch = sql[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    (Cow::Owned(out), true)
}

fn push_preserving_newlines(out: &mut String, chunk: &str) {
    for ch in chunk.chars() {
        if ch == '\n' {
            out.push('\n');
        } else {
            out.push(' ');
        }
    }
}

fn translate_jinja_expr(inner: &str, model_name: &str) -> String {
    let trimmed = inner.trim();

    // {{ config(...) }} -> no-op whitespace
    if starts_with_call(trimmed, "config") {
        return " ".to_string();
    }

    // {{ this }} -> current model identifier
    if trimmed == "this" {
        return sanitize_sql_ident(model_name);
    }

    // {{ ref('model') }} or {{ ref('pkg', 'model') }} -> model
    if let Some(args) = extract_call_args(trimmed, "ref") {
        let strings = extract_quoted_strings(args);
        if let Some(model) = strings.last() {
            return sanitize_sql_ident(model);
        }
    }

    // {{ source('schema', 'table') }} -> schema.table
    if let Some(args) = extract_call_args(trimmed, "source") {
        let strings = extract_quoted_strings(args);
        if strings.len() >= 2 {
            return format!(
                "{}.{}",
                sanitize_sql_ident(&strings[0]),
                sanitize_sql_ident(&strings[1])
            );
        } else if strings.len() == 1 {
            return sanitize_sql_ident(&strings[0]);
        }
    }

    // Fallback for {{ var(...) }}, {{ env_var(...) }}, macros -> valid SQL string literal
    "'__jinja_expr__'".to_string()
}

fn starts_with_call(s: &str, name: &str) -> bool {
    let Some(rest) = s.strip_prefix(name) else {
        return false;
    };
    rest.trim_start().starts_with('(')
}

fn extract_call_args<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(name)?.trim_start();
    let after_open = rest.strip_prefix('(')?;
    let close = after_open.rfind(')')?;
    Some(&after_open[..close])
}

fn extract_quoted_strings(args: &str) -> Vec<String> {
    let mut results = Vec::new();
    let mut chars = args.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' || ch == '"' {
            let quote = ch;
            let mut val = String::new();
            for c in chars.by_ref() {
                if c == quote {
                    break;
                }
                val.push(c);
            }
            results.push(val);
        }
    }
    results
}

fn sanitize_sql_ident(s: &str) -> String {
    let clean: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
        .collect();
    if clean.is_empty() {
        "__jinja_model__".to_string()
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_line_numbers_and_resolves_dbt_ref_and_source() {
        let input = "\
{{ config(materialized='view') }}
{# comment line #}
SELECT
    s.student_id,
    c.current_score
FROM {{ source('staging', 'populi_students') }} s
LEFT JOIN {{ ref('canvas_enrollments') }} c
    ON s.student_id = c.sis_user_id
{% if is_incremental() %}
WHERE c.last_activity_at >= {{ var('cutoff_date') }}
{% endif %};";

        let (processed, had_jinja) = preprocess_sql(input, "at_risk");
        assert!(had_jinja);
        assert_eq!(input.lines().count(), processed.lines().count());
        assert!(processed.contains("FROM staging.populi_students s"));
        assert!(processed.contains("LEFT JOIN canvas_enrollments c"));
        assert!(processed.contains("'__jinja_expr__'"));
    }

    #[test]
    fn fast_path_returns_borrowed_for_plain_sql() {
        let sql = "SELECT id FROM students;";
        let (out, had_jinja) = preprocess_sql(sql, "students");
        assert!(!had_jinja);
        assert!(matches!(out, Cow::Borrowed(_)));
    }
}
