-- Explicit offline upgrade only, under the migration/server leases and transaction.
-- Preserve the OR lookup used by scoped proposal edits: the primary key finds
-- relational identities; this expression index also finds corrupt payload aliases.
-- Non-unique intentionally: conflicting aliases must remain visible to the reader
-- and be refused, never repaired, hidden or admitted by this performance upgrade.
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM communityhero.schema_migrations WHERE version=3) THEN
        RAISE EXCEPTION 'Feedback lookup index requires history schema v3';
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS feedback_payload_id_idx ON communityhero.feedback
    (workspace_id, (payload->>'id'));

INSERT INTO communityhero.schema_migrations(version) VALUES(4) ON CONFLICT(version) DO NOTHING;
