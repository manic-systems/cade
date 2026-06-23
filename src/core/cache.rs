use super::watch::LAYER_CACHE_VERSION;
use super::{Cade, sessions::shell_gc_root_ttl};
use crate::types::CadeLayer;
use anyhow::{Context, Result};

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Cade {
    pub(super) fn ensure_layer_cache_schema(conn: &rusqlite::Connection) -> Result<()> {
        let has_last_used = conn
            .prepare("PRAGMA table_info(LayerCache)")
            .context("inspect LayerCache schema")?
            .query_map([], |row| row.get::<_, String>(1))
            .context("read LayerCache schema")?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .iter()
            .any(|column| column == "LastUsed");

        if !has_last_used {
            conn.execute(
                "ALTER TABLE LayerCache ADD COLUMN LastUsed INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .context("add LastUsed to LayerCache")?;
        }

        Ok(())
    }

    pub(super) fn prune_stale_layer_cache(conn: &rusqlite::Connection) -> Result<()> {
        let prefix = format!("{LAYER_CACHE_VERSION}\n%");
        conn.execute(
            "DELETE FROM LayerCache WHERE Token != ?1 AND Token NOT LIKE ?2",
            [LAYER_CACHE_VERSION, &prefix],
        )?;
        let cutoff = now_secs().saturating_sub(shell_gc_root_ttl().as_secs());
        conn.execute("DELETE FROM LayerCache WHERE LastUsed < ?1", [cutoff])?;
        Ok(())
    }

    pub(super) fn get_cached_layer(&self, dir: &str, token: &str) -> Result<Option<CadeLayer>> {
        match self.db.query_row(
            "SELECT Data FROM LayerCache WHERE Dir=(?1) AND Token=(?2)",
            [dir, token],
            |row| row.get::<_, String>(0),
        ) {
            Ok(data) => {
                let Some(layer) = serde_json::from_str(&data).ok() else {
                    return Ok(None);
                };
                self.db.execute(
                    "UPDATE LayerCache SET LastUsed=(?3) WHERE Dir=(?1) AND Token=(?2)",
                    (dir, token, now_secs()),
                )?;
                Ok(Some(layer))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub(super) fn store_cached_layer(
        &self,
        dir: &str,
        token: &str,
        layer: &CadeLayer,
    ) -> Result<()> {
        let data = serde_json::to_string(layer)?;
        self.db.execute(
            "INSERT OR REPLACE INTO LayerCache (Dir, Token, Data, LastUsed) VALUES (?1, ?2, ?3, ?4)",
            (dir, token, &data, now_secs()),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_stale_layer_cache_removes_old_versions_only() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE LayerCache (
                Dir TEXT PRIMARY KEY,
                Token TEXT NOT NULL,
                Data TEXT NOT NULL,
                LastUsed INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO LayerCache (Dir, Token, Data, LastUsed) VALUES (?1, ?2, ?3, ?4)",
            ("/old", "layer-cache-v2\n/a:present:1:1", "{}", now_secs()),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO LayerCache (Dir, Token, Data, LastUsed) VALUES (?1, ?2, ?3, ?4)",
            (
                "/current",
                &format!("{LAYER_CACHE_VERSION}\n/a:present:1:1"),
                "{}",
                now_secs(),
            ),
        )
        .unwrap();

        Cade::prune_stale_layer_cache(&conn).unwrap();

        let dirs: Vec<String> = conn
            .prepare("SELECT Dir FROM LayerCache ORDER BY Dir")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(dirs, ["/current"]);
    }

    #[test]
    fn ensure_layer_cache_schema_adds_last_used_to_old_table() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE LayerCache (
                Dir TEXT PRIMARY KEY,
                Token TEXT NOT NULL,
                Data TEXT NOT NULL
            );",
        )
        .unwrap();

        Cade::ensure_layer_cache_schema(&conn).unwrap();

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(LayerCache)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(columns.iter().any(|column| column == "LastUsed"));
    }

    #[test]
    fn prune_stale_layer_cache_removes_expired_current_version_rows() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE LayerCache (
                Dir TEXT PRIMARY KEY,
                Token TEXT NOT NULL,
                Data TEXT NOT NULL,
                LastUsed INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO LayerCache (Dir, Token, Data, LastUsed) VALUES (?1, ?2, ?3, ?4)",
            (
                "/expired",
                &format!("{LAYER_CACHE_VERSION}\n/a:present:1:1"),
                "{}",
                1_u64,
            ),
        )
        .unwrap();

        Cade::prune_stale_layer_cache(&conn).unwrap();

        let count: u64 = conn
            .query_row("SELECT count(*) FROM LayerCache", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
