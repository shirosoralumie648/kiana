//! 四个 settings 迁移（`ca-config-models.md` §2.9.7）。
//!
//! TS: `packages/coding-agent/src/core/settings-manager.ts#migrateSettings`。
//!
//! 迁移顺序不可变，且全部是**原地**修改。`Value` 用 `serde_json::Map`
//! （workspace 开了 `preserve_order`），所以键序、未知键原样保留。
//!
//! 关键语义（与 JS 对齐）：
//!
//! - `"key" in settings` = `Map::contains_key`：`null` / `undefined` 值也算“存在”
//!   （JSON 里没有 `undefined`，因此显式 `null` 同样算存在）；
//! - `typeof x === "object" && x !== null && !Array.isArray(x)` = `Value::Object`；
//! - `typeof x === "number"` = `Value::Number`；`typeof x === "boolean"` = `Value::Bool`；
//! - `delete obj.key` = `Map::shift_remove`（保持其余键的顺序）；
//! - `obj.key = value`：键已存在时**保持原位置**，否则追加到末尾（`Map::insert` 的行为）。
//!
//! # 调用点
//!
//! - 加载：`settings::mod::try_load_from_storage` 在 `JSON.parse` 之后、反序列化成
//!   [`crate::settings::Settings`] 之前调用；
//! - 写盘：`Shared::persist_scoped_settings` 在重读磁盘当前内容之后、覆盖本会话修改字段
//!   之前调用，因此一次保存会把旧格式一并升级后写回（`tests/settings_migrations.rs` 的
//!   黄金用例锁定逐字节结果与键序）。
//!
//! # 失败模式
//!
//! 唯一会返回错误的情况是顶层为 JSON 标量：TS 的 `"queueMode" in <scalar>` 抛
//! `TypeError`，这里用 [`SettingsMigrationError`] 复刻文案。顶层数组直接通过（`in` 对
//! 数组合法，四个迁移都找不到字段）。各迁移对字段类型不匹配不会报错：多数情况原样留在
//! 对象里，由后续宽松反序列化放进 `Settings::extra`；例外是 `retry.maxDelayMs` 无论
//! 是否为数字都会被删除（见 `migrate_retry_max_delay`）。

use serde_json::{Map, Value};

/// `migrateSettings(settings)` 抛出的 TypeError 的等价物。
///
/// 只会在顶层是 JSON 标量（`null` / 字符串 / 数字 / 布尔）时出现：TS 的
/// `"queueMode" in settings` 在原始类型上抛 `TypeError: Cannot use 'in' operator to search
/// for 'queueMode' in <value>`。顶层数组不在此列（`in` 对数组合法）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsMigrationError {
    /// 逐字对齐 V8 的 TypeError 文案。
    pub message: String,
}

impl SettingsMigrationError {
    fn not_an_object(value: &Value) -> Self {
        let rendered = match value {
            Value::Null => "null".to_string(),
            Value::String(text) => text.clone(),
            Value::Number(number) => {
                crate::settings::js_string_number(number.as_f64().unwrap_or(f64::NAN))
            }
            Value::Bool(flag) => flag.to_string(),
            Value::Array(_) => "[object Array]".to_string(),
            Value::Object(_) => "[object Object]".to_string(),
        };
        Self {
            message: format!("Cannot use 'in' operator to search for 'queueMode' in {rendered}"),
        }
    }
}

impl std::fmt::Display for SettingsMigrationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SettingsMigrationError {}

/// `migrateSettings(settings)`：原地迁移，返回 `Result<(), SettingsMigrationError>`。
///
/// 顶层数组没有可迁移的属性（`"queueMode" in []` 为 false），原样返回。
pub fn migrate_settings(settings: &mut Value) -> Result<(), SettingsMigrationError> {
    let object = match settings {
        Value::Object(object) => object,
        Value::Array(_) => return Ok(()),
        other => return Err(SettingsMigrationError::not_an_object(other)),
    };

    // 顺序固定（§2.9.7）：新增键的追加顺序由这个调用顺序决定，测试
    // `runs_migrations_in_fixed_order_and_is_idempotent` 逐键锁定。
    migrate_queue_mode(object);
    migrate_websockets(object);
    migrate_skills(object);
    migrate_retry_max_delay(object);
    Ok(())
}

/// 1. `queueMode -> steeringMode`。
///
/// `"queueMode" in settings && !("steeringMode" in settings)`：两键都存在时**不**删除
/// `queueMode`（迁移只覆盖“目标键缺失”的文件）。
fn migrate_queue_mode(settings: &mut Map<String, Value>) {
    if !settings.contains_key("queueMode") || settings.contains_key("steeringMode") {
        return;
    }
    // 值原样搬运（包括 `null` 与非字符串）：迁移只改键名，不校验内容。
    let queue_mode = settings.get("queueMode").cloned().unwrap_or(Value::Null);
    settings.insert("steeringMode".to_string(), queue_mode);
    settings.shift_remove("queueMode");
}

/// 2. 旧的 `websockets` 布尔值 -> `transport` 枚举。
///
/// 仅当 `transport` 缺失且 `websockets` 是布尔值时迁移；该分支里才删除 `websockets`
/// （`transport` 已存在时保留原 `websockets` 键）。
fn migrate_websockets(settings: &mut Map<String, Value>) {
    if settings.contains_key("transport") {
        return;
    }
    let Some(Value::Bool(enabled)) = settings.get("websockets") else {
        return;
    };
    let transport = if *enabled { "websocket" } else { "sse" };
    settings.insert(
        "transport".to_string(),
        Value::String(transport.to_string()),
    );
    settings.shift_remove("websockets");
}

/// 3. 旧的 `skills` 对象格式 -> 数组。
///
/// 当 `skills` 是非 null、非数组对象时：
/// - `enableSkillCommands` 已定义且顶层同名为 `undefined`（键缺失）→ 复制；
/// - `customDirectories` 是非空数组 → 用它替换 `skills`；
/// - 否则删除 `skills`。
fn migrate_skills(settings: &mut Map<String, Value>) {
    let Some(Value::Object(skills)) = settings.get("skills") else {
        return;
    };
    // 先克隆再修改：`skills` 借用了 `settings`，后面既要写回 `skills` 也要写顶层
    // `enableSkillCommands`，无法同时持有借用与可变借用。
    let skills = skills.clone();

    // 目标键只要存在就视为“已定义”（显式 `null` 也算，测试覆盖），此时不覆盖用户值。
    if let Some(enabled) = skills.get("enableSkillCommands")
        && !settings.contains_key("enableSkillCommands")
    {
        settings.insert("enableSkillCommands".to_string(), enabled.clone());
    }
    // 技能激活 TTL 是 Kiana 扩展字段：对象形式迁移到顶层，避免对象被折叠成数组时丢失。
    for (source, target) in [
        ("activationTtlSeconds", "skillActivationTtlSeconds"),
        ("activationTtl", "skillActivationTtl"),
    ] {
        if let Some(value) = skills.get(source)
            && !settings.contains_key(target)
        {
            settings.insert(target.to_string(), value.clone());
        }
    }
    match skills.get("customDirectories") {
        Some(Value::Array(directories)) if !directories.is_empty() => {
            settings.insert("skills".to_string(), Value::Array(directories.clone()));
        }
        _ => {
            settings.shift_remove("skills");
        }
    }
}

/// 4. `retry.maxDelayMs -> retry.provider.maxRetryDelayMs`。
///
/// `retry` 是非 null、非数组对象时：
/// - `provider` 若是“可合并对象”（`typeof === "object" && !== null`，**数组也算**）则作为
///   基础；否则为 `{}`；
/// - `typeof maxDelayMs === "number"` 且 `provider.maxRetryDelayMs` 是 `undefined` 或 `null`
///   → 写入 `provider.maxRetryDelayMs`；
/// - **总是**删除 `retry.maxDelayMs`（即使它不是数字或目标已有值）。
fn migrate_retry_max_delay(settings: &mut Map<String, Value>) {
    let Some(Value::Object(retry)) = settings.get_mut("retry") else {
        return;
    };

    // `provider` 的“可合并对象”判定：对象直接用，数组按 JS 的 `typeof === "object"` 展开成
    // 下标键；其它值（包括 `null`）视为缺失，需要写入时整体替换成新对象。
    let provider: Option<Map<String, Value>> = match retry.get("provider") {
        Some(Value::Object(provider)) => Some(provider.clone()),
        // JS 的 `typeof [] === "object"`：数组也走对象分支，展开成下标键。
        Some(Value::Array(items)) => Some(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| (index.to_string(), item.clone()))
                .collect(),
        ),
        _ => None,
    };

    // `typeof retry.maxDelayMs === "number"`：保留原始 JSON 数字表示（整数不变成 `5000.0`）。
    let max_delay_ms = match retry.get("maxDelayMs") {
        Some(Value::Number(number)) => Some(Value::Number(number.clone())),
        _ => None,
    };
    // `undefined` 或 `null` 都算目标缺失（TS 的 `== null` 语义）；已有非 null 值时不覆盖。
    let target_missing = provider
        .as_ref()
        .is_none_or(|provider| provider.get("maxRetryDelayMs").is_none_or(Value::is_null));

    if let Some(max_delay_ms) = max_delay_ms
        && target_missing
    {
        let mut merged = provider.unwrap_or_default();
        merged.insert("maxRetryDelayMs".to_string(), max_delay_ms);
        retry.insert("provider".to_string(), Value::Object(merged));
    }
    // 无论是否复制成功（`maxDelayMs` 不是数字、目标已有值），旧键都要删除。
    retry.shift_remove("maxDelayMs");
}
