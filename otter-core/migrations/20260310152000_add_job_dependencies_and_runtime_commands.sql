ALTER TABLE jobs
ADD COLUMN IF NOT EXISTS project_path TEXT,
ADD COLUMN IF NOT EXISTS runtime_start_command TEXT,
ADD COLUMN IF NOT EXISTS runtime_stop_command TEXT,
ADD COLUMN IF NOT EXISTS runtime_command_cwd TEXT;

CREATE TABLE IF NOT EXISTS job_dependencies (
  job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  depends_on_job_id UUID NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  PRIMARY KEY (job_id, depends_on_job_id),
  CHECK (job_id <> depends_on_job_id)
);

CREATE INDEX IF NOT EXISTS job_dependencies_job_id_idx
  ON job_dependencies(job_id);

CREATE INDEX IF NOT EXISTS job_dependencies_depends_on_job_id_idx
  ON job_dependencies(depends_on_job_id);
