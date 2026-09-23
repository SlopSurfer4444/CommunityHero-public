-- Offline pilot import only. Applying this schema does not dispatch operations
-- or resume jobs. The migration runner owns transaction and version tracking.
CREATE SCHEMA IF NOT EXISTS communityhero;

-- Preserve the complete source document, including unknown top-level fields and
-- collection order. Entity payloads below preserve each original JSON value.
CREATE TABLE communityhero.migration_imports (
    id text PRIMARY KEY,
    source_sha256 text NOT NULL UNIQUE,
    schema_version integer NOT NULL DEFAULT 1 CHECK (schema_version = 1),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE communityhero.workspaces (
    id text PRIMARY KEY,
    account text,
    import_id text NOT NULL UNIQUE REFERENCES communityhero.migration_imports(id),
    metadata jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata) = 'object'),
    execution_enabled boolean NOT NULL DEFAULT false CHECK (execution_enabled = false)
);

CREATE TABLE communityhero.posts (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id)
);

CREATE TABLE communityhero.branches (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    post_id text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id),
    FOREIGN KEY (workspace_id, post_id)
        REFERENCES communityhero.posts(workspace_id, id)
);

CREATE TABLE communityhero.items (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    post_id text,
    branch_id text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id),
    FOREIGN KEY (workspace_id, post_id)
        REFERENCES communityhero.posts(workspace_id, id),
    FOREIGN KEY (workspace_id, branch_id)
        REFERENCES communityhero.branches(workspace_id, id)
);

CREATE TABLE communityhero.conversations (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id)
);

CREATE TABLE communityhero.proposals (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    item_id text NOT NULL,
    status text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES communityhero.items(workspace_id, id)
);

CREATE TABLE communityhero.approvals (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    status text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id)
);

-- Older recovery fixtures may contain an operation with no routing references.
-- Missing references remain NULL; supplied references must resolve. Status is
-- copied unchanged, including dispatching/unknown, without authorizing replay.
CREATE TABLE communityhero.operations (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    item_id text,
    proposal_id text,
    approval_id text,
    status text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id),
    FOREIGN KEY (workspace_id, item_id)
        REFERENCES communityhero.items(workspace_id, id),
    FOREIGN KEY (workspace_id, proposal_id)
        REFERENCES communityhero.proposals(workspace_id, id),
    FOREIGN KEY (workspace_id, approval_id)
        REFERENCES communityhero.approvals(workspace_id, id)
);

CREATE TABLE communityhero.materials (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    kind text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id)
);

-- refId is polymorphic (and can be empty), so it is not a foreign key.
CREATE TABLE communityhero.jobs (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    kind text,
    status text,
    ref_id text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id)
);

CREATE TABLE communityhero.audit (
    workspace_id text NOT NULL REFERENCES communityhero.workspaces(id),
    id text NOT NULL,
    ordinal integer NOT NULL CHECK (ordinal >= 0),
    action text,
    ref_id text,
    payload jsonb NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    UNIQUE (workspace_id, ordinal),
    PRIMARY KEY (workspace_id, id)
);

CREATE INDEX branches_post_idx ON communityhero.branches(workspace_id, post_id);
CREATE INDEX items_post_idx ON communityhero.items(workspace_id, post_id);
CREATE INDEX items_branch_idx ON communityhero.items(workspace_id, branch_id);
CREATE INDEX proposals_item_idx ON communityhero.proposals(workspace_id, item_id);
CREATE INDEX operations_item_idx ON communityhero.operations(workspace_id, item_id);
CREATE INDEX operations_proposal_idx ON communityhero.operations(workspace_id, proposal_id);
CREATE INDEX operations_approval_idx ON communityhero.operations(workspace_id, approval_id);
