-- Clean, safe at-risk student retention query
WITH student_engagement AS (
    SELECT
        s.student_id,
        s.email,
        s.program,
        s.gpa,
        c.current_score,
        c.missing_assignments
    FROM staging.populi_students s
    LEFT JOIN staging.canvas_enrollments c
        ON s.student_id = c.sis_user_id
    WHERE s.status IN ('active', 'probation')
      AND (c.current_score < 70 OR c.enrollment_id IS NULL)
      AND NOT EXISTS (
          SELECT 1
          FROM staging.populi_students adv
          WHERE adv.advisor_email = s.email
      )
)
SELECT
    student_id,
    email,
    program,
    gpa,
    current_score,
    missing_assignments
FROM student_engagement;
