//! 设置读写（单行 JSON，键固定为 `settings`）。

use mhcore::Settings;
use rusqlite::{params, OptionalExtension};

use crate::{Result, Store, StoreError};

impl Store {
    pub fn load_settings(&self) -> Result<Settings> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let stored: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = 'settings'", [], |r| r.get(0))
            .optional()?;
        Ok(stored
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default())
    }

    pub fn save_settings(&mut self, settings: &Settings) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let json =
            serde_json::to_string(settings).map_err(|e| StoreError::Invalid(e.to_string()))?;
        conn.execute(
            "INSERT INTO settings(key, value) VALUES ('settings', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![json],
        )?;
        Ok(())
    }
}
