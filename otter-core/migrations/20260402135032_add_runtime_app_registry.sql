CREATE TABLE IF NOT EXISTS workspace_runtime_registry (
  workspace_id UUID PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
  container_name TEXT NOT NULL,
  image_tag TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('running', 'stopped', 'missing', 'unknown')),
  preferred_url TEXT,
  ports JSONB NOT NULL DEFAULT '[]'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS workspace_runtime_registry_status_idx
  ON workspace_runtime_registry(status);

CREATE TABLE IF NOT EXISTS job_runtime_app_registry (
  job_id UUID PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
  workspace_id UUID NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  working_directory TEXT,
  start_command TEXT NOT NULL,
  stop_command TEXT,
  status TEXT NOT NULL CHECK (status IN ('running', 'stopped', 'missing', 'unknown')),
  preferred_url TEXT,
  ports JSONB NOT NULL DEFAULT '[]'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS job_runtime_app_registry_status_idx
  ON job_runtime_app_registry(status);

