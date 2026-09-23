-- Explicit offline upgrade. Runner holds migration and server leases in one transaction.
CREATE TABLE IF NOT EXISTS communityhero.schema_migrations(version integer PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now());
CREATE TABLE communityhero.knowledge_entries (
 workspace_id text NOT NULL REFERENCES communityhero.workspaces(id), id text NOT NULL,
 ordinal integer NOT NULL CHECK(ordinal>=0), payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 source_material_id text, current_version_id text NOT NULL,
 PRIMARY KEY(workspace_id,id), UNIQUE(workspace_id,ordinal),
 FOREIGN KEY(workspace_id,source_material_id) REFERENCES communityhero.materials(workspace_id,id)
);
CREATE TABLE communityhero.knowledge_versions (
 workspace_id text NOT NULL REFERENCES communityhero.workspaces(id), id text NOT NULL,
 ordinal integer NOT NULL CHECK(ordinal>=0), payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'),
 entry_id text NOT NULL, source_material_id text,
 PRIMARY KEY(workspace_id,id), UNIQUE(workspace_id,ordinal), UNIQUE(workspace_id,entry_id,id),
 FOREIGN KEY(workspace_id,entry_id) REFERENCES communityhero.knowledge_entries(workspace_id,id) DEFERRABLE INITIALLY DEFERRED,
 FOREIGN KEY(workspace_id,source_material_id) REFERENCES communityhero.materials(workspace_id,id)
);
ALTER TABLE communityhero.knowledge_entries ADD CONSTRAINT knowledge_current_version_fk FOREIGN KEY(workspace_id,id,current_version_id) REFERENCES communityhero.knowledge_versions(workspace_id,entry_id,id) DEFERRABLE INITIALLY DEFERRED;
CREATE TABLE communityhero.feedback (
 workspace_id text NOT NULL REFERENCES communityhero.workspaces(id), id text NOT NULL,
 ordinal integer NOT NULL CHECK(ordinal>=0), payload jsonb NOT NULL CHECK(jsonb_typeof(payload)='object'), item_id text NOT NULL,
 PRIMARY KEY(workspace_id,id), UNIQUE(workspace_id,ordinal),
 FOREIGN KEY(workspace_id,item_id) REFERENCES communityhero.items(workspace_id,id)
);
INSERT INTO communityhero.knowledge_entries SELECT w.id,e.v->>'id',(e.n-1)::integer,e.v,e.v->>'sourceMaterialId',e.v->>'currentVersionId' FROM communityhero.workspaces w CROSS JOIN LATERAL jsonb_array_elements(COALESCE(w.metadata->'knowledge_entries','[]'::jsonb)) WITH ORDINALITY e(v,n);
INSERT INTO communityhero.knowledge_versions SELECT w.id,e.v->>'id',(e.n-1)::integer,e.v,e.v->>'entryId',e.v->>'sourceMaterialId' FROM communityhero.workspaces w CROSS JOIN LATERAL jsonb_array_elements(COALESCE(w.metadata->'knowledge_versions','[]'::jsonb)) WITH ORDINALITY e(v,n);
INSERT INTO communityhero.feedback SELECT w.id,e.v->>'id',(e.n-1)::integer,e.v,e.v->>'itemId' FROM communityhero.workspaces w CROSS JOIN LATERAL jsonb_array_elements(COALESCE(w.metadata->'feedback','[]'::jsonb)) WITH ORDINALITY e(v,n);
UPDATE communityhero.workspaces SET metadata=metadata-'knowledge_entries'-'knowledge_versions'-'feedback';
CREATE INDEX knowledge_versions_entry_idx ON communityhero.knowledge_versions(workspace_id,entry_id);
CREATE INDEX feedback_item_idx ON communityhero.feedback(workspace_id,item_id);
CREATE INDEX items_workflow_idx ON communityhero.items(workspace_id,(payload->>'workflow'));
CREATE INDEX operations_status_idx ON communityhero.operations(workspace_id,status);
CREATE INDEX jobs_status_idx ON communityhero.jobs(workspace_id,status);
CREATE FUNCTION communityhero.reject_history_rewrite() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'Immutable history cannot be changed or deleted'; END $$;
CREATE TRIGGER knowledge_versions_immutable BEFORE UPDATE OR DELETE ON communityhero.knowledge_versions FOR EACH ROW EXECUTE FUNCTION communityhero.reject_history_rewrite();
CREATE TRIGGER feedback_immutable BEFORE UPDATE OR DELETE ON communityhero.feedback FOR EACH ROW EXECUTE FUNCTION communityhero.reject_history_rewrite();
INSERT INTO communityhero.schema_migrations(version) VALUES(2);
