-- Complexity/size scoring for scheduling.
--
-- Jobs are scored at enqueue by otter-complexity. The scheduler orders runnable
-- work by intensity so short, simple tasks clear ahead of long ones, which
-- shortens average wait without changing what eventually runs.
--
-- All columns are nullable: jobs enqueued before this migration keep NULL and
-- the scheduler treats them as mid-intensity rather than jumping them to the
-- front or burying them.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS complexity SMALLINT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS task_size SMALLINT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS intensity SMALLINT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS complexity_band TEXT;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS estimated_minutes INTEGER;
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS assessment_confidence REAL;

-- Full assessment including the signals behind the score, so a surprising queue
-- position can always be explained after the fact.
ALTER TABLE jobs ADD COLUMN IF NOT EXISTS assessment JSONB;

-- The claim query filters on status and orders by (priority, intensity).
CREATE INDEX IF NOT EXISTS jobs_schedulable_idx
  ON jobs (status, priority ASC, intensity ASC, created_at ASC);
