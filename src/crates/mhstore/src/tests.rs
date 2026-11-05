//! 存储层测试。
//!
//! 全部用真实的临时数据库文件，不打内存桩：迁移、WAL、外键级联这些行为只有
//! 真跑一遍才可信。

use super::*;
use mhcore::{
    AttributeValue, Currencies, EquipmentItem, IdentitySource, InventoryItem, PanelFingerprint,
    RecognitionResult,
};
use rusqlite::params;

/// Each test gets its own temp database so runs never interfere.
fn temp_store() -> (Store, PathBuf) {
    let dir = std::env::temp_dir().join(format!("mhstore-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(DB_FILE);
    (Store::open(&path).unwrap(), dir)
}

/// 保存并拿到角色 id —— 绝大多数用例关心的是"存进了哪一行"。
fn save(store: &mut Store, result: &RecognitionResult, screens: &[String]) -> String {
    match store.save_recognition(result, screens).unwrap() {
        SaveOutcome::Saved { character_id, .. } => character_id,
        SaveOutcome::Pending { pending_id, reason } => {
            panic!("意外进了待归属（{pending_id}）：{reason}")
        }
    }
}

fn sample_result(nickname: &str, game_id: &str, gold: i64, silver: i64) -> RecognitionResult {
    let mut r = RecognitionResult::new(IdentitySource::IdField);
    r.character.nickname = Some(nickname.to_string());
    r.character.game_id = Some(game_id.to_string());
    r.character.level = Some(89);
    r.currencies.gold = Some(AttributeValue::new(gold, 0.99, gold.to_string()));
    r.currencies.silver = Some(AttributeValue::new(silver, 0.99, silver.to_string()));
    r.stats.hp = Some(AttributeValue::new(7721, 0.99, "7,721"));
    r.confidence = 0.96;
    r
}

#[test]
fn opens_at_the_latest_schema_version() {
    let (store, _dir) = temp_store();
    assert_eq!(store.schema_version().unwrap(), schema_version());
}

/// A 包裹 capture has no nickname and no combat stats. Saving it against a
/// character must not rename them to 未命名角色 nor blank out the stats the
/// 人物属性 capture recorded a moment earlier.
#[test]
fn bag_save_keeps_nickname_and_stats() {
    let (mut store, _dir) = temp_store();

    let attrs = sample_result("莫莫莫", "429277361", 0, 0);
    let id = save(&mut store, &attrs, &["a.png".into()]);

    let mut bag = RecognitionResult::new(IdentitySource::VisualMatch);
    bag.character_id = Some(id.clone());
    bag.currencies.gold = Some(AttributeValue::new(2_625_812, 0.99, "2.625.812"));
    bag.fingerprints.push(PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: "AAAA".into(),
        score: Some(20214),
    });
    save(&mut store, &bag, &["b.png".into()]);

    let chars = store.list_characters().unwrap();
    assert_eq!(chars.len(), 1, "a linked bag must not create a second row");
    assert_eq!(chars[0].character.nickname, "莫莫莫", "nickname must survive");
    assert_eq!(
        chars[0].character.latest_stats.hp.as_ref().map(|v| v.value),
        Some(7721),
        "stats must survive a capture that did not show them"
    );

    let refs = store.list_identity_refs().unwrap();
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].nickname, "莫莫莫");
    assert_eq!(refs[0].fingerprints.len(), 1);
    assert_eq!(refs[0].fingerprints[0].screen, ScreenKind::Inventory);
    assert_eq!(refs[0].fingerprints[0].score, Some(20214));
}

#[test]
fn open_twice_is_idempotent() {
    let (store, dir) = temp_store();
    drop(store);
    let reopened = Store::open(&dir.join(DB_FILE)).unwrap();
    assert!(reopened.list_characters().unwrap().is_empty());
    assert_eq!(reopened.schema_version().unwrap(), schema_version());
}

#[test]
fn save_recognition_creates_character_and_snapshot() {
    let (mut store, _dir) = temp_store();
    let r = sample_result("海淘淘", "429277013", 2_942_685, 14_297_169);
    let id = save(&mut store, &r, &["a.png".into()]);

    let chars = store.list_characters().unwrap();
    assert_eq!(chars.len(), 1);
    assert_eq!(chars[0].character.nickname, "海淘淘");
    assert_eq!(chars[0].snapshot_count, 1);
    assert!(chars[0].last_recorded_at.is_some());

    let snaps = store.list_snapshots(&id, 10).unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].stats.hp.as_ref().unwrap().value, 7721);
    assert_eq!(snaps[0].source_screens, vec!["a.png".to_string()]);
}

#[test]
fn saving_same_game_id_updates_instead_of_inserting() {
    let (mut store, _dir) = temp_store();
    let id1 = save(&mut store, &sample_result("海淘淘", "429277013", 100, 200), &[]);
    let id2 = save(&mut store, &sample_result("海淘淘", "429277013", 300, 400), &[]);

    assert_eq!(id1, id2, "same game id must resolve to the same character row");
    let chars = store.list_characters().unwrap();
    assert_eq!(chars.len(), 1, "no duplicate character row");
    let snaps = store.list_snapshots(&id1, 10).unwrap();
    assert_eq!(snaps.len(), 2, "both observations are kept as snapshots");
    assert_eq!(
        chars[0].character.latest_currencies.gold.as_ref().unwrap().value,
        300,
        "latest currency is updated"
    );
}

/// 直接写快照，用来构造"同一天里先包裹后人物属性"这种时序。
fn seed_snapshot(
    store: &Store,
    character_id: &str,
    when: &str,
    gold: Option<i64>,
    silver: Option<i64>,
) {
    let ts = chrono::DateTime::parse_from_rfc3339(when).unwrap().timestamp_millis();
    let mut cur = Currencies::default();
    if let Some(g) = gold {
        cur.gold = Some(AttributeValue::new(g, 0.9, g.to_string()));
    }
    if let Some(s) = silver {
        cur.silver = Some(AttributeValue::new(s, 0.9, s.to_string()));
    }
    let conn = store.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO snapshots
         (id, character_id, recorded_at, stats, currencies, equipment, inventory,
          confidence, source_screens)
         VALUES (?1, ?2, ?3, '{}', ?4, '[]', '[]', 0.9, '[]')",
        params![
            uuid::Uuid::new_v4().to_string(),
            character_id,
            ts,
            serde_json::to_string(&cur).unwrap()
        ],
    )
    .unwrap();
}

/// 当天最后一张是「人物属性」（不含金币）时，金币必须沿用当天最后一次
/// **有效**记录，而不是被最新快照的空货币对象覆盖成 0。
#[test]
fn daily_totals_do_not_zero_a_field_missing_from_the_last_capture() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("海淘淘", "1", 0, 0), &[]);
    store.conn.lock().unwrap().execute("DELETE FROM snapshots", []).unwrap();

    seed_snapshot(&store, &id, "2026-10-01T10:00:00Z", Some(2_600_000), Some(100));
    // 11:00 的人物属性截图：只有属性，没有金币银币
    seed_snapshot(&store, &id, "2026-10-01T11:00:00Z", None, None);

    let stats = store.statistics(5).unwrap();
    let day = stats.recent.last().expect("one day");
    assert_eq!(day.date, "2026-10-01");
    assert_eq!(day.gold, Some(2_600_000), "金币不能被空快照抹成 0");
    assert_eq!(day.silver, Some(100));
}

/// 每个字段各自取最后一次有效值：11 点只补了银币，金币应保留 10 点的。
#[test]
fn daily_totals_merge_fields_independently() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("海淘淘", "1", 0, 0), &[]);
    store.conn.lock().unwrap().execute("DELETE FROM snapshots", []).unwrap();

    seed_snapshot(&store, &id, "2026-10-01T09:00:00Z", Some(1000), None);
    seed_snapshot(&store, &id, "2026-10-01T10:00:00Z", None, Some(77));

    let stats = store.statistics(5).unwrap();
    let day = stats.recent.last().unwrap();
    assert_eq!(day.gold, Some(1000));
    assert_eq!(day.silver, Some(77));
}

/// 当天从来没有出现过金币 → 未知（None），不是 0。
#[test]
fn a_field_never_captured_stays_unknown() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("海淘淘", "1", 0, 0), &[]);
    store.conn.lock().unwrap().execute("DELETE FROM snapshots", []).unwrap();

    seed_snapshot(&store, &id, "2026-10-01T10:00:00Z", None, Some(50));

    let stats = store.statistics(5).unwrap();
    let day = stats.recent.last().unwrap();
    assert_eq!(day.gold, None, "从未采集到的字段必须是未知");
    assert_eq!(day.silver, Some(50));
}

#[test]
fn statistics_computes_percent_change() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("海淘淘", "1", 1000, 2000), &[]);
    let second = sample_result("海淘淘", "1", 1100, 2000);
    save(&mut store, &second, &[]);

    let stats = store.statistics(5).unwrap();
    assert_eq!(stats.summary.character_count, 1);
    // Same day => one aggregated point, so no percentage change available.
    assert_eq!(stats.recent.len(), 1);
    assert!(stats.summary.gold_change_percent.is_none());

    // Values are reported in 万元.
    let bar = &stats.characters[0];
    assert!((bar.gold_wan - 0.11).abs() < 1e-9, "1100 => 0.11 万, got {}", bar.gold_wan);
    let _ = id;
}

#[test]
fn identity_candidates_match_game_id_and_nickname() {
    let (mut store, _dir) = temp_store();
    save(&mut store, &sample_result("海淘淘", "429277013", 1, 1), &[]);

    let by_id = store.find_identity_candidates(Some("429277013"), None).unwrap();
    assert_eq!(by_id.len(), 1);

    let by_name = store.find_identity_candidates(None, Some("海淘淘")).unwrap();
    assert_eq!(by_name.len(), 1);

    let dust = store.find_identity_candidates(Some("nope"), Some("nobody")).unwrap();
    assert!(dust.is_empty());

    // A game id hit and a nickname hit of the same row must not duplicate.
    let both = store.find_identity_candidates(Some("429277013"), Some("海淘淘")).unwrap();
    assert_eq!(both.len(), 1);
}

#[test]
fn update_identity_marks_confirmed() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("海淘淘", "1", 1, 1), &[]);
    store
        .update_character_identity(&id, Some("海涛涛"), Some("429277013"), Some(90), Some("紫禁之巅"))
        .unwrap();

    let d = store.get_character(&id).unwrap();
    assert_eq!(d.character.nickname, "海涛涛");
    assert!(d.character.identity_confirmed);
    assert_eq!(d.character.identity_confidence, 1.0);
    assert_eq!(d.character.level, Some(90));
}

#[test]
fn delete_character_cascades_snapshots() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("海淘淘", "1", 1, 1), &[]);
    store.delete_character(&id).unwrap();
    assert!(store.get_character(&id).is_err());
    assert!(store.list_snapshots(&id, 10).unwrap().is_empty());
}

#[test]
fn clear_all_keeps_settings() {
    let (mut store, _dir) = temp_store();
    save(&mut store, &sample_result("海淘淘", "1", 1, 1), &[]);
    let mut settings = store.load_settings().unwrap();
    settings.auto_navigate_after_save = true;
    store.save_settings(&settings).unwrap();

    store.clear_all().unwrap();
    assert!(store.list_characters().unwrap().is_empty());

    let after = store.load_settings().unwrap();
    assert!(after.auto_navigate_after_save, "settings survive a data wipe");
}

#[test]
fn export_document_carries_the_identity_material() {
    let (mut store, _dir) = temp_store();
    let mut r = sample_result("海淘淘", "1", 5, 6);
    r.fingerprints.push(PanelFingerprint {
        screen: ScreenKind::Inventory,
        thumb: "THUMB".into(),
        score: Some(20214),
    });
    r.appearance = Some(mhcore::AppearanceSignature {
        hair_name: "深蓝".into(),
        hair_rgb: [42, 48, 96],
        method: 1,
        weapon_name: Some("青锋".into()),
        weapon_rgb: Some([120, 200, 170]),
    });
    r.equipment.push(EquipmentItem {
        slot: "武器".into(),
        name: "青锋".into(),
        level: Some(80),
        confidence: 0.9,
    });
    r.inventory.push(InventoryItem {
        name: "还魂丹".into(),
        count: 3,
        column: None,
        row: None,
        confidence: 0.8,
    });
    save(&mut store, &r, &[]);

    let doc = store.export_document().unwrap();
    assert_eq!(doc["characters"].as_array().unwrap().len(), 1);
    assert_eq!(doc["snapshots"].as_array().unwrap().len(), 1);
    assert!(doc["settings"].is_object());
    assert!(doc["exportedAt"].is_number());
    // 算法/结构版本必须跟着走，否则换了算法再导入会拿旧分数当新分数比。
    assert_eq!(doc["algorithmVersion"], mhcore::ANALYSIS_VERSION);
    assert_eq!(doc["schemaVersion"].as_i64().unwrap(), schema_version());

    // 这三块是"换台电脑还认得人"的关键，缺一不可。
    assert_eq!(doc["fingerprints"].as_array().unwrap().len(), 1);
    assert_eq!(doc["fingerprints"][0]["thumb"], "THUMB");
    assert_eq!(doc["appearances"].as_array().unwrap().len(), 1);
    assert_eq!(doc["appearances"][0]["hairName"], "深蓝");
    assert_eq!(doc["items"].as_array().unwrap().len(), 2, "装备 + 背包都进字典");
}

#[test]
fn items_dictionary_counts_distinct_characters() {
    let (mut store, _dir) = temp_store();
    let mut a = sample_result("甲", "1", 0, 0);
    a.inventory.push(InventoryItem {
        name: "还魂丹".into(),
        count: 1,
        column: None,
        row: None,
        confidence: 0.9,
    });
    save(&mut store, &a, &[]);
    // 同一个角色再拍一次：计数不该涨到 2。
    save(&mut store, &a, &[]);

    let items = store.list_items(10).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["characterCount"], 1, "{:?}", items[0]);

    let mut b = sample_result("乙", "2", 0, 0);
    b.inventory.push(InventoryItem {
        name: "还魂丹".into(),
        count: 1,
        column: None,
        row: None,
        confidence: 0.9,
    });
    save(&mut store, &b, &[]);
    let items = store.list_items(10).unwrap();
    assert_eq!(items[0]["characterCount"], 2, "{:?}", items[0]);
}

#[test]
fn backup_zip_contains_data_and_database() {
    let (mut store, dir) = temp_store();
    save(&mut store, &sample_result("海淘淘", "1", 5, 6), &[]);

    let dest = dir.join("backup.zip");
    let summary = store.export_backup(&dest, true).unwrap();

    assert!(summary.bytes > 0);
    assert!(summary.includes_database);
    assert!(summary.entries.contains(&"data.json".to_string()));
    assert!(summary.entries.contains(&"mhrecorder.db".to_string()));
    assert!(summary.entries.contains(&"manifest.json".to_string()));
    // 临时库文件不能留在备份旁边。
    assert!(!dir.join("backup.db.tmp").exists());

    let file = std::fs::File::open(&dest).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().into()).collect();
    names.sort();
    assert_eq!(names, vec!["data.json", "manifest.json", "mhrecorder.db"]);

    // 打包进去的库必须真的能打开并读到那条角色。
    let mut db_bytes = Vec::new();
    {
        use std::io::Read as _;
        zip.by_name("mhrecorder.db").unwrap().read_to_end(&mut db_bytes).unwrap();
    }
    let restore_path = dir.join("restored.db");
    std::fs::write(&restore_path, &db_bytes).unwrap();
    let restored = Store::open(&restore_path).unwrap();
    assert_eq!(restored.list_characters().unwrap().len(), 1);
}

#[test]
fn backup_without_database_still_writes_data_json() {
    let (mut store, dir) = temp_store();
    save(&mut store, &sample_result("海淘淘", "1", 1, 1), &[]);
    let summary = store.export_backup(&dir.join("light.zip"), false).unwrap();
    assert!(!summary.includes_database);
    assert!(!summary.entries.contains(&"mhrecorder.db".to_string()));
}

#[test]
fn feature_cache_roundtrips_and_is_version_keyed() {
    let (mut store, _dir) = temp_store();
    assert!(store.get_image_feature("abc").unwrap().is_none());
    store.put_image_feature("abc", "{\"dhash\":123}").unwrap();
    assert_eq!(store.get_image_feature("abc").unwrap().unwrap(), "{\"dhash\":123}");
    // Upsert replaces rather than errors.
    store.put_image_feature("abc", "{\"dhash\":456}").unwrap();
    assert_eq!(store.get_image_feature("abc").unwrap().unwrap(), "{\"dhash\":456}");

    // 换算法版本后键就变了，老条目不会被误读成新算法的结果。
    let key = cache::feature_key(mhcore::ANALYSIS_VERSION, "abc");
    assert!(key.ends_with(":abc"), "{key}");
    assert!(key.starts_with(&format!("v{}:", mhcore::ANALYSIS_VERSION)));

    let older = cache::feature_key(mhcore::ANALYSIS_VERSION - 1, "abc");
    store.put_image_feature(&older, "{\"dhash\":789}").unwrap();
    assert!(store.get_image_feature(&key).unwrap().is_none(), "旧版本键不应命中新键");
}

/// 缓存不能无限长：只保留最近写入的若干条。
#[test]
fn feature_cache_prunes_to_the_newest_entries() {
    let (mut store, _dir) = temp_store();
    for i in 0..6 {
        let key = cache::feature_key(mhcore::ANALYSIS_VERSION, &format!("img-{i}"));
        store.put_image_feature(&key, "{\"v\":3}").unwrap();
        // created_at 是毫秒；同一毫秒内写入时要靠 primary key 兜底排序，
        // 这里显式等一下，让"最近"这件事有确定的顺序。
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(store.feature_cache_len().unwrap(), 6);

    let removed = store.prune_image_features(4).unwrap();
    assert_eq!(removed, 2);
    assert_eq!(store.feature_cache_len().unwrap(), 4);

    let newest = cache::feature_key(mhcore::ANALYSIS_VERSION, "img-5");
    assert!(store.get_image_feature(&newest).unwrap().is_some(), "最新一条必须留下");
    let oldest = cache::feature_key(mhcore::ANALYSIS_VERSION, "img-0");
    assert!(store.get_image_feature(&oldest).unwrap().is_none(), "最旧一条被删");
}

#[test]
fn task_log_roundtrips_and_prunes() {
    let (mut store, _dir) = temp_store();
    for i in 0..5 {
        store
            .record_task(&maintenance::TaskRecord {
                id: format!("t{i}"),
                method: "recognize".into(),
                state: "done".into(),
                started_at: i,
                finished_at: Some(i + 10),
                duration_ms: Some(10),
                ok: Some(true),
                error: None,
                detail: serde_json::json!({ "images": 2 }),
            })
            .unwrap();
    }
    let tasks = store.list_tasks(10).unwrap();
    assert_eq!(tasks.len(), 5);
    assert_eq!(tasks[0].id, "t4", "新的在前");
    assert_eq!(tasks[0].detail["images"], 2);

    let removed = store.prune_tasks(2).unwrap();
    assert_eq!(removed, 3);
    assert_eq!(store.list_tasks(10).unwrap().len(), 2);
}

#[test]
fn settings_defaults_are_sane() {
    let (store, _dir) = temp_store();
    let s = store.load_settings().unwrap();
    assert!(s.editable_before_save, "default matches the design's ON toggle");
    assert!(!s.auto_navigate_after_save);
}

// ---------------------------------------------------------------------------
// 身份闸门（存储层是最后一道）
// ---------------------------------------------------------------------------
//
// 界面上的"请选择归属"只是第一道保护：`recognize.save` 是后端接口，别的客户端
// 也能直接调。所以"拿不准就不存"必须在这里成立。

/// 昵称不唯一：拒绝，而不是挑最近更新的那一行写进去。
#[test]
fn saving_by_an_ambiguous_nickname_is_refused() {
    let (mut store, _dir) = temp_store();
    // 两个服务器上的同名角色：不同游戏ID、同一昵称。
    save(&mut store, &sample_result("莫莫莫", "111", 1, 1), &[]);
    save(&mut store, &sample_result("莫莫莫", "222", 2, 2), &[]);

    let mut only_nick = RecognitionResult::new(IdentitySource::Nickname);
    only_nick.character.nickname = Some("莫莫莫".into());
    only_nick.currencies.gold = Some(AttributeValue::new(9_999, 0.9, "9,999"));

    let err = store.save_recognition(&only_nick, &[]).unwrap_err();
    assert!(matches!(err, StoreError::Identity(_)), "{err:?}");
    assert!(err.to_string().contains("2 个角色"), "{err}");

    // 两行都没被改：金币仍是各自的 1 / 2。
    for c in store.list_characters().unwrap() {
        let gold = c.character.latest_currencies.gold.map(|v| v.value);
        assert!(gold == Some(1) || gold == Some(2), "{gold:?}");
    }
}

/// 游戏ID 在库里对应多行（历史脏数据）：同样拒绝，并要求人来处理。
#[test]
fn saving_by_an_ambiguous_game_id_is_refused() {
    let (mut store, _dir) = temp_store();
    save(&mut store, &sample_result("甲", "429277361", 1, 1), &[]);
    // 绕过保存流程塞一个重复的游戏ID，模拟老版本留下的重复角色。
    store
        .conn
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO characters
             (id, nickname, game_id, latest_stats, latest_currencies, created_at, updated_at)
             VALUES ('dup', '甲（重复）', '429277361', '{}', '{}', 1, 1)",
            [],
        )
        .unwrap();

    let err = store
        .save_recognition(&sample_result("甲", "429277361", 5, 6), &[])
        .unwrap_err();
    assert!(matches!(err, StoreError::Identity(_)), "{err:?}");
    assert!(err.to_string().contains("429277361"), "{err}");
}

/// 明确的 character_id 但库里没有：报错，**不要**顺手新建一行。
#[test]
fn an_unknown_explicit_character_id_is_an_error_not_a_new_row() {
    let (mut store, _dir) = temp_store();
    let mut r = sample_result("莫莫莫", "429277361", 1, 1);
    r.character_id = Some("does-not-exist".into());

    let err = store.save_recognition(&r, &[]).unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
    assert!(store.list_characters().unwrap().is_empty());
}

/// 什么身份都没有：进待归属，不建「未命名角色」，数据一条不丢。
#[test]
fn an_identityless_result_becomes_pending_instead_of_a_character() {
    let (mut store, _dir) = temp_store();
    let mut bag = RecognitionResult::new(IdentitySource::VisualMatch);
    bag.currencies.gold = Some(AttributeValue::new(2_625_812, 0.99, "2,625,812"));
    bag.screenshot_count = 1;

    let pending_id = match store.save_recognition(&bag, &["bag.png".into()]).unwrap() {
        SaveOutcome::Pending { pending_id, reason } => {
            assert!(reason.contains("身份信息"), "{reason}");
            pending_id
        }
        other => panic!("应当是待归属：{other:?}"),
    };

    assert!(store.list_characters().unwrap().is_empty(), "不许建「未命名角色」");
    let pending = store.list_pending(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, pending_id);
    assert!(pending[0].has_currencies);
    assert_eq!(pending[0].screens, vec!["bag.png".to_string()]);
    assert_eq!(store.pending_count().unwrap(), 1);

    // 指定归属后变成正式快照，待归属行消失。
    let owner = save(&mut store, &sample_result("莫莫莫", "429277361", 0, 0), &[]);
    store.assign_pending(&pending_id, &owner).unwrap();
    assert_eq!(store.list_pending(10).unwrap().len(), 0);

    let snaps = store.list_snapshots(&owner, 10).unwrap();
    assert_eq!(snaps.len(), 2, "归属时应补一条快照");
    assert!(snaps
        .iter()
        .any(|s| s.currencies.gold.as_ref().map(|v| v.value) == Some(2_625_812)));
    assert_eq!(
        store.get_character(&owner).unwrap().character.latest_currencies.gold.unwrap().value,
        2_625_812
    );
}

#[test]
fn discarding_a_pending_record_deletes_it() {
    let (mut store, _dir) = temp_store();
    let mut bag = RecognitionResult::new(IdentitySource::VisualMatch);
    bag.currencies.silver = Some(AttributeValue::new(50, 0.9, "50"));
    let pending_id = match store.save_recognition(&bag, &[]).unwrap() {
        SaveOutcome::Pending { pending_id, .. } => pending_id,
        other => panic!("应当是待归属：{other:?}"),
    };

    store.discard_pending(&pending_id).unwrap();
    assert_eq!(store.pending_count().unwrap(), 0);
    assert!(store.discard_pending(&pending_id).is_err(), "再删一次应当是 NotFound");
}

// ---------------------------------------------------------------------------
// latest_* 的字段级合并（P1）
// ---------------------------------------------------------------------------

/// 只读到气血的截图不能把上次的魔法/速度抹掉。
#[test]
fn a_partial_stats_capture_does_not_erase_the_other_fields() {
    let (mut store, _dir) = temp_store();
    let mut full = sample_result("莫莫莫", "429277361", 0, 0);
    full.stats.hp = Some(AttributeValue::new(5459, 0.99, "5459"));
    full.stats.mp = Some(AttributeValue::new(3200, 0.99, "3200"));
    full.stats.speed = Some(AttributeValue::new(800, 0.99, "800"));
    let id = save(&mut store, &full, &[]);

    let mut partial = RecognitionResult::new(IdentitySource::IdField);
    partial.character.nickname = Some("莫莫莫".into());
    partial.character.game_id = Some("429277361".into());
    partial.stats.hp = Some(AttributeValue::new(5600, 0.99, "5600"));
    save(&mut store, &partial, &[]);

    let stats = store.get_character(&id).unwrap().character.latest_stats;
    assert_eq!(stats.hp.map(|v| v.value), Some(5600), "读到的字段要更新");
    assert_eq!(stats.mp.map(|v| v.value), Some(3200), "没读到的魔法必须保留");
    assert_eq!(stats.speed.map(|v| v.value), Some(800), "没读到的速度必须保留");

    // 快照是原始观察，不做合并：第二次那条只有 hp。
    // （同毫秒内的两条快照没有确定先后，所以按内容找，而不是按时间取最新。）
    let snaps = store.list_snapshots(&id, 10).unwrap();
    let second = snaps
        .iter()
        .find(|s| s.stats.hp.as_ref().map(|v| v.value) == Some(5600))
        .expect("第二次的快照");
    assert!(second.stats.mp.is_none(), "历史必须保持每次的原样");
    assert!(snaps
        .iter()
        .any(|s| s.stats.mp.as_ref().map(|v| v.value) == Some(3200)));
}

/// 只读到金币的截图不能抹掉上次的银币。
#[test]
fn a_partial_currency_capture_does_not_erase_the_other_field() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("莫莫莫", "429277361", 2_000_000, 100), &[]);

    let mut only_gold = RecognitionResult::new(IdentitySource::IdField);
    only_gold.character.game_id = Some("429277361".into());
    only_gold.currencies.gold = Some(AttributeValue::new(2_100_000, 0.99, "2,100,000"));
    save(&mut store, &only_gold, &[]);

    let currencies = store.get_character(&id).unwrap().character.latest_currencies;
    assert_eq!(currencies.gold.map(|v| v.value), Some(2_100_000));
    assert_eq!(currencies.silver.map(|v| v.value), Some(100), "银币不能被抹掉");
}

// ---------------------------------------------------------------------------
// server 同步（P1）
// ---------------------------------------------------------------------------

#[test]
fn updating_an_existing_character_syncs_the_server() {
    let (mut store, _dir) = temp_store();
    let id = save(&mut store, &sample_result("莫莫莫", "429277361", 1, 1), &[]);
    assert!(store.get_character(&id).unwrap().character.server.is_none());

    let mut with_server = sample_result("莫莫莫", "429277361", 1, 1);
    with_server.character.server = Some("紫禁之巅".into());
    save(&mut store, &with_server, &[]);
    assert_eq!(
        store.get_character(&id).unwrap().character.server.as_deref(),
        Some("紫禁之巅"),
        "已有角色也要跟着更新服务器"
    );

    // 这次没读到服务器：不能把已有的清空。
    let mut without = sample_result("莫莫莫", "429277361", 1, 1);
    without.character.server = None;
    save(&mut store, &without, &[]);
    assert_eq!(
        store.get_character(&id).unwrap().character.server.as_deref(),
        Some("紫禁之巅"),
        "空值不能覆盖已知的服务器"
    );

    // 手工更正也能改服务器。
    store
        .update_character_identity(&id, None, None, None, Some("生日快乐"))
        .unwrap();
    assert_eq!(
        store.get_character(&id).unwrap().character.server.as_deref(),
        Some("生日快乐")
    );
}
