-- Explicit offline upgrade only. The runner owns both leases and a transaction.
-- Do not normalize old history: incompatible existing rows fail validation and
-- roll the entire migration back. Legacy approval shapes/statuses remain valid.
DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM communityhero.schema_migrations WHERE version=2) THEN
        RAISE EXCEPTION 'History guards require knowledge schema v2';
    END IF;
END $$;

CREATE OR REPLACE FUNCTION communityhero.guard_approval_binding()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Approval history cannot be deleted';
    END IF;
    -- Only status is mutable. This also freezes unknown/future context and
    -- authority fields, identity, workspace and ordinal, not just known keys.
    IF (to_jsonb(NEW) - 'payload' - 'status') IS DISTINCT FROM
       (to_jsonb(OLD) - 'payload' - 'status') OR
       (NEW.payload - 'status') IS DISTINCT FROM (OLD.payload - 'status') THEN
        RAISE EXCEPTION 'Approval identity, context and authority are immutable';
    END IF;
    RETURN NEW;
END $$;

DO $$ BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid='communityhero.audit'::regclass AND conname='audit_payload_projection') THEN
        ALTER TABLE communityhero.audit ADD CONSTRAINT audit_payload_projection CHECK (
            jsonb_typeof(payload->'id') IS NOT DISTINCT FROM 'string' AND
            length(btrim(payload->>'id')) > 0 AND
            id IS NOT DISTINCT FROM payload->>'id' AND
            (payload->'action' IS NULL OR jsonb_typeof(payload->'action') IN ('string','null')) AND
            action IS NOT DISTINCT FROM payload->>'action' AND
            (payload->'refId' IS NULL OR jsonb_typeof(payload->'refId') IN ('string','null')) AND
            ref_id IS NOT DISTINCT FROM payload->>'refId'
        ) NOT VALID;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conrelid='communityhero.approvals'::regclass AND conname='approvals_payload_projection') THEN
        ALTER TABLE communityhero.approvals ADD CONSTRAINT approvals_payload_projection CHECK (
            jsonb_typeof(payload->'id') IS NOT DISTINCT FROM 'string' AND
            length(btrim(payload->>'id')) > 0 AND
            id IS NOT DISTINCT FROM payload->>'id' AND
            (payload->'status' IS NULL OR jsonb_typeof(payload->'status') IN ('string','null')) AND
            status IS NOT DISTINCT FROM payload->>'status'
        ) NOT VALID;
    END IF;
END $$;

ALTER TABLE communityhero.audit VALIDATE CONSTRAINT audit_payload_projection;
ALTER TABLE communityhero.approvals VALIDATE CONSTRAINT approvals_payload_projection;

-- Route-scoped source admissions use these two payload keys; the workspace and
-- ordinal constraints and jobs_status_idx already support the other hot writes.
CREATE INDEX IF NOT EXISTS items_provider_route_idx ON communityhero.items
    (workspace_id,(payload->>'objectId'),(payload->>'itemId'));

-- CREATE OR REPLACE TRIGGER makes explicit offline reapplication idempotent.
CREATE OR REPLACE TRIGGER audit_immutable
BEFORE UPDATE OR DELETE ON communityhero.audit
FOR EACH ROW EXECUTE FUNCTION communityhero.reject_history_rewrite();
CREATE OR REPLACE TRIGGER audit_no_truncate
BEFORE TRUNCATE ON communityhero.audit
FOR EACH STATEMENT EXECUTE FUNCTION communityhero.reject_history_rewrite();
CREATE OR REPLACE TRIGGER approvals_binding_immutable
BEFORE UPDATE OR DELETE ON communityhero.approvals
FOR EACH ROW EXECUTE FUNCTION communityhero.guard_approval_binding();
CREATE OR REPLACE TRIGGER approvals_no_truncate
BEFORE TRUNCATE ON communityhero.approvals
FOR EACH STATEMENT EXECUTE FUNCTION communityhero.reject_history_rewrite();

INSERT INTO communityhero.schema_migrations(version) VALUES(3) ON CONFLICT(version) DO NOTHING;
