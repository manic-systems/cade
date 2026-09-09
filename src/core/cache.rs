use std::{
    path::PathBuf,
    time::{
        SystemTime,
        UNIX_EPOCH,
    },
};

use anyhow::{
    Context as _,
    Result,
};

use crate::{
    core::{
        Cade,
        sessions::shell_gc_root_ttl,
        watch::LAYER_CACHE_VERSION,
    },
    types::layer::CachedLayer,
};

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |dur| dur.as_secs())
}

pub(super) fn ensure_layer_cache_schema(conn: &rusqlite::Connection) -> Result<()> {
    let mut statement = conn
        .prepare("PRAGMA table_info(LayerCache)")
        .context("inspect LayerCache schema")?;
    let listed = statement
        .query_map([], |row| row.get::<_, String>(1))
        .context("read LayerCache schema")?;
    let mut columns = Vec::new();
    for entry in listed {
        columns.push(entry?);
    }
    let has_last_used = columns.iter().any(|column| column == "LastUsed");

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

pub(super) fn prune_stale_watch_discovery(conn: &rusqlite::Connection) -> Result<()> {
    let prefix = format!("{LAYER_CACHE_VERSION}\n%");
    conn.execute(
        "DELETE FROM WatchDiscovery WHERE Token != ?1 AND Token NOT LIKE ?2",
        [LAYER_CACHE_VERSION, &prefix],
    )?;
    let cutoff = now_secs().saturating_sub(shell_gc_root_ttl().as_secs());
    conn.execute("DELETE FROM WatchDiscovery WHERE LastUsed < ?1", [cutoff])?;
    Ok(())
}

pub(super) fn get_watch_discovery(cade: &Cade, dir: &str) -> Option<(Vec<PathBuf>, String)> {
    let (files, token) = cade
        .db
        .query_row(
            "SELECT Files, Token FROM WatchDiscovery WHERE Dir=(?1)",
            [dir],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()?;
    cade.db
        .execute(
            "UPDATE WatchDiscovery SET LastUsed=(?2) WHERE Dir=(?1)",
            (dir, now_secs()),
        )
        .ok()?;
    Some((serde_json::from_str(&files).ok()?, token))
}

pub(super) fn store_watch_discovery(
    cade: &Cade,
    dir: &str,
    files: &[PathBuf],
    token: &str,
) -> Result<()> {
    let data = serde_json::to_string(files)?;
    cade.db.execute(
        "INSERT OR REPLACE INTO WatchDiscovery (Dir, Token, Files, LastUsed) VALUES (?1, ?2, ?3, \
         ?4)",
        (dir, token, &data, now_secs()),
    )?;
    Ok(())
}

pub(super) fn get_cached_layer(cade: &Cade, dir: &str, token: &str) -> Result<Option<CachedLayer>> {
    match cade.db.query_row(
        "SELECT Data FROM LayerCache WHERE Dir=(?1) AND Token=(?2)",
        [dir, token],
        |row| row.get::<_, String>(0),
    ) {
        Ok(data) => {
            let Some(layer) = serde_json::from_str(&data).ok() else {
                return Ok(None);
            };
            cade.db.execute(
                "UPDATE LayerCache SET LastUsed=(?3) WHERE Dir=(?1) AND Token=(?2)",
                (dir, token, now_secs()),
            )?;
            Ok(Some(layer))
        },
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn store_cached_layer(
    cade: &Cade,
    dir: &str,
    token: &str,
    layer: &CachedLayer,
) -> Result<()> {
    let data = serde_json::to_string(layer)?;
    cade.db.execute(
        "INSERT OR REPLACE INTO LayerCache (Dir, Token, Data, LastUsed) VALUES (?1, ?2, ?3, ?4)",
        (dir, token, &data, now_secs()),
    )?;
    Ok(())
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

        prune_stale_layer_cache(&conn).unwrap();

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

        ensure_layer_cache_schema(&conn).unwrap();

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

        prune_stale_layer_cache(&conn).unwrap();

        let count: u64 = conn
            .query_row("SELECT count(*) FROM LayerCache", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
