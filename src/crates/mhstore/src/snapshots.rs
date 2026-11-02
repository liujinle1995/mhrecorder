//! 快照与统计。
//!
//! 统计的核心约定：**未知不是 0**。某一天没有任何一张截图带金币时，那天的
//! 金币是 `None`，不是 0——否则一张人物属性截图就能把当天的资产"清零"。

use mhcore::{
    AssetPoint, AssetSummary, CharacterAssetBar, CombatStats, Currencies, EquipmentItem,
    InventoryItem, Snapshot, StatisticsPayload,
};
use rusqlite::params;

use crate::{from_json, Result, Store};

impl Store {
    pub fn list_snapshots(&self, character_id: &str, limit: usize) -> Result<Vec<Snapshot>> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, character_id, recorded_at, stats, currencies, equipment, inventory,
                    confidence, source_screens
             FROM snapshots WHERE character_id = ?1
             ORDER BY recorded_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![character_id, limit as i64], |row| {
            Ok(Snapshot {
                id: row.get(0)?,
                character_id: row.get(1)?,
                recorded_at: row.get(2)?,
                stats: from_json::<CombatStats>(row.get(3)?),
                currencies: from_json::<Currencies>(row.get(4)?),
                equipment: from_json::<Vec<EquipmentItem>>(row.get(5)?),
                inventory: from_json::<Vec<InventoryItem>>(row.get(6)?),
                confidence: row.get(7)?,
                source_screens: from_json::<Vec<String>>(row.get(8)?),
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn statistics(&self, recent_limit: usize) -> Result<StatisticsPayload> {
        let chars = self.list_characters()?;

        let mut gold_total = 0i64;
        let mut silver_total = 0i64;
        let mut bars = Vec::with_capacity(chars.len());
        for d in &chars {
            let gold = d.character.latest_currencies.gold.as_ref().map(|v| v.value).unwrap_or(0);
            let silver =
                d.character.latest_currencies.silver.as_ref().map(|v| v.value).unwrap_or(0);
            gold_total += gold;
            silver_total += silver;
            bars.push(CharacterAssetBar {
                character_id: d.character.id.clone(),
                nickname: d.character.nickname.clone(),
                gold_wan: gold as f64 / 10_000.0,
                silver_wan: silver as f64 / 10_000.0,
            });
        }

        let by_day = self.daily_totals()?;
        let recent: Vec<AssetPoint> = by_day
            .iter()
            .rev()
            .take(recent_limit)
            .rev()
            .cloned()
            .collect();

        // 环比只在两天都有该字段的有效记录时才有意义。
        let (gold_change, silver_change) = if recent.len() >= 2 {
            let last = &recent[recent.len() - 1];
            let prev = &recent[recent.len() - 2];
            let pct = |now: Option<i64>, before: Option<i64>| -> Option<f64> {
                match (now, before) {
                    (Some(now), Some(before)) if before != 0 => {
                        Some(((now - before) as f64 / before as f64) * 100.0)
                    }
                    _ => None,
                }
            };
            (pct(last.gold, prev.gold), pct(last.silver, prev.silver))
        } else {
            (None, None)
        };

        let summary = AssetSummary {
            gold_total,
            silver_total,
            character_count: chars.len() as i64,
            gold_change_percent: gold_change,
            silver_change_percent: silver_change,
            character_change: None,
        };

        Ok(StatisticsPayload { summary, characters: bars, recent })
    }

    /// Sum 金币/银币 per day, **taking each field's own last known value**.
    ///
    /// 老版本取当天最新一条快照、从里面同时读两个币种。但人物属性截图只记
    /// 属性、不带货币，它的 `{}` 被当成 0 加进当天合计，于是"当天最后拍的是
    /// 属性面板"就显示成金币归零，哪怕一小时前刚拍过包裹。
    ///
    /// 现在每个字段各自解析到当天最后一次**真的带了它**的记录；整天都没采
    /// 到过的字段保持 `None`，界面显示"未知"而不是 0。
    fn daily_totals(&self) -> Result<Vec<AssetPoint>> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let conn_tx = conn.transaction()?;
        let mut stmt = conn_tx.prepare(
            "SELECT character_id, recorded_at, currencies FROM snapshots
             ORDER BY recorded_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?;

        // key: (day, character) -> (last known gold, last known silver)
        type Field = Option<i64>;
        let mut latest: std::collections::BTreeMap<(String, String), (Field, Field)> =
            std::collections::BTreeMap::new();
        for r in rows {
            let (char_id, ts, currency_json) = r?;
            let day = chrono::DateTime::from_timestamp_millis(ts)
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "1970-01-01".to_string());
            let cur: Currencies = from_json(currency_json);
            let entry = latest.entry((day, char_id)).or_default();
            // ORDER BY recorded_at ASC ⇒ later rows overwrite; a snapshot that
            // simply does not show a currency leaves the previous value alone.
            if let Some(g) = cur.gold.as_ref().map(|v| v.value) {
                entry.0 = Some(g);
            }
            if let Some(s) = cur.silver.as_ref().map(|v| v.value) {
                entry.1 = Some(s);
            }
        }

        let mut days: std::collections::BTreeMap<String, (Field, Field)> =
            std::collections::BTreeMap::new();
        for ((day, _char), (gold, silver)) in latest {
            let e = days.entry(day).or_insert((None, None));
            if let Some(g) = gold {
                e.0 = Some(e.0.unwrap_or(0) + g);
            }
            if let Some(s) = silver {
                e.1 = Some(e.1.unwrap_or(0) + s);
            }
        }

        Ok(days
            .into_iter()
            .map(|(date, (gold, silver))| AssetPoint { date, gold, silver })
            .collect())
    }
}
