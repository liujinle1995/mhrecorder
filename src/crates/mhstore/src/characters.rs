//! 角色仓储：查询、身份更正、识别结果落库。
//!
//! 这里是身份安全的**最后一道闸门**。界面上的"请选择归属"只是第一道：`recognize.save`
//! 是后端接口，任何客户端（包括将来别的前端、命令行、脚本）都能直接调。所以
//! 拿不准的归属必须在这里被拒绝，而不是指望调用方先问过用户。
//!
//! 判定顺序（对应产品要求）：
//!
//! | 情况 | 行为 |
//! |---|---|
//! | 明确 `character_id` 且存在 | 保存到该角色 |
//! | 游戏ID 唯一匹配 | 保存 |
//! | 游戏ID 匹配多个 | **拒绝**，要求人工处理 |
//! | 昵称唯一匹配 | 保存（置信度按昵称算，未确认） |
//! | 昵称匹配多个 | **拒绝**，要求人工指定 |
//! | 什么都匹配不上、也没有身份字段 | 进待归属表，**不建角色** |

use mhcore::{CombatStats, Currencies, IdentitySource, RecognitionResult};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::Serialize;

use crate::items::record_seen_items;
use crate::{
    from_json, now_ms, row_to_character, screen_kind_str, CharacterDetail, Result, Store,
    StoreError, CHARACTER_COLUMNS,
};

/// 一次保存的结果。
///
/// `Pending` 不是失败：识别本身成功了，只是"这批数据属于谁"没有证据。以前这种情况
/// 会造一个「未命名角色」，它永远不会再被匹配到，却会参与资产统计。
#[derive(Debug, Clone)]
pub enum SaveOutcome {
    Saved {
        character_id: String,
        /// 是不是这次新建的角色。
        created: bool,
    },
    Pending {
        pending_id: String,
        reason: String,
    },
}

impl SaveOutcome {
    pub fn character_id(&self) -> Option<&str> {
        match self {
            SaveOutcome::Saved { character_id, .. } => Some(character_id),
            SaveOutcome::Pending { .. } => None,
        }
    }
}

/// 待归属记录（列表用，不带完整 payload）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRecord {
    pub id: String,
    pub created_at: i64,
    pub reason: String,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game_id: Option<String>,
    pub screenshot_count: usize,
    pub screens: Vec<String>,
    /// 有没有读到钱 —— 界面用它判断这条记录值不值得优先处理。
    pub has_currencies: bool,
}

/// 保存目标：这一批数据该落到哪。
enum Target {
    Existing(String),
    New,
    Pending(String),
}

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
    /// snapshot.
    ///
    /// `Err(StoreError::Identity(_))` means "the data is fine, but I refuse to
    /// guess whose it is" — the caller should surface that text to the user
    /// instead of retrying.
    pub fn save_recognition(
        &mut self,
        result: &RecognitionResult,
        source_screens: &[String],
    ) -> Result<SaveOutcome> {
        let now = now_ms();

        self.transaction(|tx| match resolve_target(tx, result)? {
            Target::Existing(id) => {
                update_character(tx, &id, result, now)?;
                persist_evidence(tx, &id, result, source_screens, now)?;
                Ok(SaveOutcome::Saved { character_id: id, created: false })
            }
            Target::New => {
                let id = insert_character(tx, result, now)?;
                persist_evidence(tx, &id, result, source_screens, now)?;
                Ok(SaveOutcome::Saved { character_id: id, created: true })
            }
            Target::Pending(reason) => {
                let pending_id = insert_pending(tx, result, source_screens, &reason, now)?;
                Ok(SaveOutcome::Pending { pending_id, reason })
            }
        })
    }

    // ------------------------------------------------------------------
    // 待归属记录
    // ------------------------------------------------------------------

    pub fn list_pending(&self, limit: usize) -> Result<Vec<PendingRecord>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, created_at, reason, confidence, source_screens, payload
             FROM pending_recognitions
             ORDER BY created_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let id: String = row.get(0)?;
            let created_at: i64 = row.get(1)?;
            let reason: String = row.get(2)?;
            let confidence: f64 = row.get(3)?;
            let screens: String = row.get(4)?;
            let payload: String = row.get(5)?;
            Ok((id, created_at, reason, confidence, screens, payload))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (id, created_at, reason, confidence, screens, payload) = row?;
            // payload 坏掉不该让整个列表失败：显示一条"无法解析"比什么都看不到好。
            let result: RecognitionResult = serde_json::from_str(&payload).unwrap_or_else(|_| {
                RecognitionResult::new(IdentitySource::VisualMatch)
            });
            out.push(PendingRecord {
                id,
                created_at,
                reason,
                confidence,
                nickname: result.character.nickname.clone(),
                game_id: result.character.game_id.clone(),
                screenshot_count: result.screenshot_count,
                screens: serde_json::from_str(&screens).unwrap_or_default(),
                has_currencies: result.currencies.gold.is_some() || result.currencies.silver.is_some(),
            });
        }
        Ok(out)
    }

    pub fn pending_count(&self) -> Result<i64> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM pending_recognitions", [], |r| r.get(0))?;
        Ok(count)
    }

    /// 把一条待归属记录归到指定角色名下。
    ///
    /// 用显式 `character_id` 重放一次保存流程：快照、指纹、外观、物品字典都走
    /// 同一条路径，不会因为"是补归属"而少记东西。
    pub fn assign_pending(&mut self, pending_id: &str, character_id: &str) -> Result<SaveOutcome> {
        let (payload, screens): (String, String) = {
            let conn = self.conn.lock().expect("store mutex poisoned");
            conn.query_row(
                "SELECT payload, source_screens FROM pending_recognitions WHERE id = ?1",
                params![pending_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("pending {pending_id}")))?
        };

        let mut result: RecognitionResult = serde_json::from_str(&payload)
            .map_err(|e| StoreError::Invalid(format!("待归属记录已损坏：{e}")))?;
        result.character_id = Some(character_id.to_string());
        result.identity_source = IdentitySource::Manual;

        let screens: Vec<String> = serde_json::from_str(&screens).unwrap_or_default();
        let outcome = self.save_recognition(&result, &screens)?;

        // 保存成功才删除；失败时记录留着，用户还能再试一次。
        if outcome.character_id().is_some() {
            let conn = self.conn.lock().expect("store mutex poisoned");
            conn.execute("DELETE FROM pending_recognitions WHERE id = ?1", params![pending_id])?;
        }
        Ok(outcome)
    }

    pub fn discard_pending(&self, pending_id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed =
            conn.execute("DELETE FROM pending_recognitions WHERE id = ?1", params![pending_id])?;
        if changed == 0 {
            return Err(StoreError::NotFound(format!("pending {pending_id}")));
        }
        Ok(())
    }

    /// Apply a human correction to identity fields and mark it confirmed.
    pub fn update_character_identity(
        &mut self,
        id: &str,
        nickname: Option<&str>,
        game_id: Option<&str>,
        level: Option<i64>,
        server: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn.execute(
            "UPDATE characters
             SET nickname = COALESCE(?1, nickname),
                 game_id  = COALESCE(?2, game_id),
                 level    = COALESCE(?3, level),
                 server   = COALESCE(NULLIF(?4, ''), server),
                 identity_confirmed = 1,
                 identity_confidence = 1.0,
                 updated_at = ?5
             WHERE id = ?6",
            params![nickname, game_id, level, server, now_ms(), id],
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

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// 按某一列找匹配的角色 id。
///
/// `column` 只从调用点的字面量传入（`game_id` / `nickname`），不拼用户输入。
fn ids_matching(tx: &Transaction<'_>, column: &'static str, value: &str) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(&format!(
        "SELECT id FROM characters WHERE {column} = ?1 ORDER BY updated_at DESC"
    ))?;
    let rows = stmt.query_map(params![value], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// 身份闸门：这一批数据该落到哪一行。
fn resolve_target(tx: &Transaction<'_>, result: &RecognitionResult) -> Result<Target> {
    // 1. 明确的 character_id：只认库里真的有的那一行。
    //    找不到就报错，而不是"那算了另建一个"——静默新建正是重复角色的来源。
    if let Some(cid) = non_empty(result.character_id.as_deref()) {
        let exists: Option<String> = tx
            .query_row("SELECT id FROM characters WHERE id = ?1", params![cid], |r| r.get(0))
            .optional()?;
        return match exists {
            Some(id) => Ok(Target::Existing(id)),
            None => Err(StoreError::NotFound(format!("character {cid}"))),
        };
    }

    // 2. 游戏ID：唯一权威标识。
    if let Some(gid) = non_empty(result.character.game_id.as_deref()) {
        let hits = ids_matching(tx, "game_id", gid)?;
        return match hits.len() {
            0 => Ok(Target::New),
            1 => Ok(Target::Existing(hits.into_iter().next().expect("one hit"))),
            n => Err(StoreError::Identity(format!(
                "游戏ID {gid} 在库中对应 {n} 个角色，无法自动保存：请先在「角色管理」里删掉重复记录，再手动指定归属"
            ))),
        };
    }

    // 3. 昵称：同一昵称可能在不同服务器各有一个，所以只接受唯一命中。
    if let Some(nick) = non_empty(result.character.nickname.as_deref()) {
        let hits = ids_matching(tx, "nickname", nick)?;
        return match hits.len() {
            0 => Ok(Target::New),
            1 => Ok(Target::Existing(hits.into_iter().next().expect("one hit"))),
            n => Err(StoreError::Identity(format!(
                "库中有 {n} 个角色都叫「{nick}」，仅凭昵称无法确定归属：请手动选择角色，或先补录一次「人物属性」拿到游戏ID"
            ))),
        };
    }

    // 4. 什么身份都没有。
    Ok(Target::Pending(
        "这批截图没有任何身份信息（没有游戏ID、没有昵称，也没有匹配到已知角色）".to_string(),
    ))
}

/// 新角色。只有走到这里时身份才是确定的（有游戏ID 或唯一昵称）。
fn insert_character(tx: &Transaction<'_>, result: &RecognitionResult, now: i64) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let nickname = non_empty(result.character.nickname.as_deref())
        .map(str::to_string)
        .unwrap_or_else(|| "未命名角色".to_string());

    tx.execute(
        "INSERT INTO characters
         (id, nickname, game_id, level, school, server, identity_confidence,
          identity_confirmed, avatar_path, avatar_thumb, latest_stats,
          latest_currencies, created_at, updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7,0,?8,?9,?10,?11,?12,?13)",
        params![
            id,
            nickname,
            non_empty(result.character.game_id.as_deref()),
            result.character.level,
            result.character.school,
            non_empty(result.character.server.as_deref()),
            result.confidence,
            result.avatar_png_base64,
            result.avatar_thumb,
            serde_json::to_string(&result.stats).unwrap_or_else(|_| "{}".into()),
            serde_json::to_string(&result.currencies).unwrap_or_else(|_| "{}".into()),
            now,
            now,
        ],
    )?;
    Ok(id)
}

/// 更新已有角色。
///
/// 两条纪律：
/// * `COALESCE`：这次没读到的字段不要清空上次读到的（包裹截图没有昵称、没有属性）。
/// * 属性/货币按**字段**合并：一份只读到气血的截图，不能把上次的魔法和速度抹掉。
fn update_character(
    tx: &Transaction<'_>,
    id: &str,
    result: &RecognitionResult,
    now: i64,
) -> Result<()> {
    let (old_stats, old_currencies): (String, String) = tx.query_row(
        "SELECT latest_stats, latest_currencies FROM characters WHERE id = ?1",
        params![id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;

    let merged_stats = merge_stats_fields(from_json::<CombatStats>(old_stats), &result.stats);
    let merged_currencies =
        merge_currency_fields(from_json::<Currencies>(old_currencies), &result.currencies);

    tx.execute(
        "UPDATE characters
         SET nickname = COALESCE(?1, nickname),
             game_id = COALESCE(?2, game_id),
             level = COALESCE(?3, level),
             school = COALESCE(?4, school),
             server = COALESCE(NULLIF(?5, ''), server),
             identity_confidence = MAX(identity_confidence, ?6),
             latest_stats = ?7,
             latest_currencies = ?8,
             updated_at = ?9,
             avatar_path = COALESCE(?10, avatar_path),
             avatar_thumb = COALESCE(?11, avatar_thumb)
         WHERE id = ?12",
        params![
            non_empty(result.character.nickname.as_deref()),
            non_empty(result.character.game_id.as_deref()),
            result.character.level,
            result.character.school,
            // server 以前只在新角色时写入，于是"识别到服务器 / 手工改了服务器"
            // 在已有角色上永远不生效。空值仍然不能覆盖已有值。
            non_empty(result.character.server.as_deref()),
            result.confidence,
            serde_json::to_string(&merged_stats).unwrap_or_else(|_| "{}".into()),
            serde_json::to_string(&merged_currencies).unwrap_or_else(|_| "{}".into()),
            now,
            result.avatar_png_base64,
            result.avatar_thumb,
            id,
        ],
    )?;
    Ok(())
}

/// 按字段合并属性：新值有才覆盖，没有就保留旧值。
pub(crate) fn merge_stats_fields(old: CombatStats, new: &CombatStats) -> CombatStats {
    let mut out = old;
    macro_rules! fill {
        ($($field:ident),* $(,)?) => {
            $( if new.$field.is_some() { out.$field = new.$field.clone(); } )*
        };
    }
    fill!(
        hp,
        mp,
        physical_damage,
        magic_damage,
        physical_defense,
        magic_defense,
        speed,
        healing_power
    );
    out
}

/// 同样的规矩用在货币上：只读到金币的截图不该抹掉上次的银币。
pub(crate) fn merge_currency_fields(old: Currencies, new: &Currencies) -> Currencies {
    let mut out = old;
    if new.gold.is_some() {
        out.gold = new.gold.clone();
    }
    if new.silver.is_some() {
        out.silver = new.silver.clone();
    }
    out
}

/// 快照、指纹、外观、物品字典 —— 每次保存都要落下的一组。
fn persist_evidence(
    tx: &Transaction<'_>,
    character_id: &str,
    result: &RecognitionResult,
    source_screens: &[String],
    now: i64,
) -> Result<()> {
    // 快照存**这一次读到的原样**，不做字段合并：历史曲线要的是每次观察到的值，
    // 合并只属于 characters.latest_* 那张"当前状态"。
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
    record_seen_items(tx, character_id, result, now)?;

    Ok(())
}

fn insert_pending(
    tx: &Transaction<'_>,
    result: &RecognitionResult,
    source_screens: &[String],
    reason: &str,
    now: i64,
) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::to_string(result)
        .map_err(|e| StoreError::Invalid(format!("无法序列化待归属记录：{e}")))?;
    tx.execute(
        "INSERT INTO pending_recognitions
         (id, created_at, reason, confidence, source_screens, payload)
         VALUES (?1,?2,?3,?4,?5,?6)",
        params![
            id,
            now,
            reason,
            result.confidence,
            serde_json::to_string(source_screens).unwrap_or_else(|_| "[]".into()),
            payload,
        ],
    )?;
    Ok(id)
}
