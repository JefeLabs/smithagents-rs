//! SQLite persistence for repos, agents, chat messages, intents, settings and plugin data.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use smithagents_core::agent::{Agent, AgentState, ChatMessage, Repo, Role};
use smithagents_core::feedback::{FeedbackRequest, SubmissionId};
use smithagents_core::ids::{AgentId, IntentId, MessageId, RepoId};
use smithagents_core::intent::{Intent, IntentFilter, IntentVersion};
use smithagents_core::plugin::{ItemsPage, ResourceItem};
use smithagents_plugin_protocol::SyncPage;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("a repo with this path already exists")]
    DuplicateRepoPath,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("malformed items cursor")]
    BadCursor,
    #[error("corrupt stored value: {0}")]
    Corrupt(String),
    #[error("database file: {0}")]
    Io(#[from] std::io::Error),
}

fn corrupt(e: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(e.to_string())
}

/// Creates the database file with mode 0600 if it does not exist, and sets
/// 0600 on it and on its `-wal` and `-shm` files where they exist.
#[cfg(unix)]
fn restrict_db_files(db: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    // An empty file is a valid, empty SQLite database.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(db)?;
    for suffix in ["", "-wal", "-shm"] {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        match std::fs::set_permissions(&name, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_db_files(_db: &Path) -> std::io::Result<()> {
    Ok(())
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, StoreError> {
    Ok(DateTime::parse_from_rfc3339(s)
        .map_err(corrupt)?
        .with_timezone(&Utc))
}

fn enum_to_str<T: serde::Serialize>(v: &T) -> Result<String, StoreError> {
    match serde_json::to_value(v).map_err(corrupt)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(corrupt(format!("expected string, got {other}"))),
    }
}

fn enum_from_str<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, StoreError> {
    serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(corrupt)
}

fn parse_id<T: FromStr>(s: &str) -> Result<T, StoreError>
where
    T::Err: std::fmt::Display,
{
    s.parse().map_err(corrupt)
}

fn repo_from_row(r: &SqliteRow) -> Result<Repo, StoreError> {
    Ok(Repo {
        id: parse_id(r.try_get("id")?)?,
        path: r.try_get::<String, _>("path")?.into(),
        default_branch: r.try_get("default_branch")?,
        created_at: parse_ts(r.try_get("created_at")?)?,
    })
}

fn agent_from_row(r: &SqliteRow) -> Result<Agent, StoreError> {
    Ok(Agent {
        id: parse_id(r.try_get("id")?)?,
        name: r.try_get("name")?,
        color: serde_json::from_str(r.try_get("color_json")?).map_err(corrupt)?,
        repo_id: parse_id(r.try_get("repo_id")?)?,
        worktree_path: r.try_get::<String, _>("worktree_path")?.into(),
        branch: r.try_get("branch")?,
        harness_session_id: r.try_get("harness_session_id")?,
        state: enum_from_str(r.try_get("state")?)?,
        created_at: parse_ts(r.try_get("created_at")?)?,
        closed_at: r
            .try_get::<Option<String>, _>("closed_at")?
            .as_deref()
            .map(parse_ts)
            .transpose()?,
        intent_id: r
            .try_get::<Option<String>, _>("intent_id")?
            .as_deref()
            .map(parse_id)
            .transpose()?,
    })
}

fn opt_ts(r: &SqliteRow, col: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
    r.try_get::<Option<String>, _>(col)?
        .as_deref()
        .map(parse_ts)
        .transpose()
}

fn intent_from_row(r: &SqliteRow) -> Result<Intent, StoreError> {
    Ok(Intent {
        id: parse_id(r.try_get("id")?)?,
        repo_id: parse_id(r.try_get("repo_id")?)?,
        slug: r.try_get("slug")?,
        state: enum_from_str(r.try_get("state")?)?,
        phase: serde_json::from_str(r.try_get("phase_json")?).map_err(corrupt)?,
        seed: serde_json::from_str(r.try_get("seed_json")?).map_err(corrupt)?,
        content: serde_json::from_str(r.try_get("content_json")?).map_err(corrupt)?,
        agent_id: r
            .try_get::<Option<String>, _>("agent_id")?
            .as_deref()
            .map(parse_id)
            .transpose()?,
        created_at: parse_ts(r.try_get("created_at")?)?,
        updated_at: parse_ts(r.try_get("updated_at")?)?,
        queued_at: opt_ts(r, "queued_at")?,
        sent_at: opt_ts(r, "sent_at")?,
        resolved_at: opt_ts(r, "resolved_at")?,
        last_start_error: r.try_get("last_start_error")?,
        base_branch: r.try_get("base_branch")?,
        origin: r
            .try_get::<Option<String>, _>("origin_json")?
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(corrupt)?,
    })
}

fn version_from_row(r: &SqliteRow) -> Result<IntentVersion, StoreError> {
    Ok(IntentVersion {
        intent_id: parse_id(r.try_get("intent_id")?)?,
        version: u32::try_from(r.try_get::<i64, _>("version")?).map_err(corrupt)?,
        author: enum_from_str(r.try_get("author")?)?,
        content: serde_json::from_str(r.try_get("content_json")?).map_err(corrupt)?,
        at: parse_ts(r.try_get("at")?)?,
    })
}

fn origin_json(intent: &Intent) -> Result<Option<String>, StoreError> {
    intent
        .origin
        .as_ref()
        .map(|o| serde_json::to_string(o).map_err(corrupt))
        .transpose()
}

/// One feedback submission: the request as sent and how far it got.
#[derive(Debug, Clone, PartialEq)]
pub struct Submission {
    pub id: SubmissionId,
    pub repo_id: RepoId,
    pub request: FeedbackRequest,
    pub title: String,
    pub branch: String,
    pub commit: String,
    pub folder: String,
    pub pushed: bool,
    pub intent_id: Option<IntentId>,
    pub issue_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn submission_from_row(r: &SqliteRow) -> Result<Submission, StoreError> {
    Ok(Submission {
        id: parse_id(r.try_get("id")?)?,
        repo_id: parse_id(r.try_get("repo_id")?)?,
        request: serde_json::from_str(r.try_get("request_json")?).map_err(corrupt)?,
        title: r.try_get("title")?,
        branch: r.try_get("branch")?,
        commit: r.try_get("commit_sha")?,
        folder: r.try_get("folder")?,
        pushed: r.try_get("pushed")?,
        intent_id: r
            .try_get::<Option<String>, _>("intent_id")?
            .as_deref()
            .map(parse_id)
            .transpose()?,
        issue_url: r.try_get("issue_url")?,
        created_at: parse_ts(r.try_get("created_at")?)?,
        updated_at: parse_ts(r.try_get("updated_at")?)?,
    })
}

fn map_intent_conflict(e: sqlx::Error) -> StoreError {
    match e {
        sqlx::Error::Database(d)
            if d.is_unique_violation()
                && d.message().contains("intents.repo_id")
                && d.message().contains("intents.slug") =>
        {
            StoreError::Conflict("an intent with this slug already exists in the repo".into())
        }
        e => e.into(),
    }
}

fn message_from_row(r: &SqliteRow) -> Result<ChatMessage, StoreError> {
    Ok(ChatMessage {
        id: parse_id(r.try_get("id")?)?,
        agent_id: parse_id(r.try_get("agent_id")?)?,
        seq: u64::try_from(r.try_get::<i64, _>("seq")?).map_err(corrupt)?,
        role: enum_from_str(r.try_get("role")?)?,
        text: r.try_get("text")?,
        created_at: parse_ts(r.try_get("created_at")?)?,
    })
}

async fn insert_version(
    tx: &mut sqlx::SqliteConnection,
    v: &IntentVersion,
    number: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO intent_versions (intent_id, version, author, content_json, at) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(v.intent_id.to_string())
    .bind(number)
    .bind(enum_to_str(&v.author)?)
    .bind(serde_json::to_string(&v.content).map_err(corrupt)?)
    .bind(ts(v.at))
    .execute(tx)
    .await?;
    Ok(())
}

async fn next_version(
    tx: &mut sqlx::SqliteConnection,
    intent_id: IntentId,
) -> Result<i64, StoreError> {
    Ok(sqlx::query_scalar(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM intent_versions WHERE intent_id = ?",
    )
    .bind(intent_id.to_string())
    .fetch_one(tx)
    .await?)
}

async fn update_intent(
    conn: &mut sqlx::SqliteConnection,
    intent: &Intent,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE intents SET repo_id = ?, slug = ?, state = ?, phase_json = ?, \
         seed_json = ?, content_json = ?, agent_id = ?, created_at = ?, updated_at = ?, \
         queued_at = ?, sent_at = ?, resolved_at = ?, last_start_error = ?, \
         base_branch = ?, origin_json = ? WHERE id = ?",
    )
    .bind(intent.repo_id.to_string())
    .bind(&intent.slug)
    .bind(enum_to_str(&intent.state)?)
    .bind(serde_json::to_string(&intent.phase).map_err(corrupt)?)
    .bind(serde_json::to_string(&intent.seed).map_err(corrupt)?)
    .bind(serde_json::to_string(&intent.content).map_err(corrupt)?)
    .bind(intent.agent_id.map(|a| a.to_string()))
    .bind(ts(intent.created_at))
    .bind(ts(intent.updated_at))
    .bind(intent.queued_at.map(ts))
    .bind(intent.sent_at.map(ts))
    .bind(intent.resolved_at.map(ts))
    .bind(intent.last_start_error.as_deref())
    .bind(intent.base_branch.as_deref())
    .bind(origin_json(intent)?)
    .bind(intent.id.to_string())
    .execute(conn)
    .await
    .map_err(map_intent_conflict)?;
    Ok(())
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Opens (creating if needed) the database file and runs migrations.
    ///
    /// The database and its `-wal`/`-shm` files hold chat transcripts, so they
    /// are owner-only (0600): a new database file is created that way before
    /// SQLite opens it (SQLite gives the `-wal` and `-shm` files the mode of
    /// the database), and existing files are tightened.
    pub async fn open(path: &Path) -> Result<Store, StoreError> {
        restrict_db_files(path)?;
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .connect_with(opts)
            .await?;
        let store = Self::migrated(pool).await?;
        // The -wal and -shm files exist now; tighten any left by an older build.
        restrict_db_files(path)?;
        Ok(store)
    }

    /// Opens a private in-memory database (single connection, so it is not shared).
    pub async fn open_in_memory() -> Result<Store, StoreError> {
        let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::migrated(pool).await
    }

    async fn migrated(pool: SqlitePool) -> Result<Store, StoreError> {
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Store { pool })
    }

    pub async fn insert_repo(&self, repo: &Repo) -> Result<(), StoreError> {
        let res = sqlx::query(
            "INSERT INTO repos (id, path, default_branch, created_at) VALUES (?, ?, ?, ?)",
        )
        .bind(repo.id.to_string())
        .bind(repo.path.to_string_lossy().into_owned())
        .bind(&repo.default_branch)
        .bind(ts(repo.created_at))
        .execute(&self.pool)
        .await;
        match res {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e))
                if e.is_unique_violation() && e.message().contains("repos.path") =>
            {
                Err(StoreError::DuplicateRepoPath)
            }
            Err(e) => Err(e.into()),
        }
    }

    pub async fn get_repo(&self, id: RepoId) -> Result<Option<Repo>, StoreError> {
        let row = sqlx::query("SELECT * FROM repos WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(repo_from_row).transpose()
    }

    pub async fn list_repos(&self) -> Result<Vec<Repo>, StoreError> {
        let rows = sqlx::query("SELECT * FROM repos ORDER BY created_at, id")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(repo_from_row).collect()
    }

    pub async fn repo_by_path(&self, path: &Path) -> Result<Option<Repo>, StoreError> {
        let row = sqlx::query("SELECT * FROM repos WHERE path = ?")
            .bind(path.to_string_lossy().into_owned())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(repo_from_row).transpose()
    }

    pub async fn insert_agent(&self, agent: &Agent) -> Result<(), StoreError> {
        self.insert_agent_with_append(agent, None).await
    }

    /// Inserts the agent row together with its system prompt append, atomically.
    pub async fn insert_agent_with_append(
        &self,
        agent: &Agent,
        system_prompt_append: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO agents (id, name, color_json, repo_id, worktree_path, branch, \
             harness_session_id, state, created_at, closed_at, intent_id, system_prompt_append) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(agent.id.to_string())
        .bind(&agent.name)
        .bind(serde_json::to_string(&agent.color).map_err(corrupt)?)
        .bind(agent.repo_id.to_string())
        .bind(agent.worktree_path.to_string_lossy().into_owned())
        .bind(&agent.branch)
        .bind(agent.harness_session_id.as_deref())
        .bind(enum_to_str(&agent.state)?)
        .bind(ts(agent.created_at))
        .bind(agent.closed_at.map(ts))
        .bind(agent.intent_id.map(|i| i.to_string()))
        .bind(system_prompt_append)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_agent(&self, id: AgentId) -> Result<Option<Agent>, StoreError> {
        let row = sqlx::query("SELECT * FROM agents WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(agent_from_row).transpose()
    }

    pub async fn list_agents(&self, include_closed: bool) -> Result<Vec<Agent>, StoreError> {
        let sql = if include_closed {
            "SELECT * FROM agents ORDER BY created_at, id"
        } else {
            "SELECT * FROM agents WHERE closed_at IS NULL ORDER BY created_at, id"
        };
        let rows = sqlx::query(sql).fetch_all(&self.pool).await?;
        rows.iter().map(agent_from_row).collect()
    }

    pub async fn update_agent_state(
        &self,
        id: AgentId,
        state: AgentState,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE agents SET state = ? WHERE id = ?")
            .bind(enum_to_str(&state)?)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_harness_session(&self, id: AgentId, session: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE agents SET harness_session_id = ? WHERE id = ?")
            .bind(session)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Marks every agent stored as `Working` or `Quiet` as `Dead` and returns
    /// how many changed. Run at daemon startup: such a turn belonged to a
    /// previous process and died with it.
    pub async fn mark_stale_running_agents_dead(&self) -> Result<u64, StoreError> {
        let res = sqlx::query("UPDATE agents SET state = ? WHERE state IN (?, ?)")
            .bind(enum_to_str(&AgentState::Dead)?)
            .bind(enum_to_str(&AgentState::Working)?)
            .bind(enum_to_str(&AgentState::Quiet)?)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Marks the agent closed at `at`. Closing an already-closed agent is a
    /// no-op (the original `closed_at` is kept); an unknown id is also a no-op.
    pub async fn close_agent(&self, id: AgentId, at: DateTime<Utc>) -> Result<(), StoreError> {
        sqlx::query("UPDATE agents SET closed_at = ? WHERE id = ? AND closed_at IS NULL")
            .bind(ts(at))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Appends a message, assigning the next per-agent `seq` (starting at 1)
    /// inside a `BEGIN IMMEDIATE` transaction so concurrent writers serialize.
    pub async fn append_message(
        &self,
        agent_id: AgentId,
        role: Role,
        text: &str,
    ) -> Result<ChatMessage, StoreError> {
        let msg_id = MessageId::new();
        let created_at = Utc::now();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let seq: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) + 1 FROM messages WHERE agent_id = ?")
                .bind(agent_id.to_string())
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query(
            "INSERT INTO messages (id, agent_id, seq, role, text, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(msg_id.to_string())
        .bind(agent_id.to_string())
        .bind(seq)
        .bind(enum_to_str(&role)?)
        .bind(text)
        .bind(ts(created_at))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(ChatMessage {
            id: msg_id,
            agent_id,
            seq: u64::try_from(seq).map_err(corrupt)?,
            role,
            text: text.to_owned(),
            created_at,
        })
    }

    pub async fn set_agent_system_prompt_append(
        &self,
        id: AgentId,
        text: &str,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE agents SET system_prompt_append = ? WHERE id = ?")
            .bind(text)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn agent_system_prompt_append(
        &self,
        id: AgentId,
    ) -> Result<Option<String>, StoreError> {
        let v: Option<Option<String>> =
            sqlx::query_scalar("SELECT system_prompt_append FROM agents WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await?;
        Ok(v.flatten())
    }

    /// Inserts the intent and its first version in one transaction. A
    /// duplicate `(repo_id, slug)` is a `StoreError::Conflict`.
    pub async fn insert_intent(
        &self,
        intent: &Intent,
        first_version: &IntentVersion,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO intents (id, repo_id, slug, state, phase_json, seed_json, \
             content_json, agent_id, created_at, updated_at, queued_at, sent_at, \
             resolved_at, last_start_error, base_branch, origin_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(intent.id.to_string())
        .bind(intent.repo_id.to_string())
        .bind(&intent.slug)
        .bind(enum_to_str(&intent.state)?)
        .bind(serde_json::to_string(&intent.phase).map_err(corrupt)?)
        .bind(serde_json::to_string(&intent.seed).map_err(corrupt)?)
        .bind(serde_json::to_string(&intent.content).map_err(corrupt)?)
        .bind(intent.agent_id.map(|a| a.to_string()))
        .bind(ts(intent.created_at))
        .bind(ts(intent.updated_at))
        .bind(intent.queued_at.map(ts))
        .bind(intent.sent_at.map(ts))
        .bind(intent.resolved_at.map(ts))
        .bind(intent.last_start_error.as_deref())
        .bind(intent.base_branch.as_deref())
        .bind(origin_json(intent)?)
        .execute(&mut *tx)
        .await
        .map_err(map_intent_conflict)?;
        insert_version(&mut tx, first_version, i64::from(first_version.version)).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_intent(&self, id: IntentId) -> Result<Option<Intent>, StoreError> {
        let row = sqlx::query("SELECT * FROM intents WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(intent_from_row).transpose()
    }

    /// Newest `updated_at` first.
    pub async fn list_intents(&self, filter: IntentFilter) -> Result<Vec<Intent>, StoreError> {
        let state = filter.state.as_ref().map(enum_to_str).transpose()?;
        let rows = sqlx::query(
            "SELECT * FROM intents WHERE (?1 IS NULL OR state = ?1) \
             AND (?2 IS NULL OR repo_id = ?2) ORDER BY updated_at DESC, id",
        )
        .bind(state)
        .bind(filter.repo_id.map(|r| r.to_string()))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(intent_from_row).collect()
    }

    /// Full row update; touches no timestamp itself.
    pub async fn save_intent(&self, intent: &Intent) -> Result<(), StoreError> {
        let mut conn = self.pool.acquire().await?;
        update_intent(&mut conn, intent).await
    }

    /// Stores `v` under the next version number (max + 1, computed inside a
    /// `BEGIN IMMEDIATE` transaction so concurrent callers serialize) and
    /// returns that number. `v.version` is ignored.
    pub async fn append_version(&self, v: &IntentVersion) -> Result<u32, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let next = next_version(&mut tx, v.intent_id).await?;
        insert_version(&mut tx, v, next).await?;
        tx.commit().await?;
        u32::try_from(next).map_err(corrupt)
    }

    /// [`Self::save_intent`] and [`Self::append_version`] in one
    /// transaction: both are stored or neither is. Returns the version number.
    pub async fn save_intent_with_version(
        &self,
        intent: &Intent,
        v: &IntentVersion,
    ) -> Result<u32, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        update_intent(&mut tx, intent).await?;
        let next = next_version(&mut tx, v.intent_id).await?;
        insert_version(&mut tx, v, next).await?;
        tx.commit().await?;
        u32::try_from(next).map_err(corrupt)
    }

    pub async fn list_versions(&self, id: IntentId) -> Result<Vec<IntentVersion>, StoreError> {
        let rows =
            sqlx::query("SELECT * FROM intent_versions WHERE intent_id = ? ORDER BY version")
                .bind(id.to_string())
                .fetch_all(&self.pool)
                .await?;
        rows.iter().map(version_from_row).collect()
    }

    /// Stores a new submission. A second insert of an id is a
    /// `StoreError::Conflict`.
    pub async fn insert_submission(&self, s: &Submission) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO feedback_submissions (id, repo_id, request_json, title, branch, \
             commit_sha, folder, pushed, intent_id, issue_url, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(s.id.to_string())
        .bind(s.repo_id.to_string())
        .bind(serde_json::to_string(&s.request).map_err(corrupt)?)
        .bind(&s.title)
        .bind(&s.branch)
        .bind(&s.commit)
        .bind(&s.folder)
        .bind(s.pushed)
        .bind(s.intent_id.map(|i| i.to_string()))
        .bind(s.issue_url.as_deref())
        .bind(ts(s.created_at))
        .bind(ts(s.updated_at))
        .execute(&self.pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                StoreError::Conflict("this submission already exists".into())
            }
            e => e.into(),
        })?;
        Ok(())
    }

    pub async fn get_submission(&self, id: SubmissionId) -> Result<Option<Submission>, StoreError> {
        let row = sqlx::query("SELECT * FROM feedback_submissions WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(submission_from_row).transpose()
    }

    /// Updates `pushed`, `intent_id`, `issue_url` and `updated_at`; the rest
    /// of a submission is fixed when it is inserted.
    pub async fn save_submission(&self, s: &Submission) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE feedback_submissions SET pushed = ?, intent_id = ?, issue_url = ?, \
             updated_at = ? WHERE id = ?",
        )
        .bind(s.pushed)
        .bind(s.intent_id.map(|i| i.to_string()))
        .bind(s.issue_url.as_deref())
        .bind(ts(s.updated_at))
        .bind(s.id.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn slug_exists(&self, repo_id: RepoId, slug: &str) -> Result<bool, StoreError> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM intents WHERE repo_id = ? AND slug = ?")
                .bind(repo_id.to_string())
                .bind(slug)
                .fetch_one(&self.pool)
                .await?;
        Ok(n > 0)
    }

    pub async fn intents_in_phase_polishing(&self) -> Result<Vec<Intent>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM intents WHERE json_extract(phase_json, '$.kind') = 'polishing' \
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(intent_from_row).collect()
    }

    /// Queued intents, oldest `queued_at` first.
    pub async fn queued_intents(&self) -> Result<Vec<Intent>, StoreError> {
        let rows = sqlx::query("SELECT * FROM intents WHERE state = ? ORDER BY queued_at, id")
            .bind(enum_to_str(&smithagents_core::intent::IntentState::Queued)?)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(intent_from_row).collect()
    }

    pub async fn get_setting<T: serde::de::DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, StoreError> {
        let v: Option<String> = sqlx::query_scalar("SELECT value_json FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        v.map(|s| serde_json::from_str(&s).map_err(corrupt))
            .transpose()
    }

    pub async fn set_setting<T: serde::Serialize>(
        &self,
        key: &str,
        value: &T,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO settings (key, value_json) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        )
        .bind(key)
        .bind(serde_json::to_string(value).map_err(corrupt)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list_messages(
        &self,
        agent_id: AgentId,
        after_seq: u64,
        limit: u32,
    ) -> Result<Vec<ChatMessage>, StoreError> {
        // The number is the client's. No stored seq is above `i64::MAX`, so
        // nothing follows a larger one.
        let after = i64::try_from(after_seq).unwrap_or(i64::MAX);
        let rows = sqlx::query(
            "SELECT * FROM messages WHERE agent_id = ? AND seq > ? ORDER BY seq LIMIT ?",
        )
        .bind(agent_id.to_string())
        .bind(after)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(message_from_row).collect()
    }
}

/// Plugin settings kept for one plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginConfig {
    pub enabled: bool,
    pub settings: serde_json::Value,
}

/// Where one resource's sync stands. The default is "never ran".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncState {
    pub cursor: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub next_due_at: Option<DateTime<Utc>>,
    pub failures: u32,
    /// The listing the cursor is part of while one spans pages and runs;
    /// the plugin manager stamps that listing's items with it.
    pub listing_id: Option<String>,
    /// With `listing_id`: that listing began with no cursor, so it saw every
    /// page. Without: the last listing ended, so the next one starts at the
    /// beginning. False when not known.
    pub listing_from_start: bool,
}

const MAX_ITEMS_LIMIT: u32 = 200;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.is_empty() || !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).ok()?, 16).ok())
        .collect()
}

/// The opaque `next` token: hex of the JSON `[source_updated_at | null, item_id]`
/// of the last row on the page.
fn encode_cursor(updated: Option<&str>, item_id: &str) -> Result<String, StoreError> {
    let json = serde_json::to_vec(&(updated, item_id)).map_err(corrupt)?;
    Ok(hex(&json))
}

/// Parses a token from a client. Anything that is not exactly what
/// `encode_cursor` makes, including a timestamp that does not parse, is
/// `BadCursor`.
fn decode_cursor(token: &str) -> Result<(Option<String>, String), StoreError> {
    let bytes = unhex(token).ok_or(StoreError::BadCursor)?;
    let (updated, id): (Option<String>, String) =
        serde_json::from_slice(&bytes).map_err(|_| StoreError::BadCursor)?;
    if let Some(u) = &updated {
        // Re-normalise so the stored column's format is what gets compared.
        let t = parse_ts(u).map_err(|_| StoreError::BadCursor)?;
        return Ok((Some(ts(t)), id));
    }
    Ok((None, id))
}

fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

impl Store {
    pub async fn plugin_config(&self, id: &str) -> Result<Option<PluginConfig>, StoreError> {
        let row =
            sqlx::query("SELECT enabled, settings_json FROM plugin_config WHERE plugin_id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| {
            Ok(PluginConfig {
                enabled: r.try_get::<i64, _>("enabled")? != 0,
                settings: serde_json::from_str(r.try_get("settings_json")?).map_err(corrupt)?,
            })
        })
        .transpose()
    }

    pub async fn set_plugin_config(&self, id: &str, cfg: &PluginConfig) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO plugin_config (plugin_id, enabled, settings_json) VALUES (?, ?, ?) \
             ON CONFLICT (plugin_id) DO UPDATE SET enabled = excluded.enabled, \
             settings_json = excluded.settings_json",
        )
        .bind(id)
        .bind(i64::from(cfg.enabled))
        .bind(serde_json::to_string(&cfg.settings).map_err(corrupt)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the plugin's configuration with `cfg` and drops all of its
    /// sync state (cursors, listings, due times) in one transaction. Its
    /// items stay.
    pub async fn reset_plugin_state(&self, id: &str, cfg: &PluginConfig) -> Result<(), StoreError> {
        let settings = serde_json::to_string(&cfg.settings).map_err(corrupt)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO plugin_config (plugin_id, enabled, settings_json) VALUES (?, ?, ?) \
             ON CONFLICT (plugin_id) DO UPDATE SET enabled = excluded.enabled, \
             settings_json = excluded.settings_json",
        )
        .bind(id)
        .bind(i64::from(cfg.enabled))
        .bind(settings)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM resource_sync WHERE plugin_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The saved state, or the default when the resource never ran.
    pub async fn sync_state(&self, id: &str, resource: &str) -> Result<SyncState, StoreError> {
        let row = sqlx::query("SELECT * FROM resource_sync WHERE plugin_id = ? AND resource = ?")
            .bind(id)
            .bind(resource)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| {
            Ok(SyncState {
                cursor: r.try_get("cursor")?,
                last_run_at: opt_ts(&r, "last_run_at")?,
                last_ok_at: opt_ts(&r, "last_ok_at")?,
                last_error: r.try_get("last_error")?,
                next_due_at: opt_ts(&r, "next_due_at")?,
                failures: u32::try_from(r.try_get::<i64, _>("failures")?).map_err(corrupt)?,
                listing_id: r.try_get("listing_id")?,
                listing_from_start: r.try_get::<i64, _>("listing_from_start")? != 0,
            })
        })
        .transpose()
        .map(Option::unwrap_or_default)
    }

    pub async fn save_sync_state(
        &self,
        id: &str,
        resource: &str,
        s: &SyncState,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO resource_sync (plugin_id, resource, cursor, last_run_at, last_ok_at, \
             last_error, next_due_at, failures, listing_id, listing_from_start) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (plugin_id, resource) DO UPDATE SET cursor = excluded.cursor, \
             last_run_at = excluded.last_run_at, last_ok_at = excluded.last_ok_at, \
             last_error = excluded.last_error, next_due_at = excluded.next_due_at, \
             failures = excluded.failures, listing_id = excluded.listing_id, \
             listing_from_start = excluded.listing_from_start",
        )
        .bind(id)
        .bind(resource)
        .bind(s.cursor.as_deref())
        .bind(s.last_run_at.map(ts))
        .bind(s.last_ok_at.map(ts))
        .bind(s.last_error.as_deref())
        .bind(s.next_due_at.map(ts))
        .bind(i64::from(s.failures))
        .bind(s.listing_id.as_deref())
        .bind(i64::from(s.listing_from_start))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// One transaction: upserts, deletes, the new cursor and the listing
    /// state that goes with it (`listing_id`, `listing_from_start`, stored as
    /// given). Returns `(upserted, deleted)`, counting only rows that changed.
    /// An unchanged row still takes the new `run_id` (so `delete_unseen` keeps
    /// it) but keeps its `synced_at`. The page is assumed validated by the
    /// runtime.
    #[allow(clippy::too_many_arguments)]
    pub async fn apply_page(
        &self,
        id: &str,
        resource: &str,
        run_id: &str,
        page: &SyncPage,
        at: DateTime<Utc>,
        listing_id: Option<&str>,
        listing_from_start: bool,
    ) -> Result<(u32, u32), StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut upserted = 0u32;
        for item in &page.upserts {
            let updated = item.updated_at.map(ts);
            let fields = serde_json::to_string(&item.fields).map_err(corrupt)?;
            let existing = sqlx::query(
                "SELECT title, url, source_updated_at, version, fields_json FROM resource_items \
                 WHERE plugin_id = ? AND resource = ? AND item_id = ?",
            )
            .bind(id)
            .bind(resource)
            .bind(&item.id)
            .fetch_optional(&mut *tx)
            .await?;
            // Relies on serde_json's sorted-key maps (no `preserve_order`): equal values serialise equally.
            let unchanged = match &existing {
                Some(r) => {
                    r.try_get::<String, _>("title")? == item.title
                        && r.try_get::<Option<String>, _>("url")? == item.url
                        && r.try_get::<Option<String>, _>("source_updated_at")? == updated
                        && r.try_get::<Option<String>, _>("version")? == item.version
                        && r.try_get::<String, _>("fields_json")? == fields
                }
                None => false,
            };
            if unchanged {
                sqlx::query(
                    "UPDATE resource_items SET run_id = ? \
                     WHERE plugin_id = ? AND resource = ? AND item_id = ?",
                )
                .bind(run_id)
                .bind(id)
                .bind(resource)
                .bind(&item.id)
                .execute(&mut *tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO resource_items (plugin_id, resource, item_id, title, url, \
                     source_updated_at, version, fields_json, synced_at, run_id) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT (plugin_id, resource, item_id) DO UPDATE SET \
                     title = excluded.title, url = excluded.url, \
                     source_updated_at = excluded.source_updated_at, version = excluded.version, \
                     fields_json = excluded.fields_json, synced_at = excluded.synced_at, \
                     run_id = excluded.run_id",
                )
                .bind(id)
                .bind(resource)
                .bind(&item.id)
                .bind(&item.title)
                .bind(item.url.as_deref())
                .bind(updated)
                .bind(item.version.as_deref())
                .bind(fields)
                .bind(ts(at))
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
                upserted += 1;
            }
        }
        let mut deleted = 0u32;
        for item_id in &page.deletes {
            let n = sqlx::query(
                "DELETE FROM resource_items WHERE plugin_id = ? AND resource = ? AND item_id = ?",
            )
            .bind(id)
            .bind(resource)
            .bind(item_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            deleted += u32::try_from(n).map_err(corrupt)?;
        }
        sqlx::query(
            "INSERT INTO resource_sync (plugin_id, resource, cursor, listing_id, \
             listing_from_start) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (plugin_id, resource) DO UPDATE SET cursor = excluded.cursor, \
             listing_id = excluded.listing_id, listing_from_start = excluded.listing_from_start",
        )
        .bind(id)
        .bind(resource)
        .bind(page.next_cursor.as_deref())
        .bind(listing_id)
        .bind(i64::from(listing_from_start))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((upserted, deleted))
    }

    /// Removes the resource's rows that the run `run_id` did not touch.
    pub async fn delete_unseen(
        &self,
        id: &str,
        resource: &str,
        run_id: &str,
    ) -> Result<u32, StoreError> {
        let n = sqlx::query(
            "DELETE FROM resource_items WHERE plugin_id = ? AND resource = ? AND run_id <> ?",
        )
        .bind(id)
        .bind(resource)
        .bind(run_id)
        .execute(&self.pool)
        .await?
        .rows_affected();
        u32::try_from(n).map_err(corrupt)
    }

    /// Removes the resource's items and its sync state; returns the item count removed.
    pub async fn delete_resource(&self, id: &str, resource: &str) -> Result<u32, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let n = sqlx::query("DELETE FROM resource_items WHERE plugin_id = ? AND resource = ?")
            .bind(id)
            .bind(resource)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        sqlx::query("DELETE FROM resource_sync WHERE plugin_id = ? AND resource = ?")
            .bind(id)
            .bind(resource)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        u32::try_from(n).map_err(corrupt)
    }

    /// Items newest first (`source_updated_at` descending, rows without one
    /// last, ties by id). `after` is a `next` token from an earlier page of the
    /// same query; one that this store did not make is `BadCursor`.
    pub async fn list_items(
        &self,
        id: &str,
        resource: &str,
        limit: u32,
        after: Option<&str>,
        q: Option<&str>,
    ) -> Result<ItemsPage, StoreError> {
        let limit = limit.clamp(1, MAX_ITEMS_LIMIT);
        let after = after.map(decode_cursor).transpose()?;
        // One static statement for every combination: `?3` says whether there
        // is a cursor, `?4` whether it sits in the rows without a date.
        let (has_after, after_null, after_ts, after_id) = match &after {
            Some((Some(t), i)) => (1_i64, 0_i64, Some(t.as_str()), Some(i.as_str())),
            Some((None, i)) => (1, 1, None, Some(i.as_str())),
            None => (0, 0, None, None),
        };
        let rows = sqlx::query(
            "SELECT item_id, title, url, source_updated_at, version, fields_json, synced_at \
             FROM resource_items WHERE plugin_id = ?1 AND resource = ?2 \
             AND (?3 = 0 \
                  OR (?4 = 1 AND source_updated_at IS NULL AND item_id > ?6) \
                  OR (?4 = 0 AND (source_updated_at IS NULL OR source_updated_at < ?5 \
                                  OR (source_updated_at = ?5 AND item_id > ?6)))) \
             AND (?7 IS NULL OR title LIKE ?7 ESCAPE '\\') \
             ORDER BY source_updated_at DESC NULLS LAST, item_id LIMIT ?8",
        )
        .bind(id)
        .bind(resource)
        .bind(has_after)
        .bind(after_null)
        .bind(after_ts)
        .bind(after_id)
        .bind(q.map(like_pattern))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;

        let more = rows.len() > limit as usize;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        let mut next = None;
        for r in rows.iter().take(limit as usize) {
            let item_id: String = r.try_get("item_id")?;
            let updated: Option<String> = r.try_get("source_updated_at")?;
            if more {
                next = Some(encode_cursor(updated.as_deref(), &item_id)?);
            }
            items.push(ResourceItem {
                id: item_id,
                title: r.try_get("title")?,
                url: r.try_get("url")?,
                updated_at: updated.as_deref().map(parse_ts).transpose()?,
                version: r.try_get("version")?,
                fields: serde_json::from_str(r.try_get("fields_json")?).map_err(corrupt)?,
                synced_at: parse_ts(r.try_get("synced_at")?)?,
            });
        }
        Ok(ItemsPage { items, next })
    }

    pub async fn count_items(&self, id: &str, resource: &str) -> Result<u64, StoreError> {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM resource_items WHERE plugin_id = ? AND resource = ?",
        )
        .bind(id)
        .bind(resource)
        .fetch_one(&self.pool)
        .await?;
        u64::try_from(n).map_err(corrupt)
    }

    /// Resource kinds the store holds items or sync state for, sorted.
    pub async fn known_resources(&self, id: &str) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT resource FROM resource_items WHERE plugin_id = ?1 \
             UNION SELECT resource FROM resource_sync WHERE plugin_id = ?1 ORDER BY resource",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithagents_core::palette::palette;

    fn repo(path: &str) -> Repo {
        Repo {
            id: RepoId::new(),
            path: path.into(),
            default_branch: "main".into(),
            created_at: Utc::now(),
        }
    }

    fn agent(repo_id: RepoId, name: &str) -> Agent {
        Agent {
            id: AgentId::new(),
            name: name.into(),
            color: palette()[0],
            repo_id,
            worktree_path: format!("/tmp/wt/{name}").into(),
            branch: format!("agent/{name}"),
            harness_session_id: Some("sess".into()),
            state: AgentState::Working,
            created_at: Utc::now(),
            closed_at: None,
            intent_id: None,
        }
    }

    async fn seeded(store: &Store, names: &[&str]) -> Vec<Agent> {
        let r = repo("/repos/a");
        store.insert_repo(&r).await.unwrap();
        let mut out = Vec::new();
        for n in names {
            let a = agent(r.id, n);
            store.insert_agent(&a).await.unwrap();
            out.push(a);
        }
        out
    }

    #[tokio::test]
    async fn state_persists_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sa.db");
        let r = repo("/repos/a");
        let a = agent(r.id, "Earth");
        {
            let store = Store::open(&db).await.unwrap();
            store.insert_repo(&r).await.unwrap();
            store.insert_agent(&a).await.unwrap();
        }
        let store = Store::open(&db).await.unwrap();
        assert_eq!(store.get_agent(a.id).await.unwrap(), Some(a));
        assert_eq!(store.get_repo(r.id).await.unwrap(), Some(r.clone()));
        assert_eq!(store.repo_by_path(&r.path).await.unwrap(), Some(r.clone()));
        assert_eq!(store.list_repos().await.unwrap(), vec![r]);
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// `<db>-wal` or `<db>-shm`.
    #[cfg(unix)]
    fn sidecar(db: &Path, suffix: &str) -> std::path::PathBuf {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        name.into()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn database_files_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sa.db");
        let store = Store::open(&db).await.unwrap();
        store.insert_repo(&repo("/repos/a")).await.unwrap();
        assert_eq!(mode(&db), 0o600);
        for suffix in ["-wal", "-shm"] {
            let file = sidecar(&db, suffix);
            assert!(file.exists(), "no {}", file.display());
            assert_eq!(mode(&file), 0o600, "{}", file.display());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn open_tightens_existing_database_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sa.db");
        {
            let store = Store::open(&db).await.unwrap();
            store.insert_repo(&repo("/repos/a")).await.unwrap();
        }
        // As left by an earlier build, readable by everyone.
        for file in [db.clone(), sidecar(&db, "-wal"), sidecar(&db, "-shm")] {
            if file.exists() {
                std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        let store = Store::open(&db).await.unwrap();
        store.insert_repo(&repo("/repos/b")).await.unwrap();
        for file in [db.clone(), sidecar(&db, "-wal"), sidecar(&db, "-shm")] {
            assert_eq!(mode(&file), 0o600, "{}", file.display());
        }
    }

    #[tokio::test]
    async fn agent_updates_round_trip() {
        let store = Store::open_in_memory().await.unwrap();
        let a = seeded(&store, &["Earth"]).await.remove(0);
        store
            .update_agent_state(a.id, AgentState::Blocked)
            .await
            .unwrap();
        store
            .set_harness_session(a.id, "new-session")
            .await
            .unwrap();
        let got = store.get_agent(a.id).await.unwrap().unwrap();
        assert_eq!(got.state, AgentState::Blocked);
        assert_eq!(got.harness_session_id.as_deref(), Some("new-session"));
        assert!(store.get_agent(AgentId::new()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn messages_after_a_seq_no_message_can_have_are_none() {
        // A seq is kept as an i64; a client may ask for what follows a
        // larger one. That is an empty page, not a corrupt store.
        let store = Store::open_in_memory().await.unwrap();
        let a = seeded(&store, &["Earth"]).await.remove(0);
        store.append_message(a.id, Role::User, "one").await.unwrap();
        let largest = i64::MAX.unsigned_abs();
        for after in [largest, largest + 1, u64::MAX] {
            let page = store.list_messages(a.id, after, 10).await.unwrap();
            assert!(page.is_empty(), "after {after}: {page:?}");
        }
    }

    #[tokio::test]
    async fn message_seq_is_monotonic_per_agent() {
        let store = Store::open_in_memory().await.unwrap();
        let agents = seeded(&store, &["Earth", "Mars"]).await;
        let (a, b) = (agents[0].id, agents[1].id);
        let mut seqs = (Vec::new(), Vec::new());
        for i in 0..3 {
            seqs.0.push(
                store
                    .append_message(a, Role::User, &format!("a{i}"))
                    .await
                    .unwrap()
                    .seq,
            );
            seqs.1.push(
                store
                    .append_message(b, Role::Assistant, &format!("b{i}"))
                    .await
                    .unwrap()
                    .seq,
            );
        }
        assert_eq!(seqs.0, vec![1, 2, 3]);
        assert_eq!(seqs.1, vec![1, 2, 3]);

        let after = store.list_messages(a, 1, 10).await.unwrap();
        assert_eq!(after.iter().map(|m| m.seq).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(after[0].text, "a1");
        assert_eq!(after[0].role, Role::User);
        let limited = store.list_messages(b, 0, 2).await.unwrap();
        assert_eq!(limited.len(), 2);
        assert!(limited.iter().all(|m| m.agent_id == b));
    }

    #[tokio::test]
    async fn concurrent_appends_get_unique_seqs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("sa.db")).await.unwrap();
        let a = seeded(&store, &["Earth"]).await.remove(0).id;
        let handles: Vec<_> = (0..20)
            .map(|i| {
                let s = store.clone();
                tokio::spawn(async move { s.append_message(a, Role::User, &format!("m{i}")).await })
            })
            .collect();
        let mut seqs = Vec::new();
        for h in handles {
            seqs.push(h.await.unwrap().unwrap().seq);
        }
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=20).collect::<Vec<u64>>());
    }

    #[tokio::test]
    async fn closed_agents_hidden_unless_requested() {
        let store = Store::open_in_memory().await.unwrap();
        let agents = seeded(&store, &["Earth", "Mars"]).await;
        let first_close = Utc::now();
        store.close_agent(agents[0].id, first_close).await.unwrap();
        // Closing again keeps the original timestamp and does not error.
        store.close_agent(agents[0].id, Utc::now()).await.unwrap();

        let open = store.list_agents(false).await.unwrap();
        assert_eq!(
            open.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![agents[1].id]
        );
        let all = store.list_agents(true).await.unwrap();
        assert_eq!(all.len(), 2);
        let closed = store.get_agent(agents[0].id).await.unwrap().unwrap();
        assert_eq!(closed.closed_at, Some(first_close));
    }

    #[tokio::test]
    async fn stale_running_agents_are_marked_dead() {
        let store = Store::open_in_memory().await.unwrap();
        let agents = seeded(&store, &["Mercury", "Venus", "Earth", "Mars", "Jupiter"]).await;
        let states = [
            AgentState::Idle,
            AgentState::Working,
            AgentState::Quiet,
            AgentState::Done,
            AgentState::Blocked,
        ];
        for (a, s) in agents.iter().zip(states) {
            store.update_agent_state(a.id, s).await.unwrap();
        }
        // A closed agent left Quiet is stale too.
        store.close_agent(agents[2].id, Utc::now()).await.unwrap();

        assert_eq!(store.mark_stale_running_agents_dead().await.unwrap(), 2);
        let mut after = Vec::new();
        for a in &agents {
            after.push(store.get_agent(a.id).await.unwrap().unwrap().state);
        }
        assert_eq!(
            after,
            [
                AgentState::Idle,
                AgentState::Dead,
                AgentState::Dead,
                AgentState::Done,
                AgentState::Blocked,
            ]
        );
        assert_eq!(store.mark_stale_running_agents_dead().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn duplicate_repo_path_is_rejected() {
        let store = Store::open_in_memory().await.unwrap();
        store.insert_repo(&repo("/repos/a")).await.unwrap();
        let err = store.insert_repo(&repo("/repos/a")).await.unwrap_err();
        assert!(matches!(err, StoreError::DuplicateRepoPath), "got {err:?}");
    }

    // ---- intents ----

    use smithagents_core::ids::{IntentId, ItemId, QuestionId};
    use smithagents_core::intent::{
        Answer, Content, Intent, IntentFilter, IntentKind, IntentState, IntentVersion, Item,
        ItemStatus, Phase, Question, QuestionOption, Seed, SeedSource, VersionAuthor,
    };

    fn content(title: &str) -> Content {
        Content {
            title: title.into(),
            kind: IntentKind::Bug,
            current_behavior: "broken".into(),
            questions: vec![Question {
                id: QuestionId::new(),
                text: "which?".into(),
                options: vec![QuestionOption {
                    id: "a".into(),
                    label: "A".into(),
                }],
                answer: Answer::Option {
                    option_id: "a".into(),
                },
            }],
            preconditions: vec![],
            criteria: vec![Item {
                id: ItemId::new(),
                text: "works".into(),
                status: ItemStatus::Accepted,
                edited: true,
                intent_ref: None,
                fixed: false,
            }],
            stories: vec![],
            depends_on: vec![],
        }
    }

    fn intent(repo_id: RepoId, slug: &str) -> Intent {
        let now = Utc::now();
        Intent {
            id: IntentId::new(),
            repo_id,
            slug: slug.into(),
            state: IntentState::Draft,
            phase: Phase::Polishing { pass: 1 },
            seed: Seed {
                kind: None,
                source: SeedSource::Text,
                text: "seed".into(),
            },
            content: content(slug),
            agent_id: None,
            created_at: now,
            updated_at: now,
            queued_at: None,
            sent_at: None,
            resolved_at: None,
            last_start_error: Some("boom".into()),
            base_branch: None,
            origin: None,
        }
    }

    fn version(i: &Intent, v: u32) -> IntentVersion {
        IntentVersion {
            intent_id: i.id,
            version: v,
            author: VersionAuthor::Seed,
            content: i.content.clone(),
            at: Utc::now(),
        }
    }

    async fn store_with_repo() -> (Store, Repo) {
        let store = Store::open_in_memory().await.unwrap();
        let r = repo("/repos/i");
        store.insert_repo(&r).await.unwrap();
        (store, r)
    }

    #[tokio::test]
    async fn intent_round_trips_with_first_version() {
        let (store, r) = store_with_repo().await;
        let i = intent(r.id, "one");
        store.insert_intent(&i, &version(&i, 1)).await.unwrap();
        assert_eq!(store.get_intent(i.id).await.unwrap(), Some(i.clone()));
        assert!(store.get_intent(IntentId::new()).await.unwrap().is_none());
        let vs = store.list_versions(i.id).await.unwrap();
        assert_eq!(vs.len(), 1);
        assert_eq!(vs[0].version, 1);
        assert_eq!(vs[0].content, i.content);

        let mut j = i.clone();
        j.state = IntentState::Sent;
        j.phase = Phase::Review;
        j.last_start_error = None;
        j.sent_at = Some(Utc::now());
        store.save_intent(&j).await.unwrap();
        assert_eq!(store.get_intent(i.id).await.unwrap(), Some(j));
    }

    #[tokio::test]
    async fn duplicate_slug_is_conflict_and_rolls_back() {
        let (store, r) = store_with_repo().await;
        let i = intent(r.id, "same");
        store.insert_intent(&i, &version(&i, 1)).await.unwrap();
        let dup = intent(r.id, "same");
        let err = store
            .insert_intent(&dup, &version(&dup, 1))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "got {err:?}");
        assert!(store.get_intent(dup.id).await.unwrap().is_none());
        assert!(store.slug_exists(r.id, "same").await.unwrap());
        assert!(!store.slug_exists(r.id, "other").await.unwrap());
        // Same slug in another repo is fine.
        let r2 = repo("/repos/j");
        store.insert_repo(&r2).await.unwrap();
        let ok = intent(r2.id, "same");
        store.insert_intent(&ok, &version(&ok, 1)).await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_append_version_has_no_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("sa.db")).await.unwrap();
        let r = repo("/repos/i");
        store.insert_repo(&r).await.unwrap();
        let i = intent(r.id, "one");
        store.insert_intent(&i, &version(&i, 1)).await.unwrap();
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let s = store.clone();
                let v = version(&i, 0);
                tokio::spawn(async move { s.append_version(&v).await })
            })
            .collect();
        let mut got = Vec::new();
        for h in handles {
            got.push(h.await.unwrap().unwrap());
        }
        got.sort_unstable();
        assert_eq!(got, (2..=11).collect::<Vec<u32>>());
        let vs = store.list_versions(i.id).await.unwrap();
        assert_eq!(
            vs.iter().map(|v| v.version).collect::<Vec<_>>(),
            (1..=11).collect::<Vec<u32>>()
        );
    }

    #[tokio::test]
    async fn append_version_numbers_sequentially() {
        let (store, r) = store_with_repo().await;
        let i = intent(r.id, "one");
        store.insert_intent(&i, &version(&i, 1)).await.unwrap();
        assert_eq!(store.append_version(&version(&i, 0)).await.unwrap(), 2);
        assert_eq!(store.append_version(&version(&i, 0)).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn save_with_version_stores_both_or_neither() {
        let (store, r) = store_with_repo().await;
        let i = intent(r.id, "one");
        store.insert_intent(&i, &version(&i, 1)).await.unwrap();
        let mut sent = i.clone();
        sent.state = IntentState::Sent;
        sent.sent_at = Some(Utc::now());
        let n = store
            .save_intent_with_version(&sent, &version(&sent, 0))
            .await
            .unwrap();
        assert_eq!(n, 2);
        assert_eq!(store.get_intent(i.id).await.unwrap(), Some(sent.clone()));
        assert_eq!(store.list_versions(i.id).await.unwrap().len(), 2);

        // A failing save (slug taken) appends no version.
        let other = intent(r.id, "two");
        store
            .insert_intent(&other, &version(&other, 1))
            .await
            .unwrap();
        let mut clash = sent.clone();
        clash.slug = "two".into();
        let err = store
            .save_intent_with_version(&clash, &version(&clash, 0))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "got {err:?}");
        assert_eq!(store.get_intent(i.id).await.unwrap(), Some(sent));
        assert_eq!(store.list_versions(i.id).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_intents_filters_and_orders_by_update() {
        let (store, r) = store_with_repo().await;
        let r2 = repo("/repos/j");
        store.insert_repo(&r2).await.unwrap();
        let base = Utc::now();
        let mut made = Vec::new();
        for (n, (repo_id, state)) in [
            (r.id, IntentState::Draft),
            (r.id, IntentState::Queued),
            (r2.id, IntentState::Draft),
        ]
        .into_iter()
        .enumerate()
        {
            let mut i = intent(repo_id, &format!("s{n}"));
            i.state = state;
            i.updated_at = base + chrono::Duration::seconds(n as i64);
            store.insert_intent(&i, &version(&i, 1)).await.unwrap();
            made.push(i);
        }
        let all = store.list_intents(IntentFilter::default()).await.unwrap();
        let ids: Vec<_> = all.iter().map(|i| i.id).collect();
        assert_eq!(ids, vec![made[2].id, made[1].id, made[0].id]);
        let drafts = store
            .list_intents(IntentFilter {
                state: Some(IntentState::Draft),
                repo_id: None,
            })
            .await
            .unwrap();
        assert_eq!(drafts.len(), 2);
        let both = store
            .list_intents(IntentFilter {
                state: Some(IntentState::Draft),
                repo_id: Some(r.id),
            })
            .await
            .unwrap();
        assert_eq!(
            both.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![made[0].id]
        );
    }

    #[tokio::test]
    async fn queued_and_polishing_queries() {
        let (store, r) = store_with_repo().await;
        let base = Utc::now();
        let mut ids = Vec::new();
        for (n, offset) in [30i64, 10, 20].into_iter().enumerate() {
            let mut i = intent(r.id, &format!("q{n}"));
            i.state = IntentState::Queued;
            i.phase = Phase::Review;
            i.queued_at = Some(base + chrono::Duration::seconds(offset));
            store.insert_intent(&i, &version(&i, 1)).await.unwrap();
            ids.push(i.id);
        }
        let polishing = intent(r.id, "p");
        store
            .insert_intent(&polishing, &version(&polishing, 1))
            .await
            .unwrap();
        let q = store.queued_intents().await.unwrap();
        assert_eq!(
            q.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![ids[1], ids[2], ids[0]]
        );
        let p = store.intents_in_phase_polishing().await.unwrap();
        assert_eq!(
            p.iter().map(|i| i.id).collect::<Vec<_>>(),
            vec![polishing.id]
        );
    }

    #[tokio::test]
    async fn settings_round_trip() {
        let store = Store::open_in_memory().await.unwrap();
        assert_eq!(store.get_setting::<u32>("limit").await.unwrap(), None);
        store.set_setting("limit", &3u32).await.unwrap();
        assert_eq!(store.get_setting::<u32>("limit").await.unwrap(), Some(3));
        store.set_setting("limit", &5u32).await.unwrap();
        assert_eq!(store.get_setting::<u32>("limit").await.unwrap(), Some(5));
    }

    #[tokio::test]
    async fn agent_intent_id_and_prompt_append_round_trip() {
        let (store, r) = store_with_repo().await;
        let i = intent(r.id, "one");
        store.insert_intent(&i, &version(&i, 1)).await.unwrap();
        let mut a = agent(r.id, "Earth");
        a.intent_id = Some(i.id);
        store.insert_agent(&a).await.unwrap();
        assert_eq!(store.get_agent(a.id).await.unwrap(), Some(a.clone()));
        assert_eq!(
            store.list_agents(true).await.unwrap()[0].intent_id,
            Some(i.id)
        );
        assert_eq!(store.agent_system_prompt_append(a.id).await.unwrap(), None);
        store
            .set_agent_system_prompt_append(a.id, "be nice")
            .await
            .unwrap();
        assert_eq!(
            store
                .agent_system_prompt_append(a.id)
                .await
                .unwrap()
                .as_deref(),
            Some("be nice")
        );
    }

    #[tokio::test]
    async fn database_from_0001_migrates_without_touching_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("old.db");
        let r = repo("/repos/old");
        let a = agent(r.id, "Old");
        {
            let opts = SqliteConnectOptions::new()
                .filename(&db)
                .create_if_missing(true)
                .foreign_keys(true);
            let pool = SqlitePool::connect_with(opts).await.unwrap();
            let mut m = sqlx::migrate!("./migrations");
            m.migrations = m
                .migrations
                .iter()
                .filter(|x| x.version == 1)
                .cloned()
                .collect::<Vec<_>>()
                .into();
            m.run(&pool).await.unwrap();
            sqlx::query(
                "INSERT INTO repos (id, path, default_branch, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(r.id.to_string())
            .bind("/repos/old")
            .bind("main")
            .bind(ts(r.created_at))
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO agents (id, name, color_json, repo_id, worktree_path, branch, \
                 harness_session_id, state, created_at, closed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(a.id.to_string())
            .bind(&a.name)
            .bind(serde_json::to_string(&a.color).unwrap())
            .bind(r.id.to_string())
            .bind(a.worktree_path.to_string_lossy().into_owned())
            .bind(&a.branch)
            .bind(a.harness_session_id.as_deref())
            .bind(enum_to_str(&a.state).unwrap())
            .bind(ts(a.created_at))
            .bind(None::<String>)
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        }
        let store = Store::open(&db).await.unwrap();
        let got = store.get_agent(a.id).await.unwrap().unwrap();
        assert_eq!(got, a);
        assert_eq!(got.intent_id, None);
        assert_eq!(store.agent_system_prompt_append(a.id).await.unwrap(), None);
        assert_eq!(store.get_repo(r.id).await.unwrap(), Some(r));
    }
}
