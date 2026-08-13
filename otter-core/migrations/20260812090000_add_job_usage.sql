-- Per-job token accounting and cost estimation.
--
-- Kept in a separate table rather than as columns on `jobs` so the hot queue
-- and history queries keep their existing row shape, and so a job with no
-- recorded usage is representable as an absent row instead of ambiguous zeros.

CREATE TABLE IF NOT EXISTS job_usage (
  job_id UUID PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
  model TEXT,
  prompt_tokens BIGINT NOT NULL DEFAULT 0,
  completion_tokens BIGINT NOT NULL DEFAULT 0,
  total_tokens BIGINT NOT NULL DEFAULT 0,
  -- NULL means "no price configured for this model", never "free".
  estimated_cost_usd DOUBLE PRECISION,
  duration_ms BIGINT,
  created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS job_usage_created_at_idx ON job_usage(created_at DESC);
CREATE INDEX IF NOT EXISTS job_usage_model_idx ON job_usage(model);
