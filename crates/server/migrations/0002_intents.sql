CREATE TABLE intents (
    id TEXT PRIMARY KEY NOT NULL,
    repo_id TEXT NOT NULL REFERENCES repos(id),
    slug TEXT NOT NULL,
    state TEXT NOT NULL,
    phase_json TEXT NOT NULL,
    seed_json TEXT NOT NULL,
    content_json TEXT NOT NULL,
    agent_id TEXT REFERENCES agents(id),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    queued_at TEXT,
    sent_at TEXT,
    resolved_at TEXT,
    last_start_error TEXT,
    UNIQUE (repo_id, slug)
);

CREATE TABLE intent_versions (
    intent_id TEXT NOT NULL REFERENCES intents(id),
    version INTEGER NOT NULL,
    author TEXT NOT NULL,
    content_json TEXT NOT NULL,
    at TEXT NOT NULL,
    PRIMARY KEY (intent_id, version)
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL
);

ALTER TABLE agents ADD COLUMN intent_id TEXT;
ALTER TABLE agents ADD COLUMN system_prompt_append TEXT;
