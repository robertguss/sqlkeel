use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

fn sqlkeel() -> Command {
    Command::cargo_bin("sqlkeel").expect("sqlkeel binary built")
}

fn example(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/seminary-sql")
        .join(rel)
}

#[test]
fn buggy_seminary_query_catches_left_join_not_in_hallucinated_col_and_unused_cte() {
    let out = sqlkeel()
        .args([
            "check",
            example("queries/at_risk_students_buggy.sql").to_str().unwrap(),
            "--schema",
            example("contracts").to_str().unwrap(),
            "--format",
            "agent",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(report["ok"], false);
    assert_eq!(report["schemas_loaded"], 2);

    let diags = report["diagnostics"].as_array().unwrap();
    let codes: Vec<&str> = diags.iter().map(|d| d["code"].as_str().unwrap()).collect();

    assert!(codes.contains(&"SK_LEFT_JOIN_WHERE_FILTER"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_NOT_IN_SUBQUERY"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_UNKNOWN_COLUMN"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_UNUSED_CTE"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_SELECT_STAR"), "codes: {codes:?}");

    // Verify fuzzy suggestion for `gpa_score` -> `gpa`
    let col_diag = diags
        .iter()
        .find(|d| d["code"] == "SK_UNKNOWN_COLUMN")
        .unwrap();
    assert_eq!(col_diag["suggestion"], "gpa");
}

#[test]
fn clean_seminary_query_passes_with_zero_errors_and_warnings() {
    let out = sqlkeel()
        .args([
            "check",
            example("queries/at_risk_students_clean.sql").to_str().unwrap(),
            "--schema",
            example("contracts").to_str().unwrap(),
            "--fail-on",
            "warning",
            "--format",
            "agent",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(report["ok"], true);
    assert_eq!(report["error_count"], 0);
    assert_eq!(report["warning_count"], 0);
}

#[test]
fn unsafe_postgres_migration_catches_all_locking_hazards() {
    let out = sqlkeel()
        .args([
            "check",
            example("migrations/002_unsafe_migration.sql").to_str().unwrap(),
            "--fail-on",
            "warning",
            "--format",
            "agent",
        ])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&out).unwrap();
    let codes: Vec<&str> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();

    assert!(codes.contains(&"SK_PG_CREATE_INDEX_LOCK"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_PG_ADD_COLUMN_NOT_NULL"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_PG_ALTER_COLUMN_TYPE"), "codes: {codes:?}");
    assert!(codes.contains(&"SK_PG_ADD_CONSTRAINT_LOCK"), "codes: {codes:?}");
}

#[test]
fn schema_grounding_with_ddl_catches_unknown_table_and_proves_safe_not_in() {
    let dir = tempdir().unwrap();
    let ddl = dir.path().join("schema.sql");
    fs::write(
        &ddl,
        "CREATE TABLE students (id BIGINT PRIMARY KEY, email TEXT NOT NULL, notes TEXT);",
    )
    .unwrap();

    // Safe: `id` is PRIMARY KEY (NOT NULL) so NOT IN (SELECT id FROM students) is proven safe!
    // Safe: anti-join `WHERE b.id IS NULL` is NOT flagged!
    sqlkeel()
        .args(["check", "-", "--schema", ddl.to_str().unwrap(), "--format", "agent"])
        .write_stdin(
            "SELECT a.id FROM students a LEFT JOIN students b ON a.id = b.id \
             WHERE b.id IS NULL AND a.id NOT IN (SELECT id FROM students);",
        )
        .assert()
        .success();

    // Hallucinated table `studentz` -> SK_UNKNOWN_TABLE with fuzzy suggestion `students`
    let out = sqlkeel()
        .args(["check", "-", "--schema", ddl.to_str().unwrap(), "--format", "agent"])
        .write_stdin("SELECT id FROM studentz;")
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let report: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(report["diagnostics"][0]["code"], "SK_UNKNOWN_TABLE");
    assert_eq!(report["diagnostics"][0]["suggestion"], "students");
}

#[test]
fn dml_safety_null_comparison_and_inline_suppressions() {
    let out = sqlkeel()
        .args(["check", "-", "--format", "agent"])
        .write_stdin(
            "DELETE FROM users;\n\
             UPDATE users SET active = false;\n\
             SELECT id FROM users WHERE deleted_at = NULL;\n\
             -- sqlkeel:ignore SK_SELECT_STAR\n\
             SELECT * FROM users;\n",
        )
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&out).unwrap();
    let codes: Vec<&str> = report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();

    assert!(codes.contains(&"SK_DELETE_WITHOUT_WHERE"));
    assert!(codes.contains(&"SK_UPDATE_WITHOUT_WHERE"));
    assert!(codes.contains(&"SK_NULL_COMPARISON"));
    assert!(
        !codes.contains(&"SK_SELECT_STAR"),
        "SK_SELECT_STAR should have been suppressed by inline comment"
    );
}

#[test]
fn rules_schema_skill_and_exit_2_errors() {
    let dir = tempdir().unwrap();

    sqlkeel()
        .args(["rules", "--format", "agent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SK_LEFT_JOIN_WHERE_FILTER"));

    sqlkeel()
        .args(["schema", example("contracts").to_str().unwrap(), "--format", "agent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("populi_students"))
        .stdout(predicate::str::contains("canvas_enrollments"));

    let skill_dir = dir.path().join(".agents/skills/sqlkeel");
    sqlkeel()
        .args(["skill", "--install", skill_dir.to_str().unwrap()])
        .assert()
        .success();
    assert!(skill_dir.join("SKILL.md").exists());

    // Unknown rule filter -> exit 2 with structured JSON error envelope on stderr
    let stderr = sqlkeel()
        .args([
            "check",
            example("queries/at_risk_students_clean.sql").to_str().unwrap(),
            "--ignore",
            "SK_DOES_NOT_EXIST",
            "--format",
            "agent",
        ])
        .assert()
        .code(2)
        .get_output()
        .stderr
        .clone();
    let env: Value = serde_json::from_slice(&stderr).expect("valid JSON error envelope");
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["code"], "SK_UNKNOWN_RULE");
}

#[test]
fn dbt_jinja_models_lint_cleanly_and_trace_multi_hop_column_blast_radius() {
    // 1. Check dbt/Jinja models grounded against ODCS/TOML contracts
    sqlkeel()
        .args([
            "check",
            example("models/int_student_risk.sql").to_str().unwrap(),
            "--schema",
            example("contracts").to_str().unwrap(),
            "--fail-on",
            "warning",
            "--format",
            "agent",
        ])
        .assert()
        .success();

    // 2. Trace 2-hop transitive column blast radius for `populi_students.gpa`
    //    populi_students.gpa -> int_student_risk.{student_gpa, risk_tier}
    //                       -> mart_advisor_outreach.{student_gpa, risk_tier, needs_immediate_outreach}
    let out = sqlkeel()
        .args([
            "lineage",
            example("models").to_str().unwrap(),
            "--schema",
            example("contracts").to_str().unwrap(),
            "--column",
            "populi_students.gpa",
            "--format",
            "agent",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let report: Value = serde_json::from_slice(&out).expect("valid JSON");
    assert_eq!(report["ok"], true);
    let br = &report["blast_radius"];
    assert_eq!(br["impacted_models"], 2);
    assert!(br["impacted_columns"].as_u64().unwrap() >= 4);

    let hits = br["hits"].as_array().unwrap();
    let hit_pairs: Vec<(String, String, u64)> = hits
        .iter()
        .map(|h| {
            (
                h["model"].as_str().unwrap().to_string(),
                h["output_column"].as_str().unwrap().to_string(),
                h["hop_depth"].as_u64().unwrap(),
            )
        })
        .collect();

    assert!(
        hit_pairs.contains(&("int_student_risk".to_string(), "student_gpa".to_string(), 1)),
        "hits: {hit_pairs:?}"
    );
    assert!(
        hit_pairs.contains(&("int_student_risk".to_string(), "risk_tier".to_string(), 1)),
        "hits: {hit_pairs:?}"
    );
    assert!(
        hit_pairs.contains(&(
            "mart_advisor_outreach".to_string(),
            "needs_immediate_outreach".to_string(),
            2
        )),
        "hits: {hit_pairs:?}"
    );

    // 3. Mermaid DAG rendering
    sqlkeel()
        .args([
            "lineage",
            example("models").to_str().unwrap(),
            "--mermaid",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("flowchart LR"))
        .stdout(predicate::str::contains("int_student_risk --> mart_advisor_outreach"));
}
