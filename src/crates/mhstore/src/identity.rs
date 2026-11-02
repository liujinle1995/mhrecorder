//! 身份相关的派生数据：面板视觉指纹、外观特征。
//!
//! 这两张表是"包裹截图不认识人"问题的全部依据，所以也必须在导出/备份里
//! 完整带走——换台电脑重装之后它们丢了，等于把积累的识别能力清零。

use mhcore::{AppearanceSignature, IdentityRef, PanelFingerprint, ScreenKind};
use rusqlite::params;

use crate::{parse_rgb, screen_kind_str, Result, Store};

impl Store {
    /// Remember the latest capture of one panel kind for a character.
    ///
    /// Only the newest one is kept: the fingerprint is meant to describe what
    /// that character's panel looks like *now*, and item grids change.
    pub fn save_fingerprint(
        &mut self,
        character_id: &str,
        fingerprint: &PanelFingerprint,
        updated_at: i64,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO character_fingerprints(character_id, screen_kind, thumb, score, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(character_id, screen_kind) DO UPDATE SET
                 thumb = excluded.thumb,
                 score = COALESCE(excluded.score, character_fingerprints.score),
                 updated_at = excluded.updated_at",
            params![
                character_id,
                screen_kind_str(fingerprint.screen),
                fingerprint.thumb,
                fingerprint.score,
                updated_at
            ],
        )?;
        Ok(())
    }

    /// Every character's stored fingerprints, for the recognition pipeline.
    ///
    /// The 人物评分 is carried forward across rows even when a later capture did
    /// not show one, so a bag captured while the score was off-screen can still
    /// be matched by score.
    pub fn list_identity_refs(&self) -> Result<Vec<IdentityRef>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        // LEFT JOIN on both sides: a character that has an appearance signature
        // but no fingerprint yet (or the other way round) is still a candidate.
        let mut stmt = conn.prepare(
            "SELECT c.id, c.nickname, f.screen_kind, f.thumb, f.score,
                    a.hair_name, a.hair_rgb, a.weapon_name, a.weapon_rgb,
                    c.game_id, c.server, c.level, c.avatar_thumb, c.updated_at
             FROM characters c
             LEFT JOIN character_fingerprints f ON f.character_id = c.id
             LEFT JOIN character_appearance a ON a.character_id = c.id
             ORDER BY c.updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, Option<i64>>(11)?,
                r.get::<_, Option<String>>(12)?,
                r.get::<_, i64>(13)?,
            ))
        })?;

        let mut refs: Vec<IdentityRef> = Vec::new();
        for row in rows {
            let (
                id, nickname, kind, thumb, score, hair_name, hair_rgb, weapon_name, weapon_rgb,
                game_id, server, level, avatar_thumb, updated_at,
            ) = row?;
            let entry = match refs.iter_mut().find(|r| r.character_id == id) {
                Some(e) => e,
                None => {
                    refs.push(IdentityRef {
                        character_id: id.clone(),
                        nickname: nickname.clone(),
                        game_id: game_id.clone(),
                        server: server.clone(),
                        level,
                        fingerprints: Vec::new(),
                        appearance: None,
                        avatar_thumb: avatar_thumb.clone(),
                        last_seen_at: Some(updated_at),
                    });
                    refs.last_mut().unwrap()
                }
            };

            if let Some(kind) = kind {
                let screen = serde_json::from_value::<ScreenKind>(serde_json::Value::String(kind))
                    .unwrap_or(ScreenKind::Unknown);
                if let Some(thumb) = thumb {
                    entry.fingerprints.push(PanelFingerprint { screen, thumb, score });
                }
            }

            if entry.appearance.is_none() {
                if let (Some(hair_name), Some(hair_rgb)) = (hair_name, hair_rgb) {
                    entry.appearance = Some(AppearanceSignature {
                        hair_name,
                        hair_rgb: parse_rgb(&hair_rgb).unwrap_or([0, 0, 0]),
                        weapon_name,
                        weapon_rgb: weapon_rgb.as_deref().and_then(parse_rgb),
                    });
                }
            }
        }
        Ok(refs)
    }

    /// 原始指纹/外观行，只给导出用（`IdentityRef` 会按角色聚合，导出要平的）。
    pub(crate) fn export_identity_rows(
        &self,
    ) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
        let refs = self.list_identity_refs()?;
        let mut fingerprints = Vec::new();
        let mut appearances = Vec::new();
        for r in refs {
            for f in &r.fingerprints {
                fingerprints.push(serde_json::json!({
                    "characterId": r.character_id,
                    "screenKind": f.screen,
                    "thumb": f.thumb,
                    "score": f.score,
                }));
            }
            if let Some(a) = &r.appearance {
                appearances.push(serde_json::json!({
                    "characterId": r.character_id,
                    "hairName": a.hair_name,
                    "hairRgb": a.hair_rgb,
                    "weaponName": a.weapon_name,
                    "weaponRgb": a.weapon_rgb,
                }));
            }
        }
        Ok((fingerprints, appearances))
    }
}
