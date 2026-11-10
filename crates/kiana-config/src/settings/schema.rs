//! `core/settings-schema.ts`：`SettingsSchema` 对应的 Rust 数据模型 + 校验。
//!
//! TS 只把这份 schema 用于**生成** `schemas/settings.schema.json`；运行期读取
//! `settings.json` 时**不做** schema 校验（`ca-config-models.md` §4.1/§4.2），
//! 每个 getter 自己用 `typeof`/`Number.isSafeInteger` 等判断并回落到默认值。
//! 因此这里的结构体分两层：
//!
//! - **类型化字段**：字段名、声明顺序、JSON 名称是当前稳定契约，改动需同步 schema 与测试
//!   （声明顺序就是阶段 8 重新启用 schema 生成时的属性顺序）；
//! - **原样保留**：顶层与每个嵌套对象的 [`extra`](Settings::extra) 保存未知键，以及
//!   **类型与 schema 不符**（含显式 `null`）的已知键。反序列化不会因为某个已知字段的
//!   类型不对而失败，序列化会把原值写回，所以 `serde_json::to_value` 能还原输入
//!   （键序除外）。settings getter 需要 `String(value)` 错误文案时可以直接读原值
//!   （见 [`Settings::extra`]、[`CompactionSettings::extra`]）。
//!
//! 顶层 [`Settings`]/[`SettingsInput`] 还接受 JSON 数组：TS 的 `loadFromStorage` 只做
//! `JSON.parse` + `migrateSettings`，数组不会抛错，等价于没有已知设置的空对象
//! （`settings-manager.ts`）。
//!
//! 数字字段一律用 `f64`：TS 的 `number` 可以带小数，而部分 getter
//! （`getCompactionReserveTokens` 等）必须把 `1.5` 原样带进错误文案。
//! 写出 JSON 时数字格式由 `kiana_json`（`JSON.stringify` 语义）负责，`60.0` 写成 `60`。
//!
//! 生成的 JSON Schema 产物（`packages/coding-agent/schemas/*.json`）属于阶段 8
//! （`ARCHITECTURE.md` §8.25、GAPS G3.4）：本模块**不**嵌入描述字符串，也**不**依赖
//! `schemars`。这里只有一份手写校验器 [`validate_settings_schema`]，复刻 TS 测试里
//! `Compile(SettingsSchema).Check` 的判定（TypeBox/JSON Schema 2020-12 语义）。
//!
//! # 为什么手写 serde，而不是 derive
//!
//! `derive(Deserialize)` 会在任一已知字段类型不符时让**整份** `settings.json` 解析失败，
//! 而 TS 的读取路径从不让这种情况发生。getter 各自处理类型不符的值：`??` 型 getter
//! （如 `getTransport`、`getCompactionEnabled`）原样返回，只对 `null`/`undefined` 回落
//! 默认值；另一些 getter 则按 `typeof`/白名单把非法值收紧到默认值（如 `getThemeSetting`
//! 仅在 `typeof === "string"` 时返回值，`getSessionShareDestination` 把非
//! `"gist"`/`"radius"` 的值映射成 `"auto"`，详见 `ca-config-models.md` §2.9.11）。但无论
//! 哪种，单个字段类型不符都不会让整份 `settings.json` 解析失败。`settings_object!`
//! 因此为每个设置对象生成手写实现：逐键尝试类型化，失败就保留原值；`Serialize` 再把
//! 类型化字段与未解析的原值重新拼回 JSON，保证「读进来什么，写回去还是什么」（键序除外）。
//!
//! # 一次反序列化 / 序列化经过哪些阶段
//!
//! 1. 顶层结构体（`top_level`）先把输入读成 `serde_json::Value`：对象交给 `from_map`，
//!    数组直接返回 `Default`（数组不会让迁移抛错，所有已知设置缺失），其余标量返回错误
//!    （对应 TS 迁移在 `in` 运算符处抛 TypeError）。
//! 2. `from_map` 按字段声明顺序 `shift_remove` 每个已知键：值为 `null` 或无法解析成声明
//!    类型时把原值放进 `extra`，成功则写入类型化字段。
//! 3. 已知键处理完后，剩余键（未知键）按输入顺序追加到 `extra`。嵌套对象重复同一过程，
//!    但它只接受 JSON 对象：形状不对时反序列化失败，错误由父级 `from_map` 捕获并把整个
//!    原值放进父对象的 `extra`。
//! 4. `Serialize` 先按声明顺序写已知键（`Some` 写类型化值，`None` 写 `extra` 里的原值），
//!    再按插入顺序追加 `extra` 里的未知键。输出的键序因此由结构体声明顺序与未知键插入
//!    顺序决定，与输入键序无关。
//!
//! # 注意事项
//!
//! - [`Settings::raw`] 对已解析字段返回的是**重新序列化**的 `Value`，不是输入文本。
//! - `extra` 里可能残留已被类型化字段覆盖的旧值（先解析失败、之后由 setter 赋值的场景）；
//!   `raw` 与 `Serialize` 都以类型化字段优先，残留值不会写回。
//! - [`validate_settings_schema`] 与宽松解析是两条独立路径：前者判断 TS 的
//!   `Compile(SettingsSchema).Check` 是否会接受，**不**参与运行期读取。仓库内调用它的
//!   只有 `tests/config_schemas.rs` 与 `tests/settings_migrations.rs`。
//! - 纯字符串联合枚举用 `#[serde(rename_all = ...)]` 输出 TS 字面量。未知字面量在直接
//!   反序列化该枚举时是错误；在设置对象里则由父级 `from_map` 收进 `extra`，不会让整份
//!   设置解析失败。

use indexmap::IndexMap;
use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// `ModelThinkingLevelSchema` 派生的类型（`"off" | "minimal" | "low" | "medium" | "high" |
/// "xhigh" | "max"`）。复用 kiana-ai 的 7 值枚举，serde 表示与 TS 相同。
pub use kiana_ai::ModelThinkingLevel;

// ---------------------------------------------------------------------------
// 原样保留的设置对象
// ---------------------------------------------------------------------------

/// 声明一个设置对象结构体：已知字段 + 原样保留的 `extra`。
///
/// 每个字段写作 `field: Type = "jsonName"`。生成的结构体字段类型是 `Option<Type>`，
/// 并实现 `Serialize`/`Deserialize`（宽松解析）、`known_keys` 与 `raw`。
/// 结构体名前写 `top_level` 时额外接受顶层 JSON 数组（settings.json 的 `JSON.parse`
/// 语义，见该分支注释）；嵌套对象不在此列。
///
/// 三条不变量：
///
/// - 字段的 `Option` 表示「能否类型化」，不表示「JSON 中是否存在」：键存在但值为
///   `null` 或类型不符时字段为 `None`，原值留在 `extra`；
/// - `from_map` 按声明顺序处理已知键，所以 `extra` 中先出现「类型不符的已知键」，再出现
///   「未知键」；
/// - `raw` 是 getter 回落读原值的入口：已知键优先返回类型化值，其次才是 `extra`
///   （`settings/mod.rs` 的回落 helper 都经由它；需要就地修改原始对象的 setter 例外，
///   它们直接操作 `extra`）。
macro_rules! settings_object {
    // 顶层 settings.json 对象：除 JSON 对象外还接受 JSON 数组。
    //
    // TS 的 `loadFromStorage` 只做 `JSON.parse` + `migrateSettings`：顶层数组不会抛错
    // （`"queueMode" in []` 为 false），等价于一个没有已知设置的对象；null/字符串/
    // 数字/布尔会在 `in` 处抛 TypeError。这是本类型与嵌套对象的唯一区别。
    //
    // 注意：settings-manager 的加载路径会先把顶层数组展开成下标键对象，再交给这里的
    // 反序列化（`try_load_from_storage`），所以此分支保护的是直接
    // `serde_json::from_value::<Settings>` 的调用方（`tests/config_schemas.rs`）。
    (
        top_level
        $(#[$struct_meta:meta])*
        pub struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $field:ident : $ty:ty = $key:literal,
            )*
        }
    ) => {
        settings_object!(@object $(#[$struct_meta])* pub struct $name {
            $(
                $(#[$field_meta])*
                $field : $ty = $key,
            )*
        });

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                match Value::deserialize(deserializer)? {
                    Value::Object(map) => Ok(Self::from_map(map.into_iter().collect())),
                    Value::Array(_) => Ok(Self::default()),
                    other => Err(D::Error::custom(format!(
                        "invalid settings.json top-level value: {other}"
                    ))),
                }
            }
        }
    };

    // 普通设置对象：只接受 JSON 对象。非对象输入会让 `IndexMap` 反序列化失败，错误由
    // 父级 `from_map` 捕获并把整个原值放进父对象的 `extra`，因此嵌套字段类型不符不会
    // 让整份 settings.json 解析失败。
    (
        $(#[$struct_meta:meta])*
        pub struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $field:ident : $ty:ty = $key:literal,
            )*
        }
    ) => {
        settings_object!(@object $(#[$struct_meta])* pub struct $name {
            $(
                $(#[$field_meta])*
                $field : $ty = $key,
            )*
        });

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let raw = IndexMap::<String, Value>::deserialize(deserializer)?;
                Ok(Self::from_map(raw))
            }
        }
    };

    // 公共部分：结构体、`known_keys`、`from_map`、`raw` 与 `Serialize`。
    (
        @object
        $(#[$struct_meta:meta])*
        pub struct $name:ident {
            $(
                $(#[$field_meta:meta])*
                $field:ident : $ty:ty = $key:literal,
            )*
        }
    ) => {
        $(#[$struct_meta])*
        #[derive(Debug, Clone, Default, PartialEq)]
        pub struct $name {
            $(
                $(#[$field_meta])*
                pub $field: Option<$ty>,
            )*
            /// 未知键，或类型与 schema 不符（含显式 `null`）而无法解析的已知键，原样保留。
            ///
            /// 保持插入顺序：`from_map` 先放入类型不符的已知键（声明顺序），再追加未知键
            /// （输入顺序）；`Serialize` 依赖这个顺序写回未知键。
            pub extra: IndexMap<String, Value>,
        }

        impl $name {
            /// 全部 schema 已知键（JSON 名称，声明顺序）。
            ///
            /// 顺序即结构体声明顺序（= `settings-schema.ts` 的属性顺序）：
            /// `tests/config_schemas.rs` 逐个锁定 `SettingsInput` 的 60 项与 `Settings`
            /// 的 58 项，调整声明顺序会让该测试失败。
            pub fn known_keys() -> &'static [&'static str] {
                &[$($key),*]
            }

            /// 从 JSON 对象构造。已知键按声明类型解析；解析失败（含 `null`）时原值进入
            /// [`Self::extra`]，未知键按插入顺序追加到 `extra`。
            ///
            /// 一个键的三种去向：解析成功 → 类型化字段；显式 `null` 或类型不符 → `extra`
            /// 中的原值；未知键 → 处理完已知键后按输入顺序进入 `extra`。例如
            /// `{"theme": 12}` 反序列化后 `theme == None` 且 `extra["theme"] == 12`
            /// （`tests/config_schemas.rs` 锁定）。
            pub fn from_map(mut raw: IndexMap<String, Value>) -> Self {
                let mut parsed = Self::default();
                $(
                    // 取出并删除该已知键：成功则类型化；`null` 与解析失败都把原值放进
                    // `extra`。
                    if let Some(raw_value) = raw.shift_remove($key) {
                        if raw_value.is_null() {
                            parsed.extra.insert($key.to_string(), raw_value);
                        } else {
                            match serde_json::from_value::<$ty>(raw_value.clone()) {
                                Ok(value) => parsed.$field = Some(value),
                                Err(_) => {
                                    parsed.extra.insert($key.to_string(), raw_value);
                                }
                            }
                        }
                    }
                )*
                // 剩下的是未知键，保持它们在输入中的相对顺序。
                parsed.extra.extend(raw);
                parsed
            }

            /// 某个键的原始 JSON 值：已解析的字段重新序列化，未解析的键返回保留的原值。
            ///
            /// `Some(Value::Null)` 与 `None` 含义不同：前者表示键存在且原值就是 `null`，
            /// 后者表示键不存在。大多数回落 helper 把两者都当作缺省，但
            /// `timeout_setting` 会把显式 `null` 当作非法值报错（`settings/mod.rs`）。
            pub fn raw(&self, key: &str) -> Option<Value> {
                // 已知键：类型化值优先，未类型化时读 `extra` 中保留的原值。
                match key {
                    $(
                        $key => match &self.$field {
                            Some(field) => serde_json::to_value(field).ok(),
                            None => self.extra.get(key).cloned(),
                        },
                    )*
                    _ => self.extra.get(key).cloned(),
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let mut map = serializer.serialize_map(None)?;
                $(
                    // 第一阶段：已知键按声明顺序写出；`None` 时回落到 `extra` 的原值，
                    // 保证 `null` 与类型不符的键在 round-trip 中不丢失。
                    match &self.$field {
                        Some(field) => map.serialize_entry($key, field)?,
                        None => {
                            if let Some(raw) = self.extra.get($key) {
                                map.serialize_entry($key, raw)?;
                            }
                        }
                    }
                )*
                // 第二阶段：只追加未知键。已知键已在第一阶段处理（类型化值或原值），
                // 这里跳过；`extra` 中残留的旧值也因此不会重复写出。
                let known = Self::known_keys();
                for (key, raw) in &self.extra {
                    // 已知键要么已经类型化写出，要么在上面的分支按原值写出；其余是未知键。
                    if known.contains(&key.as_str()) {
                        continue;
                    }
                    map.serialize_entry(key, raw)?;
                }
                map.end()
            }
        }
    };
}

// ---------------------------------------------------------------------------
// 字符串字面量联合
// ---------------------------------------------------------------------------

/// `transport`：`"auto" | "sse" | "websocket" | "websocket-cached"`。
///
/// 旧键 `websockets`（布尔）在加载时迁移成 `websocket`/`sse`（见 `migrations`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TransportSetting {
    Auto,
    Sse,
    Websocket,
    WebsocketCached,
}

/// `sessionShareDestination`：`"auto" | "gist" | "radius"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionShareDestination {
    Auto,
    Gist,
    Radius,
}

/// `steeringMode` / `followUpMode` / 旧 `queueMode`：`"all" | "one-at-a-time"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SteeringMode {
    All,
    OneAtATime,
}

/// `defaultProjectTrust`：`"ask" | "always" | "never"`。
///
/// 缺省值是 `Ask`（`#[default]`），与 `SETTINGS_DEFAULTS.default_project_trust` 一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultProjectTrust {
    #[default]
    Ask,
    Always,
    Never,
}

/// `doubleEscapeAction`：`"fork" | "tree" | "none"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DoubleEscapeAction {
    Fork,
    Tree,
    None,
}

/// `treeFilterMode`：`"default" | "no-tools" | "user-only" | "labeled-only" | "all"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TreeFilterMode {
    Default,
    NoTools,
    UserOnly,
    LabeledOnly,
    All,
}

/// `markdown.mermaid`：`"off" | "final" | "streaming"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MermaidRenderingMode {
    Off,
    Final,
    Streaming,
}

/// `codemode.mode`：`"on" | "only"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodemodeMode {
    On,
    Only,
}

/// `cacheWarming`：`"off" | "streaming" | "idle"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheWarmingMode {
    Off,
    Streaming,
    Idle,
}

/// `tuiMode`：`"regular" | "fullscreen"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TuiMode {
    Regular,
    Fullscreen,
}

/// `fullscreenExitOutput`：`"transcript" | "resume-hint"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FullscreenExitOutput {
    Transcript,
    ResumeHint,
}

/// `fullscreenScrollbar`：`"auto" | "always" | "hidden"`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FullscreenScrollbar {
    Auto,
    Always,
    Hidden,
}

/// `terminal.images` 的协议部分：`"kitty" | "iterm2" | "auto"`。
///
/// 与 `false`（关闭）一起组成 [`TerminalImagesSetting`]；把本枚举单独用于反序列化时，
/// `false` 不是合法值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalImageProtocol {
    Kitty,
    Iterm2,
    Auto,
}

// ---------------------------------------------------------------------------
// 混合联合（需要手写 serde：unit 变体在 untagged 表示下会序列化成 null）
// ---------------------------------------------------------------------------

/// `quietStartup`：`boolean | "header"`。
///
/// 反序列化只接受 `true`/`false` 与字符串 `"header"`；其它值返回错误（在设置对象里
/// 由 `from_map` 收进 `extra`，设置本身仍能解析）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietStartup {
    /// `true` / `false`。
    Boolean(bool),
    /// `"header"`。
    Header,
}

impl Serialize for QuietStartup {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Boolean(value) => serializer.serialize_bool(*value),
            Self::Header => serializer.serialize_str("header"),
        }
    }
}

impl<'de> Deserialize<'de> for QuietStartup {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Bool(value) => Ok(Self::Boolean(value)),
            Value::String(value) if value == "header" => Ok(Self::Header),
            other => Err(D::Error::custom(format!(
                "invalid quietStartup value: {other}"
            ))),
        }
    }
}

/// `boolean | "auto"`（`terminal.hyperlinks` 与 `terminal.trueColor`）。
///
/// 字符串分支只接受 `"auto"`；其它字符串或对象等值反序列化失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BooleanOrAuto {
    /// `true` / `false`。
    Boolean(bool),
    /// `"auto"`。
    Auto,
}

impl Serialize for BooleanOrAuto {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Boolean(value) => serializer.serialize_bool(*value),
            Self::Auto => serializer.serialize_str("auto"),
        }
    }
}

impl<'de> Deserialize<'de> for BooleanOrAuto {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Bool(value) => Ok(Self::Boolean(value)),
            Value::String(value) if value == "auto" => Ok(Self::Auto),
            other => Err(D::Error::custom(format!(
                "invalid boolean-or-auto value: {other}"
            ))),
        }
    }
}

/// `terminal.images`：`"kitty" | "iterm2" | "auto" | false`。
///
/// 只接受 `false` 作为布尔值：`true` 不是合法取值（`tests/config_schemas.rs` 锁定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalImagesSetting {
    /// `"kitty" | "iterm2" | "auto"`。
    Protocol(TerminalImageProtocol),
    /// `false`（关闭图片）。
    Disabled,
}

impl Serialize for TerminalImagesSetting {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Protocol(protocol) => protocol.serialize(serializer),
            Self::Disabled => serializer.serialize_bool(false),
        }
    }
}

impl<'de> Deserialize<'de> for TerminalImagesSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::Bool(false) => Ok(Self::Disabled),
            Value::String(value) => match value.as_str() {
                "kitty" => Ok(Self::Protocol(TerminalImageProtocol::Kitty)),
                "iterm2" => Ok(Self::Protocol(TerminalImageProtocol::Iterm2)),
                "auto" => Ok(Self::Protocol(TerminalImageProtocol::Auto)),
                _ => Err(D::Error::custom(format!(
                    "invalid terminal images value: {value:?}"
                ))),
            },
            other => Err(D::Error::custom(format!(
                "invalid terminal images value: {other}"
            ))),
        }
    }
}

/// 超时设置：`number | "disabled"`（`httpIdleTimeoutMs`、`websocketConnectTimeoutMs`）。
///
/// 反序列化接受任意 JSON 数字（包括负数与小数）：运行期不校验 schema，
/// `parseHttpIdleTimeoutMs` 在 getter 里负责判定并给出逐字错误。
///
/// 校验器 [`validate_settings_schema`] 另要求数字 `>= 0`（`validate_timeout_setting`），
/// 因此「能反序列化」不等于「schema 接受」。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TimeoutSetting {
    /// `"disabled"`。
    Disabled,
    /// 毫秒数。
    Millis(f64),
}

impl Serialize for TimeoutSetting {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Disabled => serializer.serialize_str("disabled"),
            Self::Millis(value) => serializer.serialize_f64(*value),
        }
    }
}

impl<'de> Deserialize<'de> for TimeoutSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::String(value) if value == "disabled" => Ok(Self::Disabled),
            Value::Number(value) => value
                .as_f64()
                .map(Self::Millis)
                .ok_or_else(|| D::Error::custom("invalid timeout number")),
            other => Err(D::Error::custom(format!("invalid timeout value: {other}"))),
        }
    }
}

/// `fullscreenWheelScrollLines`：`number | "auto"`。
///
/// 字符串分支只接受 `"auto"`；数字分支接受任意 `f64`（小数、负数也能反序列化），
/// schema 校验同样只要求它是 number。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FullscreenWheelScrollLines {
    /// `"auto"`。
    Auto,
    /// 每次滚轮事件的行数。
    Lines(f64),
}

impl Serialize for FullscreenWheelScrollLines {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Auto => serializer.serialize_str("auto"),
            Self::Lines(value) => serializer.serialize_f64(*value),
        }
    }
}

impl<'de> Deserialize<'de> for FullscreenWheelScrollLines {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match Value::deserialize(deserializer)? {
            Value::String(value) if value == "auto" => Ok(Self::Auto),
            Value::Number(value) => value
                .as_f64()
                .map(Self::Lines)
                .ok_or_else(|| D::Error::custom("invalid wheel scroll lines number")),
            other => Err(D::Error::custom(format!(
                "invalid fullscreenWheelScrollLines value: {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// 嵌套设置对象（§2.8.2）
// ---------------------------------------------------------------------------

settings_object! {
    /// `CompactionModelOverrideSchema`：单个模型的 compaction token 覆盖。
    ///
    /// 作为 `compaction.modelOverrides` 的条目时，外层键是 `${provider}/${id}`
    /// （`settings/compaction.rs` 用精确字符串查找，不做 glob）；字段缺省表示该模型
    /// 沿用 [`CompactionSettings`] 的普通值。
    pub struct CompactionModelOverride {
        reserve_tokens: f64 = "reserveTokens",
        keep_recent_tokens: f64 = "keepRecentTokens",
    }
}

settings_object! {
    /// `CompactionSettingsSchema`。
    ///
    /// `modelOverrides` 的键是精确的 `${provider}/${id}`；`enabled` 没有 per-model
    /// 覆盖（条目里即使写了 `enabled` 也会被忽略，见 `settings/compaction.rs`）。
    ///
    /// token 字段在 schema 里是非负安全整数，但宽松反序列化会保留任意数字/非法值，
    /// 由 getter 报错（[`CompactionSettings::raw_token_setting`]）。
    pub struct CompactionSettings {
        enabled: bool = "enabled",
        reserve_tokens: f64 = "reserveTokens",
        keep_recent_tokens: f64 = "keepRecentTokens",
        model_overrides: IndexMap<String, CompactionModelOverride> = "modelOverrides",
    }
}

impl CompactionSettings {
    /// TS `compaction?.[field]`：`reserveTokens` / `keepRecentTokens` 的原始 JSON 值。
    ///
    /// 合法数字返回 `Value::Number`；`null`、字符串、布尔、对象等非法值返回原值
    /// （`settings-manager` 用它复刻 `String(value)` 错误文案）。字段不存在时返回 `None`。
    pub fn raw_token_setting(&self, field: &str) -> Option<Value> {
        self.raw(field)
    }

    /// TS `compaction?.modelOverrides?.[modelKey]`：条目原始 JSON 值。
    ///
    /// 对象条目返回对象（可再按字段取值）；非对象条目（`null`/数字/字符串/数组）
    /// 返回原值，供 `Invalid compaction.modelOverrides["…"] setting` 报错。
    pub fn raw_model_override(&self, model_key: &str) -> Option<Value> {
        self.raw("modelOverrides")?.get(model_key).cloned()
    }
}

settings_object! {
    /// `BranchSummarySettingsSchema`。
    ///
    /// `reserveTokens` 在 schema 中是普通 `number`（小数、负数也合法），与 compaction
    /// token 的非负安全整数限制不同（`validate_branch_summary`）。
    pub struct BranchSummarySettings {
        reserve_tokens: f64 = "reserveTokens",
        skip_prompt: bool = "skipPrompt",
    }
}

settings_object! {
    /// `ProviderRetrySettingsSchema`：provider/SDK 请求的超时与重试参数。
    pub struct ProviderRetrySettings {
        timeout_ms: f64 = "timeoutMs",
        max_retries: f64 = "maxRetries",
        max_retry_delay_ms: f64 = "maxRetryDelayMs",
    }
}

settings_object! {
    /// `RetrySettingsSchema` 去掉旧字段 `maxDelayMs`（`RetrySettings`）。
    ///
    /// 这是迁移后的运行期视图：旧 `maxDelayMs` 由迁移写入 `provider.maxRetryDelayMs`
    /// （`migrations::migrate_retry_max_delay`）。
    pub struct RetrySettings {
        enabled: bool = "enabled",
        max_retries: f64 = "maxRetries",
        base_delay_ms: f64 = "baseDelayMs",
        max_agent_delay_ms: f64 = "maxAgentDelayMs",
        provider: ProviderRetrySettings = "provider",
    }
}

settings_object! {
    /// `Static<typeof RetrySettingsSchema>`（`SettingsInput.retry`，含旧字段 `maxDelayMs`）。
    ///
    /// 与 [`RetrySettings`] 的唯一差别是保留迁移前的 `maxDelayMs`；运行期视图只使用
    /// [`RetrySettings`]。
    pub struct RetrySettingsInput {
        enabled: bool = "enabled",
        max_retries: f64 = "maxRetries",
        base_delay_ms: f64 = "baseDelayMs",
        max_agent_delay_ms: f64 = "maxAgentDelayMs",
        provider: ProviderRetrySettings = "provider",
        max_delay_ms: f64 = "maxDelayMs",
    }
}

settings_object! {
    /// `TerminalSettingsSchema`。
    ///
    /// `hyperlinks` / `trueColor` / `images` 是三个手写联合，其余字段用宽松的类型化解析。
    pub struct TerminalSettings {
        show_images: bool = "showImages",
        image_width_cells: f64 = "imageWidthCells",
        clear_on_shrink: bool = "clearOnShrink",
        show_terminal_progress: bool = "showTerminalProgress",
        hyperlinks: BooleanOrAuto = "hyperlinks",
        images: TerminalImagesSetting = "images",
        true_color: BooleanOrAuto = "trueColor",
    }
}

settings_object! {
    /// `ImageSettingsSchema`：图片发送相关（自动缩放、是否阻止发送）。
    pub struct ImageSettings {
        auto_resize: bool = "autoResize",
        block_images: bool = "blockImages",
    }
}

settings_object! {
    /// `UiSettingsSchema`：TUI 界面相关设置。
    ///
    /// 目前只有 `locale`（界面语言标识，如 `"en"` / `"zh-CN"`）；字符串内容不做
    /// 白名单校验，由 `/settings` 与 i18n 层自行解释。旧键 `language` 不在 schema
    /// 里，`SettingsManager::get_ui_locale` 会在 `ui.locale` 缺失时回退读取它。
    pub struct UiSettings {
        locale: String = "locale",
    }
}

settings_object! {
    /// `ThinkingBudgetsSettingsSchema`：四个思考级别各自的 token 预算。
    pub struct ThinkingBudgetsSettings {
        minimal: f64 = "minimal",
        low: f64 = "low",
        medium: f64 = "medium",
        high: f64 = "high",
    }
}

settings_object! {
    /// `MarkdownSettingsSchema`。
    ///
    /// `codeBlockIndent` 是代码块缩进字符串（默认恰好两个空格），`mermaid` 是
    /// Mermaid 渲染模式（[`MermaidRenderingMode`]）。
    pub struct MarkdownSettings {
        code_block_indent: String = "codeBlockIndent",
        mermaid: MermaidRenderingMode = "mermaid",
    }
}

settings_object! {
    /// `WarningSettingsSchema`：目前只有一个警告开关 `anthropicExtraUsage`。
    pub struct WarningSettings {
        anthropic_extra_usage: bool = "anthropicExtraUsage",
    }
}

settings_object! {
    /// `CodemodeSettingsSchema`：codemode 的展示模式与内联声明的 token 预算。
    pub struct CodemodeSettings {
        mode: CodemodeMode = "mode",
        inline_budget: f64 = "inlineBudget",
    }
}

settings_object! {
    /// `PackageSourceSchema` 的对象形式（`source` 在 schema 中必填；宽松解析允许缺失）。
    ///
    /// `source` 缺失时本类型反序列化不会报错（`source` 只是 `None`）：校验器会报
    /// `missing "source"`；运行期消费方（`kiana-resources` 的包解析）会返回与 TS 相同的
    /// 错误 `Cannot read properties of undefined (reading 'startsWith')`。
    pub struct PackageSourceObject {
        source: String = "source",
        autoload: bool = "autoload",
        extensions: Vec<String> = "extensions",
        skills: Vec<String> = "skills",
        prompts: Vec<String> = "prompts",
        themes: Vec<String> = "themes",
    }
}

settings_object! {
    /// `SkillsInputSchema` 的对象形式（`deprecated: true`）。
    ///
    /// 加载时 `migrate_skills` 会把 `enableSkillCommands` 复制到顶层（仅当顶层缺失），
    /// 并在 `customDirectories` 是非空数组时用它替换 `skills`；否则删除 `skills`。
    pub struct SkillsSettings {
        enable_skill_commands: bool = "enableSkillCommands",
        custom_directories: Vec<String> = "customDirectories",
    }
}

/// `PackageSourceSchema`：字符串或带资源过滤的对象。
///
/// `#[serde(untagged)]` 按声明顺序尝试：JSON 字符串走 [`PackageSource::Text`]，对象走
/// [`PackageSource::Object`]；其它形状（数组、数字等）整体反序列化失败。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PackageSource {
    /// 字符串形式：加载包中的全部资源。
    Text(String),
    /// 对象形式：按资源过滤。
    Object(PackageSourceObject),
}

/// `SkillsInputSchema`：`string[]` 或旧的对象形式。
///
/// `#[serde(untagged)]` 先尝试数组，再尝试对象；对象形式见 [`SkillsSettings`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillsInput {
    /// `string[]`。
    List(Vec<String>),
    /// 旧对象形式（`deprecated: true`）。
    Object(SkillsSettings),
}

// ---------------------------------------------------------------------------
// SettingsInput / Settings（§2.8.3，60 个顶层字段的声明顺序）
// ---------------------------------------------------------------------------

settings_object! {
    top_level
    /// `SettingsInput = Static<typeof SettingsSchema>`：settings.json 的原始形状。
    ///
    /// 保留迁移前的字段形状（`queueMode`、`websockets`、`retry.maxDelayMs`、对象形式的
    /// `skills`）。运行期加载在迁移后反序列化成 [`Settings`]；当前仓库内只有
    /// `tests/config_schemas.rs` 直接使用本类型。
    pub struct SettingsInput {
        schema: String = "$schema",
        last_changelog_version: String = "lastChangelogVersion",
        default_provider: String = "defaultProvider",
        default_model: String = "defaultModel",
        default_thinking_level: ModelThinkingLevel = "defaultThinkingLevel",
        model_thinking_levels: IndexMap<String, ModelThinkingLevel> = "modelThinkingLevels",
        transport: TransportSetting = "transport",
        session_share_destination: SessionShareDestination = "sessionShareDestination",
        steering_mode: SteeringMode = "steeringMode",
        follow_up_mode: SteeringMode = "followUpMode",
        theme: String = "theme",
        compaction: CompactionSettings = "compaction",
        branch_summary: BranchSummarySettings = "branchSummary",
        retry: RetrySettingsInput = "retry",
        hide_thinking_block: bool = "hideThinkingBlock",
        show_cache_miss_notices: bool = "showCacheMissNotices",
        external_editor: String = "externalEditor",
        shell_path: String = "shellPath",
        quiet_startup: QuietStartup = "quietStartup",
        default_project_trust: DefaultProjectTrust = "defaultProjectTrust",
        shell_command_prefix: String = "shellCommandPrefix",
        npm_command: Vec<String> = "npmCommand",
        collapse_changelog: bool = "collapseChangelog",
        enable_install_telemetry: bool = "enableInstallTelemetry",
        enable_analytics: bool = "enableAnalytics",
        tracking_id: String = "trackingId",
        packages: Vec<PackageSource> = "packages",
        extensions: Vec<String> = "extensions",
        device_id: String = "deviceId",
        skills: SkillsInput = "skills",
        prompts: Vec<String> = "prompts",
        themes: Vec<String> = "themes",
        enable_skill_commands: bool = "enableSkillCommands",
        terminal: TerminalSettings = "terminal",
        images: ImageSettings = "images",
        ui: UiSettings = "ui",
        enabled_models: Vec<String> = "enabledModels",
        default_tools: Vec<String> = "defaultTools",
        double_escape_action: DoubleEscapeAction = "doubleEscapeAction",
        tree_filter_mode: TreeFilterMode = "treeFilterMode",
        thinking_budgets: ThinkingBudgetsSettings = "thinkingBudgets",
        editor_padding_x: f64 = "editorPaddingX",
        output_pad: f64 = "outputPad",
        autocomplete_max_visible: f64 = "autocompleteMaxVisible",
        show_hardware_cursor: bool = "showHardwareCursor",
        markdown: MarkdownSettings = "markdown",
        warnings: WarningSettings = "warnings",
        codemode: CodemodeSettings = "codemode",
        session_dir: String = "sessionDir",
        http_proxy: String = "httpProxy",
        http_idle_timeout_ms: TimeoutSetting = "httpIdleTimeoutMs",
        cache_warming: CacheWarmingMode = "cacheWarming",
        websocket_connect_timeout_ms: TimeoutSetting = "websocketConnectTimeoutMs",
        tui_mode: TuiMode = "tuiMode",
        fullscreen_exit_output: FullscreenExitOutput = "fullscreenExitOutput",
        fullscreen_scrollbar: FullscreenScrollbar = "fullscreenScrollbar",
        fullscreen_copy_on_select: bool = "fullscreenCopyOnSelect",
        fullscreen_wheel_scroll_lines: FullscreenWheelScrollLines = "fullscreenWheelScrollLines",
        queue_mode: SteeringMode = "queueMode",
        websockets: bool = "websockets",
    }
}

settings_object! {
    top_level
    /// `Settings`：迁移后的运行期视图。
    ///
    /// 相比 [`SettingsInput`] 去掉了 `queueMode` / `websockets`（迁移删除），
    /// `retry` 不含旧字段 `maxDelayMs`，`skills` 只剩数组形式。
    ///
    /// 本类型既用于单个作用域（global 或 project）的文件内容，也用于合并后的有效设置；
    /// 两层合并由 [`crate::settings::deep_merge_settings`] 完成，本结构不负责合并语义。
    pub struct Settings {
        schema: String = "$schema",
        last_changelog_version: String = "lastChangelogVersion",
        default_provider: String = "defaultProvider",
        default_model: String = "defaultModel",
        default_thinking_level: ModelThinkingLevel = "defaultThinkingLevel",
        model_thinking_levels: IndexMap<String, ModelThinkingLevel> = "modelThinkingLevels",
        transport: TransportSetting = "transport",
        session_share_destination: SessionShareDestination = "sessionShareDestination",
        steering_mode: SteeringMode = "steeringMode",
        follow_up_mode: SteeringMode = "followUpMode",
        theme: String = "theme",
        compaction: CompactionSettings = "compaction",
        branch_summary: BranchSummarySettings = "branchSummary",
        retry: RetrySettings = "retry",
        hide_thinking_block: bool = "hideThinkingBlock",
        show_cache_miss_notices: bool = "showCacheMissNotices",
        external_editor: String = "externalEditor",
        shell_path: String = "shellPath",
        quiet_startup: QuietStartup = "quietStartup",
        default_project_trust: DefaultProjectTrust = "defaultProjectTrust",
        shell_command_prefix: String = "shellCommandPrefix",
        npm_command: Vec<String> = "npmCommand",
        collapse_changelog: bool = "collapseChangelog",
        enable_install_telemetry: bool = "enableInstallTelemetry",
        enable_analytics: bool = "enableAnalytics",
        tracking_id: String = "trackingId",
        packages: Vec<PackageSource> = "packages",
        extensions: Vec<String> = "extensions",
        device_id: String = "deviceId",
        skills: Vec<String> = "skills",
        prompts: Vec<String> = "prompts",
        themes: Vec<String> = "themes",
        enable_skill_commands: bool = "enableSkillCommands",
        terminal: TerminalSettings = "terminal",
        images: ImageSettings = "images",
        ui: UiSettings = "ui",
        enabled_models: Vec<String> = "enabledModels",
        default_tools: Vec<String> = "defaultTools",
        double_escape_action: DoubleEscapeAction = "doubleEscapeAction",
        tree_filter_mode: TreeFilterMode = "treeFilterMode",
        thinking_budgets: ThinkingBudgetsSettings = "thinkingBudgets",
        editor_padding_x: f64 = "editorPaddingX",
        output_pad: f64 = "outputPad",
        autocomplete_max_visible: f64 = "autocompleteMaxVisible",
        show_hardware_cursor: bool = "showHardwareCursor",
        markdown: MarkdownSettings = "markdown",
        warnings: WarningSettings = "warnings",
        codemode: CodemodeSettings = "codemode",
        session_dir: String = "sessionDir",
        http_proxy: String = "httpProxy",
        http_idle_timeout_ms: TimeoutSetting = "httpIdleTimeoutMs",
        cache_warming: CacheWarmingMode = "cacheWarming",
        websocket_connect_timeout_ms: TimeoutSetting = "websocketConnectTimeoutMs",
        tui_mode: TuiMode = "tuiMode",
        fullscreen_exit_output: FullscreenExitOutput = "fullscreenExitOutput",
        fullscreen_scrollbar: FullscreenScrollbar = "fullscreenScrollbar",
        fullscreen_copy_on_select: bool = "fullscreenCopyOnSelect",
        fullscreen_wheel_scroll_lines: FullscreenWheelScrollLines = "fullscreenWheelScrollLines",
    }
}

// ---------------------------------------------------------------------------
// 校验（复刻 `Compile(SettingsSchema).Check`）
// ---------------------------------------------------------------------------
//
// 下面的 `match` 为每个顶层已知键列一个分支，未知键走最后的 `_ => {}`。新增设置项时
// 若忘记在这里加分支，校验器会静默放行该字段：`SettingsInput` 的字段清单与本函数
// 需要同步维护。

/// TypeBox `Number.MAX_SAFE_INTEGER`。
///
/// 与 `settings/compaction.rs` 里的私有 `MAX_SAFE_INTEGER` 是同一个值，两处各自定义；
/// 修改一边时另一边也要改。
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// 设置 schema 校验失败（内部使用；`settings.json` 的运行期读取**不**做此校验）。
///
/// `Display` 输出 `path: message`；顶层错误（`path` 为空，例如顶层不是对象）只输出
/// `message`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSchemaError {
    /// JSON 路径（`properties` 用 `.`、record 用 `[key]` 拼接）。
    ///
    /// record 条目的实际写法是 `modelOverrides["provider/model"]`（方括号内带双引号；
    /// `tests/config_schemas.rs` 的 `validator_reports_the_failing_path` 锁定）。
    pub path: String,
    /// 失败原因（非 TS 用户可见文案）。
    pub message: String,
}

impl SettingsSchemaError {
    fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SettingsSchemaError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            formatter.write_str(&self.message)
        } else {
            write!(formatter, "{}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for SettingsSchemaError {}

/// `Compile(SettingsSchema).Check(value)`：value 是否符合 settings schema。
///
/// 是 [`validate_settings_schema`] 的布尔封装；需要失败路径时调用后者。
pub fn settings_schema_accepts(value: &Value) -> bool {
    validate_settings_schema(value).is_ok()
}

/// 手写版的 settings schema 校验（TypeBox/JSON Schema 2020-12 语义）。
///
/// - 顶层与所有对象都允许未知键（TS 只给**顶层**显式写了 `additionalProperties: true`，
///   TypeBox 的 `Value.Check` 对未声明 `additionalProperties` 的对象同样放行）；
/// - 不要求任何字段；但出现的字段必须满足声明的类型/字面量/范围。
///
/// # 判定与错误顺序
///
/// - 顶层必须是 JSON 对象：数组、`null`、字符串、数字、布尔一律拒绝。这与 [`Settings`]
///   的宽松反序列化（顶层数组得到空设置）不同，别把两者混用。
/// - 返回第一条失败的 [`SettingsSchemaError`]。顶层键与 `modelThinkingLevels` 等 record
///   的条目按输入顺序遍历（`serde_json` 启用了 `preserve_order`），而各个 `validate_*`
///   内部的已知字段按本文件写死的顺序检查；多条错误时报告哪一条由遍历顺序决定。
pub fn validate_settings_schema(value: &Value) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, "")?;
    // 按输入键分派：每个分支只校验出现的值，未列出的键（未知键）放行。
    for (key, field) in object {
        let path = key.clone();
        match key.as_str() {
            "$schema"
            | "lastChangelogVersion"
            | "defaultProvider"
            | "defaultModel"
            | "theme"
            | "externalEditor"
            | "shellPath"
            | "shellCommandPrefix"
            | "trackingId"
            | "deviceId"
            | "sessionDir"
            | "httpProxy" => expect_string(field, &path)?,
            "defaultThinkingLevel" => expect_model_thinking_level(field, &path)?,
            "modelThinkingLevels" => {
                let levels = expect_object(field, &path)?;
                for (model_key, level) in levels {
                    // 见 [`record_pattern_matches`]：行终止符键不参与校验。
                    if !record_pattern_matches(model_key) {
                        continue;
                    }
                    expect_model_thinking_level(level, &format!("{path}.{model_key}"))?;
                }
            }
            "transport" => expect_one_of(
                field,
                &path,
                &["auto", "sse", "websocket", "websocket-cached"],
            )?,
            "sessionShareDestination" => {
                expect_one_of(field, &path, &["auto", "gist", "radius"])?;
            }
            "steeringMode" | "followUpMode" | "queueMode" => {
                expect_one_of(field, &path, &["all", "one-at-a-time"])?;
            }
            "compaction" => validate_compaction(field, &path)?,
            "branchSummary" => validate_branch_summary(field, &path)?,
            "retry" => validate_retry(field, &path)?,
            "hideThinkingBlock"
            | "showCacheMissNotices"
            | "collapseChangelog"
            | "enableInstallTelemetry"
            | "enableAnalytics"
            | "enableSkillCommands"
            | "showHardwareCursor"
            | "fullscreenCopyOnSelect"
            | "websockets" => expect_bool(field, &path)?,
            "quietStartup" => {
                if field.as_bool().is_none() && field.as_str() != Some("header") {
                    return Err(type_error(&path, "boolean or \"header\""));
                }
            }
            "defaultProjectTrust" => {
                expect_one_of(field, &path, &["ask", "always", "never"])?;
            }
            "npmCommand" | "extensions" | "prompts" | "themes" | "enabledModels"
            | "defaultTools" => expect_string_array(field, &path)?,
            "packages" => validate_packages(field, &path)?,
            "skills" => validate_skills(field, &path)?,
            "terminal" => validate_terminal(field, &path)?,
            "images" => validate_images(field, &path)?,
            "ui" => validate_ui(field, &path)?,
            "doubleEscapeAction" => expect_one_of(field, &path, &["fork", "tree", "none"])?,
            "treeFilterMode" => expect_one_of(
                field,
                &path,
                &["default", "no-tools", "user-only", "labeled-only", "all"],
            )?,
            "thinkingBudgets" => validate_thinking_budgets(field, &path)?,
            "editorPaddingX" | "outputPad" | "autocompleteMaxVisible" => {
                expect_number(field, &path)?;
                if key == "outputPad" {
                    // `outputPad` 在 schema 中是 `0 | 1` 的字面量联合，不是任意数字；
                    // 上面的 expect_number 已排除非数字值。
                    let number = field.as_f64().unwrap_or(f64::NAN);
                    if number != 0.0 && number != 1.0 {
                        return Err(type_error(&path, "0 or 1"));
                    }
                }
            }
            "markdown" => validate_markdown(field, &path)?,
            "warnings" => validate_warnings(field, &path)?,
            "codemode" => validate_codemode(field, &path)?,
            "httpIdleTimeoutMs" | "websocketConnectTimeoutMs" => {
                validate_timeout_setting(field, &path)?;
            }
            "cacheWarming" => expect_one_of(field, &path, &["off", "streaming", "idle"])?,
            "tuiMode" => expect_one_of(field, &path, &["regular", "fullscreen"])?,
            "fullscreenExitOutput" => {
                expect_one_of(field, &path, &["transcript", "resume-hint"])?;
            }
            "fullscreenScrollbar" => expect_one_of(field, &path, &["auto", "always", "hidden"])?,
            "fullscreenWheelScrollLines" => match field.as_f64() {
                Some(_) => {}
                None if field.as_str() == Some("auto") => {}
                None => return Err(type_error(&path, "number or \"auto\"")),
            },
            // 顶层 `{ additionalProperties: true }`。
            _ => {}
        }
    }
    Ok(())
}

/// `Type.Record(Type.String(), T)` 的键是否参与值校验。
///
/// TypeBox 1.3.27 把它编译成 `patternProperties: { "^.*$": T }`，且不写
/// `additionalProperties`（缺省为 true）。JSON Schema 的 `patternProperties` 键按
/// ECMA-262 正则匹配，`^.*$` 没有 `s` 标志时 `.` 不匹配行终止符（`\n`、`\r`、
/// U+2028、U+2029），所以含这些字符的键既不匹配模式、也不受 `additionalProperties`
/// 约束，对应值可以是任意 JSON。其余键都匹配 `^.*$`，必须满足 `T`。
///
/// 参考行为：`Compile(SettingsSchema).Check({ modelThinkingLevels: { "a\nb": "bogus" } })`
/// 在 TS 侧返回 true。
fn record_pattern_matches(key: &str) -> bool {
    !key.chars()
        .any(|character| matches!(character, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
}

// 校验辅助函数的约定：`path` 空串表示顶层（`SettingsSchemaError` 的 `Display` 实现
// 依赖这一约定——只对空 `path` 做特判）。属性用 `.` 连接；record 条目的拼接方式由各
// 校验函数自行决定，并不统一：`modelOverrides` 的条目用 `["key"]` 追加，
// `modelThinkingLevels` 的条目则用 `.{key}` 追加。
// 所有检查失败即返回，调用方用 `?` 上传，因此一次只报告第一个错误。

type JsonObject = serde_json::Map<String, Value>;

// 对象是嵌套校验的入口；顶层传入空 path。
fn expect_object<'a>(value: &'a Value, path: &str) -> Result<&'a JsonObject, SettingsSchemaError> {
    value
        .as_object()
        .ok_or_else(|| type_error(path, "an object"))
}

fn expect_string(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    if value.is_string() {
        Ok(())
    } else {
        Err(type_error(path, "a string"))
    }
}

fn expect_bool(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    if value.is_boolean() {
        Ok(())
    } else {
        Err(type_error(path, "a boolean"))
    }
}

fn expect_number(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    if value.as_f64().is_some() {
        Ok(())
    } else {
        Err(type_error(path, "a number"))
    }
}

fn expect_string_array(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let items = value
        .as_array()
        .ok_or_else(|| type_error(path, "an array"))?;
    // 每个元素都必须是字符串；错误路径带下标（如 `extensions[1]`）。
    for (index, item) in items.iter().enumerate() {
        expect_string(item, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

// 只接受 allowlist 中的字符串；错误文案列出全部合法值。
fn expect_one_of(value: &Value, path: &str, allowed: &[&str]) -> Result<(), SettingsSchemaError> {
    match value.as_str() {
        Some(text) if allowed.contains(&text) => Ok(()),
        Some(_) => Err(type_error(path, &format!("one of {}", quote_list(allowed)))),
        None => Err(type_error(path, &format!("one of {}", quote_list(allowed)))),
    }
}

// Debug 格式给每个值加双引号：`"a", "b"`。
fn quote_list(values: &[&str]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `nonNegativeSafeInteger()`：`Type.Integer({ minimum: 0, maximum: MAX_SAFE_INTEGER })`。
///
/// JSON 没有独立的整数类型，因此用 `fract() == 0` 判断整数；上限取 2^53-1，超过后
/// `f64` 已无法精确表示每个整数。
fn expect_non_negative_safe_integer(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let Some(number) = value.as_f64() else {
        return Err(type_error(path, "a non-negative safe integer"));
    };
    if number.is_finite() && number.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&number) {
        Ok(())
    } else {
        Err(type_error(path, "a non-negative safe integer"))
    }
}

/// `Type.Number({ minimum: 0 })`。
///
/// 允许小数（例如 `codemode.inlineBudget`），与 `expect_non_negative_safe_integer`
/// 的整数限制不同。
fn expect_non_negative_number(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    match value.as_f64() {
        Some(number) if number >= 0.0 => Ok(()),
        _ => Err(type_error(path, "a number >= 0")),
    }
}

// 7 个字面量与 TS `ModelThinkingLevelSchema` 逐一对应；增删
// `kiana_ai::ModelThinkingLevel` 的变体时这里也要同步。
fn expect_model_thinking_level(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    expect_one_of(
        value,
        path,
        &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
    )
}

// 普通 token 字段与 per-model override 都要求非负安全整数；条目里的其它键（包括
// `enabled`）不在 schema 中，按未知键放行。
fn validate_compaction(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("enabled") {
        expect_bool(field, &format!("{path}.enabled"))?;
    }
    for key in ["reserveTokens", "keepRecentTokens"] {
        if let Some(field) = object.get(key) {
            expect_non_negative_safe_integer(field, &format!("{path}.{key}"))?;
        }
    }
    if let Some(overrides) = object.get("modelOverrides") {
        let overrides_path = format!("{path}.modelOverrides");
        let entries = expect_object(overrides, &overrides_path)?;
        for (model_key, entry) in entries {
            // 见 [`record_pattern_matches`]：行终止符键不参与校验。
            if !record_pattern_matches(model_key) {
                continue;
            }
            let entry_path = format!("{overrides_path}[\"{model_key}\"]");
            let entry = expect_object(entry, &entry_path)?;
            for key in ["reserveTokens", "keepRecentTokens"] {
                if let Some(field) = entry.get(key) {
                    expect_non_negative_safe_integer(field, &format!("{entry_path}.{key}"))?;
                }
            }
        }
    }
    Ok(())
}

// `reserveTokens` 是普通 number（小数、负数也合法），`skipPrompt` 是布尔；注意这里
// 与 compaction 的非负安全整数限制不同。
fn validate_branch_summary(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("reserveTokens") {
        expect_number(field, &format!("{path}.reserveTokens"))?;
    }
    if let Some(field) = object.get("skipPrompt") {
        expect_bool(field, &format!("{path}.skipPrompt"))?;
    }
    Ok(())
}

// 兼容迁移前的形状：同时校验顶层 `maxDelayMs` 与 `provider.maxRetryDelayMs`。
fn validate_retry(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("enabled") {
        expect_bool(field, &format!("{path}.enabled"))?;
    }
    for key in ["maxRetries", "baseDelayMs", "maxAgentDelayMs", "maxDelayMs"] {
        if let Some(field) = object.get(key) {
            expect_number(field, &format!("{path}.{key}"))?;
        }
    }
    if let Some(provider) = object.get("provider") {
        let provider_path = format!("{path}.provider");
        let provider = expect_object(provider, &provider_path)?;
        for key in ["timeoutMs", "maxRetries", "maxRetryDelayMs"] {
            if let Some(field) = provider.get(key) {
                expect_number(field, &format!("{provider_path}.{key}"))?;
            }
        }
    }
    Ok(())
}

// 字符串形式直接通过；对象形式要求 `source` 是字符串，其余四个资源列表可选。
fn validate_packages(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let items = value
        .as_array()
        .ok_or_else(|| type_error(path, "an array"))?;
    for (index, item) in items.iter().enumerate() {
        let item_path = format!("{path}[{index}]");
        match item {
            Value::String(_) => {}
            Value::Object(object) => {
                expect_string(
                    object.get("source").ok_or_else(|| {
                        SettingsSchemaError::new(&item_path, "missing \"source\"")
                    })?,
                    &format!("{item_path}.source"),
                )?;
                if let Some(field) = object.get("autoload") {
                    expect_bool(field, &format!("{item_path}.autoload"))?;
                }
                for key in ["extensions", "skills", "prompts", "themes"] {
                    if let Some(field) = object.get(key) {
                        expect_string_array(field, &format!("{item_path}.{key}"))?;
                    }
                }
            }
            _ => return Err(type_error(&item_path, "a string or an object")),
        }
    }
    Ok(())
}

// 联合类型：`string[]` 或旧对象形式（对象只校验两个已知键）。
fn validate_skills(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    match value {
        Value::Array(_) => expect_string_array(value, path),
        Value::Object(object) => {
            if let Some(field) = object.get("enableSkillCommands") {
                expect_bool(field, &format!("{path}.enableSkillCommands"))?;
            }
            if let Some(field) = object.get("customDirectories") {
                expect_string_array(field, &format!("{path}.customDirectories"))?;
            }
            Ok(())
        }
        _ => Err(type_error(path, "an array or an object")),
    }
}

// 三个布尔字段、一个数字字段、两个 `boolean | "auto"` 联合和一个 `images` 联合。
// 注意 `images` 只接受 `false` 或三个协议名：`true` 会被拒绝（`tests/config_schemas.rs`）。
fn validate_terminal(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    for key in ["showImages", "clearOnShrink", "showTerminalProgress"] {
        if let Some(field) = object.get(key) {
            expect_bool(field, &format!("{path}.{key}"))?;
        }
    }
    if let Some(field) = object.get("imageWidthCells") {
        expect_number(field, &format!("{path}.imageWidthCells"))?;
    }
    for key in ["hyperlinks", "trueColor"] {
        let invalid = object
            .get(key)
            .is_some_and(|field| field.as_bool().is_none() && field.as_str() != Some("auto"));
        if invalid {
            return Err(type_error(&format!("{path}.{key}"), "boolean or \"auto\""));
        }
    }
    if let Some(field) = object.get("images") {
        let images_path = format!("{path}.images");
        let valid = field.as_bool() == Some(false)
            || matches!(field.as_str(), Some("kitty" | "iterm2" | "auto"));
        if !valid {
            return Err(type_error(
                &images_path,
                "\"kitty\", \"iterm2\", \"auto\", or false",
            ));
        }
    }
    Ok(())
}

// 两个布尔开关。
fn validate_images(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    for key in ["autoResize", "blockImages"] {
        if let Some(field) = object.get(key) {
            expect_bool(field, &format!("{path}.{key}"))?;
        }
    }
    Ok(())
}

// 目前只有 `locale`（字符串）；其它键按未知键放行（对象允许扩展）。
fn validate_ui(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("locale") {
        expect_string(field, &format!("{path}.locale"))?;
    }
    Ok(())
}

// 四个键都只是 number（允许负数与小数）；schema 没有设置上下限。
fn validate_thinking_budgets(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    for key in ["minimal", "low", "medium", "high"] {
        if let Some(field) = object.get(key) {
            expect_number(field, &format!("{path}.{key}"))?;
        }
    }
    Ok(())
}

// `codeBlockIndent` 是字符串；`mermaid` 是三个字面量之一。
fn validate_markdown(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("codeBlockIndent") {
        expect_string(field, &format!("{path}.codeBlockIndent"))?;
    }
    if let Some(field) = object.get("mermaid") {
        expect_one_of(
            field,
            &format!("{path}.mermaid"),
            &["off", "final", "streaming"],
        )?;
    }
    Ok(())
}

// 目前只有一个布尔开关。
fn validate_warnings(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("anthropicExtraUsage") {
        expect_bool(field, &format!("{path}.anthropicExtraUsage"))?;
    }
    Ok(())
}

// `mode` 是 `"on" | "only"`；`inlineBudget` 是非负 number（允许小数）。
fn validate_codemode(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    let object = expect_object(value, path)?;
    if let Some(field) = object.get("mode") {
        expect_one_of(field, &format!("{path}.mode"), &["on", "only"])?;
    }
    if let Some(field) = object.get("inlineBudget") {
        expect_non_negative_number(field, &format!("{path}.inlineBudget"))?;
    }
    Ok(())
}

// 与 TS 的 `timeoutSetting()` 相同：`"disabled"` 或 >= 0 的数字；小数在 schema 层合法。
fn validate_timeout_setting(value: &Value, path: &str) -> Result<(), SettingsSchemaError> {
    if value.as_str() == Some("disabled") {
        return Ok(());
    }
    match value.as_f64() {
        Some(number) if number >= 0.0 => Ok(()),
        _ => Err(type_error(path, "a number >= 0 or \"disabled\"")),
    }
}

// 统一文案为 `expected <expected>`；路径由调用方给出。
fn type_error(path: &str, expected: &str) -> SettingsSchemaError {
    SettingsSchemaError::new(path, format!("expected {expected}"))
}
