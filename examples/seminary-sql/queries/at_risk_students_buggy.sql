-- Buggy at-risk student query illustrating common silent SQL & AI-agent bugs
WITH unused_cohort AS (
    SELECT student_id FROM staging.populi_students
),
active_students AS (
    SELECT
        s.student_id,
        s.email,
        s.gpa_score,
        c.current_score,
        c.missing_assignments
    FROM staging.populi_students s
    LEFT JOIN staging.canvas_enrollments c
        ON s.student_id = c.sis_user_id
    WHERE s.status = 'active'
      AND c.current_score < 70
      AND s.email NOT IN (SELECT advisor_email FROM staging.populi_students)
)
SELECT *
FROM active_students;
