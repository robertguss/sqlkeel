{{ config(materialized='table') }}
SELECT
    student_id,
    email,
    advisor_email,
    student_gpa,
    risk_tier,
    CASE WHEN risk_tier = 'high' THEN true ELSE false END AS needs_immediate_outreach
FROM {{ ref('int_student_risk') }}
WHERE risk_tier IN ('high', 'medium');
