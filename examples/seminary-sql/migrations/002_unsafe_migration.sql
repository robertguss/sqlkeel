-- Unsafe Postgres migration with table-locking DDL hazards
CREATE INDEX idx_canvas_sis_user_id ON staging.canvas_enrollments (sis_user_id);

ALTER TABLE staging.populi_students
    ADD COLUMN risk_tier text NOT NULL;

ALTER TABLE staging.populi_students
    ALTER COLUMN gpa TYPE numeric(4, 2);

ALTER TABLE staging.populi_students
    ADD CONSTRAINT chk_gpa_bounds CHECK (gpa >= 0.0 AND gpa <= 4.0);
