//! 角色仓储：查询、身份更正、识别结果落库。

use mhcore::RecognitionResult;
use rusqlite::{params, OptionalExtension};

use crate::items::record_seen_items;
use crate::{
    now_ms, row_to_character, screen_kind_str, CharacterDetail, Result, Store, StoreError,
    CHARACTER_COLUMNS,
};

impl Store {
    pub fn list_characters(&self) -> Result<Vec<CharacterDetail>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {CHARACTER_COLUMNS},
                    (SELECT COUNT(*) FROM snapshots s WHERE s.character_id = characters.id),
                    (SELECT MAX(recorded_at) FROM snapshots s WHERE s.character_id = characters.id)
             FROM characters
             ORDER BY updated_at DESC"
        ))?;
        let rows = stmt.query_map([], |row| {
            let snap_count: i64 = row.get(13)?;
            let last_at: Option<i64> = row.get(14)?;
            Ok(CharacterDetail {
                character: row_to_character(row)?,
                snapshot_count: snap_count,
                last_recorded_at: last_at,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_character(&self, id: &str) -> Result<CharacterDetail> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(&format!(
            "SELECT {CHARACTER_COLUMNS},
                    (SELECT COUNT(*) FROM snapshots s WHERE s.character_id = characters.id),
                    (SELECT MAX(recorded_at) FROM snapshots s WHERE s.character_id = characters.id)
             FROM characters WHERE id = ?1"
        ))?;
        let mut rows = stmt.query_map(params![id], |row| {
            let snap_count: i64 = row.get(13)?;
            let last_at: Option<i64> = row.get(14)?;
            Ok(CharacterDetail {
                character: row_to_character(row)?,
                snapshot_count: snap_count,
                last_recorded_at: last_at,
            })
        })?;
        match rows.next() {
            Some(Ok(d)) => Ok(d),
            _ => Err(StoreError::NotFound(format!("character {id}"))),
        }
    }

    /// Find candidate characters for identity association by game id or
    /// nickname.
    ///
    /// Returns **every** match: a nickname is not unique in this game (two
    /// servers, two people), so the caller has to decide with the full list
    /// rather than take the first row.
    pub fn find_identity_candidates(
        &self,
        game_id: Option<&str>,
        nickname: Option<&str>,
    ) -> Result<Vec<mhcore::Character>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut out = Vec::new();

        if let Some(gid) = game_id.filter(|s| !s.trim().is_empty()) {
            let mut stmt = conn.prepare(&format!(
                "SELECT {CHARACTER_COLUMNS} FROM characters WHERE game_id = ?1"
            ))?;
            let rows = stmt.query_map(params![gid], row_to_character)?;
            for r in rows {
                out.push(r?);
            }
        }

        if let Some(nick) = nickname.filter(|s| !s.trim().is_empty()) {
            let mut stmt = conn.prepare(&format!(
                "SELECT {CHARACTER_COLUMNS} FROM characters WHERE nickname = ?1"
            ))?;
            let rows = stmt.query_map(params![nick], row_to_character)?;
            for r in rows {
                let c = r?;
                if !out.iter().any(|existing| existing.id == c.id) {
                    out.push(c);
                }
            }
        }

        Ok(out)
    }

    /// Insert or update a character from a recognition result and append a
    /// snapshot. Returns the character id.
    pub fn save_recognition(
        &mut self,
        result: &RecognitionResult,
        source_screens: &[String],
    ) -> Result<String> {
        let now = now_ms();
        let character_id = self.transaction(|tx| {
            // A 包裹 capture shows no nickname at all. Falling back to the
            // placeholder is fine for a brand-new row, but it must never
            // overwrite the nickname a character already has — that is how a bag
            // capture used to rename 莫莫莫 to 未命名角色.
            let incoming_nickname = result
                .character
                .nickname
                .clone()
                .filter(|s| !s.trim().is_empty());
            let nickname = incoming_nickname
                .clone()
                .unwrap_or_else(|| "未命名角色".to_string());
            let game_id = result.character.game_id.clone().filter(|s| !s.trim().is_empty());

            // Prefer an explicit id from the pipeline, then match by game id,
            // then fall back to nickname. Manual corrections always win. With
            // neither an id nor a nickname (a 包裹 capture that could not be
            // linked) there is nothing to match on, so a row is created rather
            // than guessed at.
            let existing: Option<String> = if let Some(cid) = result.character_id.as_deref() {
                tx.query_row("SELECT id FROM characters WHERE id = ?1", params![cid], |r| r.get(0))
                    .optional()?
            } else if let Some(gid) = game_id.as_deref() {
                tx.query_row(
                    "SELECT id FROM characters WHERE game_id = ?1 ORDER BY updated_at DESC LIMIT 1",
                    params![gid],
                    |r| r.get(0),
                )
                .optional()?
            } else if let Some(nick) = incoming_nickname.as_deref() {
                tx.query_row(
                    "SELECT id FROM characters WHERE nickname = ?1 ORDER BY updated_at DESC LIMIT 1",
                    params![nick],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                None
            };

            let stats_json = serde_json::to_string(&result.stats).unwrap_or_else(|_| "{}".into());
            let currency_json =
                serde_json::to_string(&result.currencies).unwrap_or_else(|_| "{}".into());

            let character_id = match existing {
                Some(id) => {
                    tx.execute(
                        // COALESCE / CASE keep what we already know when the new
                        // capture simply does not show it: a 包裹 screenshot has
                        // no nickname and no combat stats, and saving it must not
                        // blank out the 人物属性 data recorded a minute earlier.
                        "UPDATE characters
                         SET nickname = COALESCE(?1, nickname),
                             game_id = COALESCE(?2, game_id),
                             level = COALESCE(?3, level),
                             school = COALESCE(?4, school),
                             identity_confidence = MAX(identity_confidence, ?5),
                             latest_stats = CASE WHEN ?6 = '{}' THEN latest_stats ELSE ?6 END,
                             latest_currencies = CASE WHEN ?7 = '{}' THEN latest_currencies ELSE ?7 END,
                             updated_at = ?8,
                             avatar_path = COALESCE(?10, avatar_path),
                             avatar_thumb = COALESCE(?11, avatar_thumb)
                         WHERE id = ?9",
                        params![
                            incoming_nickname,
                            game_id,
                            result.character.level,
                            result.character.school,
                            result.confidence,
                            stats_json,
                            currency_json,
                            now,
                            id,
                            // Store the cropped portrait as a base64 PNG in the
                            // avatar_path column; COALESCE keeps the previous one
                            // when a later capture had no avatar.
                            result.avatar_png_base64,
                            result.avatar_thumb,
                        ],
                    )?;
                    id
                }
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO characters
                         (id, nickname, game_id, level, school, server, identity_confidence,
                          identity_confirmed, avatar_path, avatar_thumb, latest_stats,
                          latest_currencies, created_at, updated_at)
                         VALUES (?1,?2,?3,?4,?5,?6,?7,0,?12,?13,?8,?9,?10,?11)",
                        params![
                            id,
                            nickname,
                            game_id,
                            result.character.level,
                            result.character.school,
                            result.character.server,
                            result.confidence,
                            stats_json,
                            currency_json,
                            now,
                            now,
                            result.avatar_png_base64,
                            result.avatar_thumb,
                        ],
                    )?;
                    id
                }
            };

            tx.execute(
                "INSERT INTO snapshots
                 (id, character_id, recorded_at, stats, currencies, equipment, inventory,
                  confidence, source_screens)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    character_id,
                    now,
                    serde_json::to_string(&result.stats).unwrap_or_else(|_| "{}".into()),
                    serde_json::to_string(&result.currencies).unwrap_or_else(|_| "{}".into()),
                    serde_json::to_string(&result.equipment).unwrap_or_else(|_| "[]".into()),
                    serde_json::to_string(&result.inventory).unwrap_or_else(|_| "[]".into()),
                    result.confidence,
                    serde_json::to_string(source_screens).unwrap_or_else(|_| "[]".into()),
                ],
            )?;

            // Remember what this character's panels looked like, so a later 包裹
            // capture — which renders no 游戏ID at all — can be linked back.
            for fingerprint in &result.fingerprints {
                tx.execute(
                    "INSERT INTO character_fingerprints
                     (character_id, screen_kind, thumb, score, updated_at)
                     VALUES (?1,?2,?3,?4,?5)
                     ON CONFLICT(character_id, screen_kind) DO UPDATE SET
                         thumb = excluded.thumb,
                         score = COALESCE(excluded.score, character_fingerprints.score),
                         updated_at = excluded.updated_at",
                    params![
                        character_id,
                        screen_kind_str(fingerprint.screen),
                        fingerprint.thumb,
                        fingerprint.score,
                        now
                    ],
                )?;
            }

            // 发色 / 武器. Kept apart from the panel thumbnails on purpose: the
            // thumbnails describe a picture that goes stale when the bag's items
            // change, this describes the character and does not.
            if let Some(app) = result.appearance.as_ref() {
                let hair_rgb =
                    serde_json::to_string(&app.hair_rgb).unwrap_or_else(|_| "[0,0,0]".to_string());
                let weapon_rgb = app
                    .weapon_rgb
                    .map(|v| serde_json::to_string(&v).unwrap_or_else(|_| String::new()));
                tx.execute(
                    "INSERT INTO character_appearance
                     (character_id, hair_name, hair_rgb, weapon_name, weapon_rgb, updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6)
                     ON CONFLICT(character_id) DO UPDATE SET
                         hair_name = excluded.hair_name,
                         hair_rgb = excluded.hair_rgb,
                         weapon_name = COALESCE(excluded.weapon_name, character_appearance.weapon_name),
                         weapon_rgb = COALESCE(excluded.weapon_rgb, character_appearance.weapon_rgb),
                         updated_at = excluded.updated_at",
                    params![
                        character_id,
                        app.hair_name,
                        hair_rgb,
                        app.weapon_name,
                        weapon_rgb,
                        now
                    ],
                )?;
            }

            // 物品字典：把这一批里见过的装备/背包物品登记一次，将来做图标识别
            // 时这就是现成的标注来源。
            record_seen_items(tx, &character_id, result, now)?;

            Ok(character_id)
        })?;

        Ok(character_id)
    }

    /// Apply a human correction to identity fields and mark it confirmed.
    pub fn update_character_identity(
        &mut self,
        id: &str,
        nickname: Option<&str>,
        game_id: Option<&str>,
        level: Option<i64>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE characters
             SET nickname = COALESCE(?1, nickname),
                 game_id  = COALESCE(?2, game_id),
                 level    = COALESCE(?3, level),
                 identity_confirmed = 1,
                 identity_confidence = 1.0,
                 updated_at = ?4
             WHERE id = ?5",
            params![nickname, game_id, level, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("character {id}")));
        }
        Ok(())
    }

    pub fn confirm_character(&mut self, id: &str, confidence: f64) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE characters SET identity_confirmed = 1, identity_confidence = ?1,
                                  updated_at = ?2
             WHERE id = ?3",
            params![confidence, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("character {id}")));
        }
        Ok(())
    }

    pub fn delete_character(&mut self, id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute("DELETE FROM characters WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("character {id}")));
        }
        Ok(())
    }
}
