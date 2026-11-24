CREATE TABLE repos (
    id TEXT PRIMARY KEY NOT NULL,
    path TEXT NOT NULL UNIQUE,
    default_branch TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE agents (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    color_json TEXT NOT NULL,
    repo_id TEXT NOT NULL REFERENCES repos(id),
    worktree_path TEXT NOT NULL,
    branch TEXT NOT NULL,
    harness_session_id TEXT,
    state TEXT NOT NULL,
    created_at TEXT NOT NULL,
    closed_at TEXT
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY NOT NULL,
    agent_id TEXT NOT NULL REFERENCES agents(id),
    seq INTEGER NOT NULL,
    role TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (agent_id, seq)
);
