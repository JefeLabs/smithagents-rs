//! SQLite persistence for repos, agents and chat messages.

use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use smithagents_core::agent::{Agent, AgentState, ChatMessage, Repo, Role};
use smithagents_core::ids::{AgentId, MessageId, RepoId};
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
    #[error("corrupt stored value: {0}")]
    Corrupt(String),
}

fn corrupt(e: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(e.to_string())
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
    })
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

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Opens (creating if needed) the database file and runs migrations.
    pub async fn open(path: &Path) -> Result<Store, StoreError> {
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
        Self::migrated(pool).await
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
        sqlx::query(
            "INSERT INTO agents (id, name, color_json, repo_id, worktree_path, branch, \
             harness_session_id, state, created_at, closed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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

    pub async fn list_messages(
        &self,
        agent_id: AgentId,
        after_seq: u64,
        limit: u32,
    ) -> Result<Vec<ChatMessage>, StoreError> {
        let after = i64::try_from(after_seq).map_err(corrupt)?;
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
    async fn duplicate_repo_path_is_rejected() {
        let store = Store::open_in_memory().await.unwrap();
        store.insert_repo(&repo("/repos/a")).await.unwrap();
        let err = store.insert_repo(&repo("/repos/a")).await.unwrap_err();
        assert!(matches!(err, StoreError::DuplicateRepoPath), "got {err:?}");
    }
}
