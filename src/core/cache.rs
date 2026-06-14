use super::Cade;
use crate::types::CadeLayer;
use anyhow::Result;

impl Cade {
    pub(super) fn get_cached_layer(&self, dir: &str, token: &str) -> Result<Option<CadeLayer>> {
        match self.db.query_row(
            "SELECT Data FROM LayerCache WHERE Dir=(?1) AND Token=(?2)",
            [dir, token],
            |row| row.get::<_, String>(0),
        ) {
            Ok(data) => Ok(serde_json::from_str(&data).ok()),
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
            "INSERT OR REPLACE INTO LayerCache (Dir, Token, Data) VALUES (?1, ?2, ?3)",
            [dir, token, &data],
        )?;
        Ok(())
    }
}
