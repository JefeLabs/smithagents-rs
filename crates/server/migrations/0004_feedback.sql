ALTER TABLE intents ADD COLUMN base_branch TEXT;
ALTER TABLE intents ADD COLUMN origin_json TEXT;

CREATE TABLE feedback_submissions (
    id TEXT PRIMARY KEY NOT NULL,
    repo_id TEXT NOT NULL REFERENCES repos(id),
    request_json TEXT NOT NULL,
    title TEXT NOT NULL,
    branch TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    folder TEXT NOT NULL,
    pushed INTEGER NOT NULL DEFAULT 0,
    intent_id TEXT REFERENCES intents(id),
    issue_url TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
