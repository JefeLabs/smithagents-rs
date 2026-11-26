CREATE TABLE plugin_config (
    plugin_id TEXT PRIMARY KEY NOT NULL,
    enabled INTEGER NOT NULL,
    settings_json TEXT NOT NULL
);

CREATE TABLE resource_sync (
    plugin_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    cursor TEXT,
    last_run_at TEXT,
    last_ok_at TEXT,
    last_error TEXT,
    next_due_at TEXT,
    failures INTEGER NOT NULL DEFAULT 0,
    listing_id TEXT,
    listing_from_start INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (plugin_id, resource)
);

CREATE TABLE resource_items (
    plugin_id TEXT NOT NULL,
    resource TEXT NOT NULL,
    item_id TEXT NOT NULL,
    title TEXT NOT NULL,
    url TEXT,
    source_updated_at TEXT,
    version TEXT,
    fields_json TEXT NOT NULL,
    synced_at TEXT NOT NULL,
    run_id TEXT NOT NULL,
    PRIMARY KEY (plugin_id, resource, item_id)
);

CREATE INDEX resource_items_list
    ON resource_items (plugin_id, resource, source_updated_at DESC, item_id);
