-- Monotonic cursor for the SSE event stream.
--
-- The stream previously paged with `WHERE created_at > $1`, advancing the cursor
-- to the last row's timestamp. `created_at` is not unique: two events written in
-- the same microsecond share a timestamp, so the strict `>` comparison silently
-- skipped every tied row after the first. Under `output_chunk` bursts that drops
-- terminal lines from the UI.
--
-- A dedicated sequence gives the stream a unique, strictly increasing cursor.
ALTER TABLE job_events ADD COLUMN IF NOT EXISTS seq BIGSERIAL;

CREATE UNIQUE INDEX IF NOT EXISTS job_events_seq_idx ON job_events(seq);
CREATE INDEX IF NOT EXISTS job_events_job_id_seq_idx ON job_events(job_id, seq);
