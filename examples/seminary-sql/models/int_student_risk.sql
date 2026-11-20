{{ config(materialized='view') }}
{# Intermediate student risk model combining Populi SIS and Canvas LMS #}
WITH joined AS (
    SELECT
        s.student_id,
        s.email,
        s.program,
        s.gpa AS student_gpa,
        s.advisor_email,
        c.current_score,
        c.missing_assignments
    FROM {{ source('staging', 'populi_students') }} s
    LEFT JOIN {{ source('staging', 'canvas_enrollments') }} c
        ON s.student_id = c.sis_user_id
    WHERE s.status IN ('active', 'probation')
)
SELECT
    student_id,
    email,
    program,
    student_gpa,
    advisor_email,
    CASE
        WHEN student_gpa < 2.5 OR current_score < 70 THEN 'high'
        WHEN missing_assignments >= 2 THEN 'medium'
        ELSE 'low'
    END AS risk_tier
FROM joined;
