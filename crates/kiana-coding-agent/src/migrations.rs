//! `migrations.ts`（315 行）：启动时执行的一次性迁移。
//!
//! 顺序固定：auth.json → 顶层会话 → tools/→bin/ → keybindings → 扩展系统弃用检查。
//! 读写都走 `kiana-json`：`JSON.parse` 的宽松语义（接受 `1e400` → `null`、孤立代理项转义）
//! 与 `JSON.stringify(value, null, 2)` 的字节级格式都由它保证（ARCHITECTURE §3.2、§4.3）。
//!
//! TS: `packages/coding-agent/src/migrations.ts`
//!
//! 与 TS 的被迫偏差（`kiana-json` 已在 §8 记录）：孤立代理项在 Rust `String` 中无法表示，
//! 解析时替换为 U+FFFD，因此含 `"\ud800"` 的文件重写后写出 `"�"` 而不是原转义序列。
//! 迁移本身不再被静默跳过。
//!
//! # 在启动流程中的位置
//!
//! 生产调用方只有 [`crate::app::main`]：启动第 17 步同步执行 [`run_migrations`]（在创建
//! 启动设置与 session/runtime 之前），第 36 步在 Interactive 模式下把
//! `deprecation_warnings` 交给 [`show_deprecation_warnings`] 展示并等待一次按键；
//! `migrated_auth_providers` 原样传入 `InteractiveModeOptions::migrated_providers`
//! （见 `crates/kiana-coding-agent/src/app.rs`）。
//!
//! # 执行流程与失败语义
//!
//! [`run_migrations`] 按固定顺序调用 5 个迁移，彼此不共享中间状态：
//!
//! 1. [`migrate_auth_to_auth_json`]：`oauth.json` 与 `settings.json.apiKeys` → `auth.json`；
//! 2. [`migrate_sessions_from_agent_root`]：agent 目录顶层的 `*.jsonl` →
//!    `sessions/--<encoded-cwd>--/`；
//! 3. [`migrate_tools_to_bin`]：`tools/{fd,rg,fd.exe,rg.exe}` → `bin/`；
//! 4. [`migrate_keybindings_config_file`]：`keybindings.json` 旧键名改写；
//! 5. [`migrate_extension_system`]：`commands/` → `prompts/`，并收集 `hooks/`、自定义
//!    `tools/` 的弃用警告。
//!
//! 每一步都是幂等的：已有目标就跳过（`auth.json` 存在、会话目标文件存在、`prompts/`
//! 存在），`bin/` 中已有同名文件时删除源文件，keybindings 没有改名时不重写。因此每次
//! 启动重复执行不会改变结果。
//!
//! 绝大多数失败被静默忽略（读文件、解析 JSON、rename、settings/keybindings 写回），
//! 与 TS 在这些位置用 `try/catch` 包裹的行为一致。**只有两处**失败会中止启动：
//! `auth.json` 的目录创建/写入，以及 `bin/` 目录的创建。Rust 用 [`FatalStartupError`]
//! panic 表示 TS 里逃出 `runMigrations` 的未捕获异常，由二进制入口的
//! [`crate::cli::setup::install_fatal_startup_panic_hook`] 转成 `Error: <message>` 与退出
//! 码 1；这也解释了为什么 [`run_migrations`] 返回 [`MigrationResult`] 而不是 `Result`。
//!
//! # 注意事项
//!
//! - agent 目录下的路径均在调用时通过 `get_agent_dir()` 解析（每次读
//!   `KIANA_CODING_AGENT_DIR`；`bin/` 经 `get_bin_dir()` 派生）；项目级路径来自
//!   [`run_migrations`] 的 `cwd` 参数（`<cwd>/.kiana`）。测试据此把迁移重定向到临时目录。
//! - 文件语义刻意复刻 Node：解析走 `kiana-json` 的宽松 `JSON.parse`，写出走其
//!   `JSON.stringify(value, null, 2)` 等价物；不要改成 `serde_json::to_string_pretty`
//!   （数字格式与整数键顺序都不同）。
//! - 迁移只移动/重写文件，不校验迁移后的配置是否可用；非法值留给后续加载器按各自规则
//!   丢弃（例如 keybindings 的非法值在 `KeybindingsManager` 加载时被丢弃）。
//! - 本模块的文件 IO 全部同步执行；异步的只有弃用警告展示函数（需要等待按键）。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use kiana_core::config::{CONFIG_DIR_NAME, get_agent_dir, get_bin_dir};
use kiana_core::text::strip_bom;
use serde_json::Value;

use crate::cli::setup::{FatalStartupError, Output, Style, node_fs_error_message};

/// `MIGRATION_GUIDE_URL`。
///
/// 弃用警告末尾 `Migration guide: …` 一行使用该常量。URL 文本被
/// `deprecation_warnings_print_the_exact_plain_lines` 逐字锁定，改动会让输出与 TS 不一致。
pub const MIGRATION_GUIDE_URL: &str = "https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration";
/// `EXTENSIONS_DOC_URL`。
///
/// 弃用警告末尾 `Documentation: …` 一行使用该常量；同样被上述测试逐字锁定。
pub const EXTENSIONS_DOC_URL: &str =
    "https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/docs/extensions.md";

/// `runMigrations` 的返回值。
///
/// 两个列表都可能为空：空 `migrated_auth_providers` 表示 `auth.json` 已存在或没有可迁移
/// 的凭据，空 `deprecation_warnings` 表示没有发现旧扩展目录。两个字段的消费方见模块文档
/// 的“在启动流程中的位置”。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationResult {
    /// 被迁移到 `auth.json` 的 provider（oauth 在前、settings 在后）。
    pub migrated_auth_providers: Vec<String>,
    /// 扩展系统弃用警告（Global 在前、Project 在后）。
    pub deprecation_warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// JS JSON.stringify(value, null, 2)
// ---------------------------------------------------------------------------

/// `JSON.stringify(value, null, 2)` 的等价文本（无尾随换行）。
pub(crate) fn js_stringify_pretty(value: &Value) -> String {
    kiana_json::js_object::stringify_pretty(value, 2)
}

// ---------------------------------------------------------------------------
// JS 对象语义（属性顺序 / Object.entries / 原型链）
// ---------------------------------------------------------------------------

/// 规范数组索引字符串：`"0"`…`"4294967294"`（无前导零）。
///
/// ECMAScript 把这类键视为整数索引，枚举时排在所有字符串键之前并按数值升序；
/// `serde_json` 的 `preserve_order` 只有插入序，因此需要显式重排。
fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key != "0" && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u64 = key.parse().ok()?;
    if value >= u32::MAX as u64 {
        return None;
    }
    Some(value as u32)
}

/// JS 属性枚举顺序（`Object.keys` / `Object.entries` / `JSON.stringify`）：
/// 整数索引键按数值升序在前，其余键保持插入序。
fn js_ordered_keys(map: &serde_json::Map<String, Value>) -> Vec<&String> {
    let mut indices: Vec<(u32, &String)> = Vec::new();
    let mut others: Vec<&String> = Vec::new();
    for key in map.keys() {
        match js_array_index(key) {
            Some(index) => indices.push((index, key)),
            None => others.push(key),
        }
    }
    indices.sort_by_key(|(index, _)| *index);
    indices
        .into_iter()
        .map(|(_, key)| key)
        .chain(others)
        .collect()
}

/// JS `Object.entries(value)` 的条目顺序；`null` / `undefined` 在 JS 会抛错，返回 `None`。
///
/// - 对象：整数索引键在前，其余插入序；
/// - 数组：索引序；
/// - 字符串：按 UTF-16 码元枚举（孤立代理项在 UTF-8 中无法表示，替换成 U+FFFD）；
/// - 数字/布尔：没有可枚举自有属性。
fn js_object_entries(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Object(map) => Some(
            js_ordered_keys(map)
                .into_iter()
                .map(|key| {
                    (
                        key.clone(),
                        map.get(key.as_str()).cloned().unwrap_or(Value::Null),
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Some(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| (index.to_string(), item.clone()))
                .collect(),
        ),
        Value::String(text) => Some(js_string_entries(text)),
        Value::Number(_) | Value::Bool(_) => Some(Vec::new()),
        Value::Null => None,
    }
}

/// `Object.entries("abc")`：每个 UTF-16 码元一个 `["<index>", "<unit>"]`。
///
/// 非 BMP 字符（如 emoji）在 UTF-16 里是代理对，两个码元都不是合法的 Rust `char`，
/// 因此各变成一个 U+FFFD；JS 侧得到的是两个独立的代理项字符串。这个偏差只影响把字符串
/// 当作 entry 源的极端输入（例如 `oauth.json` 的根或某个凭据是字符串，或 `__proto__`
/// 被赋成字符串）。
fn js_string_entries(text: &str) -> Vec<(String, Value)> {
    text.encode_utf16()
        .enumerate()
        .map(|(index, unit)| {
            let character = char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}');
            (index.to_string(), Value::String(character.to_string()))
        })
        .collect()
}

/// JS truthiness（JSON 能表达的值里没有 `undefined`/NaN）。
fn js_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `Object.prototype` 的自有属性名（读取普通对象上的这些名字总是得到 truthy 的函数）。
const OBJECT_PROTOTYPE_KEYS: &[&str] = &[
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

/// `migrateAuthToAuthJson` 里 `migrated` 普通对象的最小 JS 语义：
///
/// - `migrated[key] = value` 对 `__proto__` 触发原型 setter（不产生自有属性）；
/// - `!migrated[provider]` 会沿原型链读取，包含 `Object.prototype` 上的函数名；
/// - `Object.keys(migrated)` / `JSON.stringify(migrated)` 只看自有属性。
struct JsPlainObject {
    own: serde_json::Map<String, Value>,
    prototype: Option<serde_json::Map<String, Value>>,
}

impl JsPlainObject {
    fn new() -> Self {
        Self {
            own: serde_json::Map::new(),
            prototype: None,
        }
    }

    /// `migrated[key] = value`：`__proto__` 改为设置原型（不产生自有属性），其余键写入
    /// 自有表。
    fn set(&mut self, key: String, value: Value) {
        if key == "__proto__" {
            match value {
                Value::Object(map) => self.prototype = Some(map),
                Value::Array(items) => {
                    self.prototype = Some(
                        items
                            .into_iter()
                            .enumerate()
                            .map(|(index, item)| (index.to_string(), item))
                            .collect(),
                    );
                }
                Value::String(text) => {
                    self.prototype = Some(js_string_entries(&text).into_iter().collect());
                }
                Value::Null => self.prototype = None,
                Value::Number(_) | Value::Bool(_) => {}
            }
            return;
        }
        // 与 JS 赋值一致：已存在的自有属性保持原位置。
        self.own.insert(key, value);
    }

    /// `Boolean(obj[key])`，沿原型链（含 `Object.prototype`）。
    fn get_truthy(&self, key: &str) -> bool {
        if let Some(value) = self.own.get(key) {
            return js_truthy(value);
        }
        if let Some(prototype) = &self.prototype
            && let Some(value) = prototype.get(key)
        {
            return js_truthy(value);
        }
        // `Object.prototype.__proto__` getter 返回原型对象（初始为 Object.prototype，非 null）。
        key == "__proto__" || OBJECT_PROTOTYPE_KEYS.contains(&key)
    }

    fn is_empty(&self) -> bool {
        self.own.is_empty()
    }

    /// 只序列化自有属性：`JSON.stringify` 看不到原型上的值，因此这里直接丢弃原型。
    fn into_value(self) -> Value {
        Value::Object(self.own)
    }
}

// ---------------------------------------------------------------------------
// Node `fs` 错误文案
// ---------------------------------------------------------------------------

/// TS 里逃出 `runMigrations` 的同步 fs 异常：Node 以未捕获异常终止启动（stderr 打印
/// `Error: <message>`，退出码 1）。
///
/// Rust 无法复刻 JS stack trace，用 [`FatalStartupError`] panic 表示同样的“启动中止”；
/// 二进制入口的 panic 钩子（[`crate::cli::setup::install_fatal_startup_panic_hook`]）把它
/// 映射为 Node 的 stderr 文案与退出码 1。测试里它仍可被 `catch_unwind` 捕获并检查消息。
fn fatal_node_fs_error(syscall: &str, path: Option<&Path>, error: &std::io::Error) -> ! {
    std::panic::panic_any(FatalStartupError {
        message: node_fs_error_message(error, syscall, path, None),
    })
}

/// `writeFileSync` 的失败点：磁盘写满 / I/O 错误发生在 `write` 阶段（libuv 不带路径），
/// 其余发生在 `open` 阶段（带路径）。
fn write_file_syscall(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::StorageFull | ErrorKind::WriteZero => "write",
        _ => "open",
    }
}

// ---------------------------------------------------------------------------
// 文件 helpers
// ---------------------------------------------------------------------------

/// Node `readFileSync(path, "utf-8")`：非法字节替换成 U+FFFD。
fn read_text(path: &Path) -> Option<String> {
    std::fs::read(path)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// `JSON.parse(stripBom(text))`：使用与 V8 一致的宽松解析（接受数值溢出与孤立代理项）。
/// 解析失败返回 `None`（TS 的 `catch` 分支）。
fn parse_json(path: &Path) -> Option<Value> {
    let content = read_text(path)?;
    kiana_json::parse(&strip_bom(&content)).ok()
}

/// `writeFileSync(path, text)`：保留已有文件权限（不存在时按 umask 创建）。
fn write_text(path: &Path, content: &str) -> std::io::Result<()> {
    std::fs::write(path, content)
}

/// `writeFileSync(path, text, { mode: 0o600 })`。
fn write_text_mode_0600(path: &Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(content.as_bytes())?;
        file.flush()
    }
    #[cfg(not(unix))]
    {
        write_text(path, content)
    }
}

/// JS `{ ...value }` 展开进一个对象：`target` 上按插入序覆盖。
///
/// 字符串展开成索引属性（`{...("ab")}` → `{0:"a", 1:"b"}`）；数字/布尔/`null` 没有自有属性。
fn spread_object(target: &mut serde_json::Map<String, Value>, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                target.insert(key.clone(), entry.clone());
            }
        }
        Value::Array(items) => {
            for (index, entry) in items.iter().enumerate() {
                target.insert(index.to_string(), entry.clone());
            }
        }
        Value::String(text) => {
            for (key, entry) in js_string_entries(text) {
                target.insert(key, entry);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// auth.json 迁移
// ---------------------------------------------------------------------------

/// `migrateAuthToAuthJson()`。
///
/// - `auth.json` 已存在 → 什么都不做，返回空。
/// - `oauth.json` 成功解析 → 每个 provider `{type:"oauth", ...cred}`，随后改名为
///   `oauth.json.migrated`；根是 `null` 时 `Object.entries` 抛错 → 不改名也不迁移。
/// - `settings.json` 的 `apiKeys` 对象 → 缺失 provider 写 `{type:"api_key", key}`（oauth 优先），
///   删除 `apiKeys` 并以 2 空格缩进重写（无尾随换行）。
/// - 迁移结果非空时写 `auth.json`（mode 0600）；该写入在 TS 里位于 try 之外，
///   失败会让 `runMigrations` 抛出并中止启动。
///
/// 细节：
///
/// - 三个路径都来自 `get_agent_dir()`；`auth.json` 存在时函数在读取另外两个文件之前返回。
/// - `oauth.json` 的根可以是字符串或数组（JS 的 `Object.entries` 对二者都可枚举）；
///   数字根没有条目，但解析成功后仍会把文件改名为 `.migrated`。
/// - `settings.json` 只在顶层是对象、`apiKeys` 为 truthy 对象/数组（JS 里数组也是对象）
///   时处理；值不是字符串的 provider 跳过。即使没有任何 provider 被迁移，也会删掉
///   `apiKeys` 并重写 `settings.json`。
/// - `providers` 的顺序是 oauth 段在前、settings 段在后；段内按 JS 属性枚举序
///   （整数索引键按数值升序在前，其余保持插入序），不是属性插入顺序。由于
///   `migrated[provider] = …` 的 JS 语义，名为 `__proto__` 的 provider 会被报告但不会
///   写入 `auth.json`（见 `JsPlainObject` 与测试
///   `oauth_proto_provider_sets_the_prototype_and_is_still_reported`）。
pub fn migrate_auth_to_auth_json() -> Vec<String> {
    let agent_dir = get_agent_dir();
    let auth_path = agent_dir.join("auth.json");
    let oauth_path = agent_dir.join("oauth.json");
    let settings_path = agent_dir.join("settings.json");

    if auth_path.exists() {
        return Vec::new();
    }

    let mut migrated = JsPlainObject::new();
    let mut providers: Vec<String> = Vec::new();

    // 阶段 1：oauth.json（优先级最高）。解析失败直接跳过（不让单个损坏文件阻塞迁移）；解析成功
    // 则无论条目多少都把源文件改名成 `.migrated`，避免下次启动重复处理。
    if oauth_path.exists()
        && let Some(oauth) = parse_json(&oauth_path)
        && let Some(entries) = js_object_entries(&oauth)
    {
        for (provider, credential) in entries {
            let mut value: serde_json::Map<String, Value> = serde_json::Map::new();
            value.insert("type".to_string(), Value::String("oauth".to_string()));
            spread_object(&mut value, &credential);
            migrated.set(provider.clone(), Value::Object(value));
            providers.push(provider);
        }
        // 解析成功后无论内容是否为空都改名；改名失败静默（与 TS 的 catch 一致）。
        let _ = std::fs::rename(&oauth_path, format!("{}.migrated", oauth_path.display()));
    }

    // 阶段 2：settings.json.apiKeys。`get_truthy` 复刻 `!migrated[provider]`：已由 oauth
    // 迁移的 provider、以及 Object.prototype 上的名字都不会被覆盖；非字符串的值跳过。
    if settings_path.exists()
        && let Some(settings) = parse_json(&settings_path)
        && let Value::Object(mut map) = settings
        && let Some(api_keys) = map.get("apiKeys").cloned()
        && js_truthy(&api_keys)
        && matches!(api_keys, Value::Object(_) | Value::Array(_))
        && let Some(entries) = js_object_entries(&api_keys)
    {
        for (provider, key) in entries {
            if !migrated.get_truthy(&provider)
                && let Value::String(key) = key
            {
                let mut value: serde_json::Map<String, Value> = serde_json::Map::new();
                value.insert("type".to_string(), Value::String("api_key".to_string()));
                value.insert("key".to_string(), Value::String(key));
                migrated.set(provider.clone(), Value::Object(value));
                providers.push(provider);
            }
        }
        // JS `delete` 保持其余键的相对顺序（serde_json 的 `remove` 是 swap_remove）。
        map.shift_remove("apiKeys");
        // `writeFileSync` 在 try 内：失败被 TS 的 catch 吞掉。
        let _ = write_text(&settings_path, &js_stringify_pretty(&Value::Object(map)));
    }

    // 阶段 3：写 auth.json。TS 的这一步在 try 之外，失败会中止启动；目录创建与写入分别
    // 映射到 Node 的 `mkdir` / `open`（或 `write`）syscall 文案。
    if !migrated.is_empty() {
        if let Some(parent) = auth_path.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            fatal_node_fs_error("mkdir", Some(parent), &error);
        }
        if let Err(error) =
            write_text_mode_0600(&auth_path, &js_stringify_pretty(&migrated.into_value()))
        {
            fatal_node_fs_error(write_file_syscall(&error), Some(&auth_path), &error);
        }
    }

    providers
}

// ---------------------------------------------------------------------------
// 会话迁移
// ---------------------------------------------------------------------------

/// `migrateSessionsFromAgentRoot()`：把 `~/.kiana/agent/*.jsonl` 移到
/// `~/.kiana/agent/sessions/--<encoded-cwd>--/`。
///
/// 首行为空/纯空白、或 header 不是 `{"type":"session","cwd":…}` 的文件被静默跳过；
/// 目标已存在时不覆盖。
///
/// 编码直接用 header 里的原始 cwd 字符串（`cwd.replace(/^[/\\]/, "").replace(/[/\\:]/g, "-")`），
/// **不能**走 `get_default_session_dir_path`——后者会先做 node `path.resolve`，
/// 把相对路径、`..` 与末尾斜杠规范化成不同的目录名。
///
/// 其它约束：
///
/// - 只扫描 agent 目录**顶层**的 `*.jsonl`，不递归；子目录中的会话文件不处理。
/// - 读取整份文件，但只用首行判断 header；随后用 `rename` 移动文件，内容逐字节不变。
/// - `cwd` 缺失、不是字符串或为空字符串都跳过；首行不是 JSON 对象或 `type != "session"`
///   同样跳过，源文件留在原处。
/// - 目录创建与 `rename` 的失败都被忽略（`create_dir_all` 失败后 rename 也会失败，结果
///   只是文件没被移动）；目标已存在时跳过且不覆盖。
pub fn migrate_sessions_from_agent_root() {
    let agent_dir = get_agent_dir();

    // 阶段 1：列出顶层的 `*.jsonl`。read_dir 失败（目录不存在/不可读）等价于没有可迁移
    // 文件；文件名不是合法 UTF-8 的条目在过滤时被排除。
    let Ok(entries) = std::fs::read_dir(&agent_dir) else {
        return;
    };
    let files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".jsonl"))
        })
        .collect();

    // 阶段 2：逐个文件校验 header：先用 JS 的空白集合判断首行是否纯空白（BOM 也算空白），
    // 再解析原始首行。任何不满足条件的文件都留在原处，不产生错误。
    for file in files {
        let Some(content) = read_text(&file) else {
            continue;
        };
        let first_line = content.split('\n').next().unwrap_or("");
        if kiana_tui::text::trim_js(first_line).is_empty() {
            continue;
        }
        let Some(header) = kiana_json::parse(first_line).ok() else {
            continue;
        };
        let Some(header) = header.as_object() else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) != Some("session") {
            continue;
        }
        let Some(cwd) = header.get("cwd").and_then(Value::as_str) else {
            continue;
        };
        if cwd.is_empty() {
            continue;
        }

        // 阶段 3：目录名来自 header 的原始 cwd（不做 path.resolve）；目标已存在时保留
        // 现有文件，源文件不删除。
        let correct_dir = agent_dir
            .join("sessions")
            .join(format!("--{}--", encode_session_cwd(cwd)));
        if !correct_dir.exists() {
            let _ = std::fs::create_dir_all(&correct_dir);
        }

        let Some(file_name) = file.file_name().map(|name| name.to_os_string()) else {
            continue;
        };
        let new_path = correct_dir.join(file_name);
        if new_path.exists() {
            continue;
        }
        let _ = std::fs::rename(&file, &new_path);
    }
}

/// `safePath` 的编码：去掉**一个**开头的 `/` 或 `\`，再把所有 `/`、`\`、`:` 换成 `-`。
fn encode_session_cwd(cwd: &str) -> String {
    let stripped = cwd
        .strip_prefix('/')
        .or_else(|| cwd.strip_prefix('\\'))
        .unwrap_or(cwd);
    stripped
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 扩展系统迁移
// ---------------------------------------------------------------------------

/// `migrateCommandsToPrompts(baseDir, label)`。
///
/// 当 `<baseDir>/commands` 存在且 `<baseDir>/prompts` **不**存在时，把前者整体重命名为
/// 后者，并通过 [`Output`] 输出一行 `Migrated {label} commands/ → prompts/`
/// （`Style` 启用颜色时为绿色）。
///
/// 返回 `true` 仅表示 `rename` 成功；目录不满足条件（没有 `commands/` 或已存在
/// `prompts/`）时返回 `false` 且不输出。`rename` 失败不算致命错误：输出一行
/// `Warning: Could not migrate …`（`Style` 启用颜色时为黄色，文案用 Node 的
/// `error.message`）后继续启动，返回 `false`。`label` 只用于消息文本（`"Global"` /
/// `"Project"`）。
///
/// 输出走 [`Output::process`]；测试或嵌入调用方用
/// [`migrate_commands_to_prompts_with_output`] 注入可捕获的 [`Output`]。
pub fn migrate_commands_to_prompts(base_dir: &Path, label: &str) -> bool {
    migrate_commands_to_prompts_with_output(base_dir, label, &Output::process())
}

/// 指定 [`Output`] 的 [`migrate_commands_to_prompts`]（测试/嵌入调用方使用）。
///
/// 返回语义与 [`migrate_commands_to_prompts`] 相同；[`Output::capture`] 构造的通道让
/// 测试可以逐字断言输出（例如 `commands_rename_failure_warns_with_the_node_fs_message`
/// 断言 rename 失败的警告文本）。
pub fn migrate_commands_to_prompts_with_output(
    base_dir: &Path,
    label: &str,
    output: &Output,
) -> bool {
    let commands_dir = base_dir.join("commands");
    let prompts_dir = base_dir.join("prompts");
    if !commands_dir.exists() || prompts_dir.exists() {
        return false;
    }
    let style = Style::detect();
    match std::fs::rename(&commands_dir, &prompts_dir) {
        Ok(()) => {
            output
                .write_line_stdout(&style.green(&format!("Migrated {label} commands/ → prompts/")));
            true
        }
        Err(error) => {
            // Node 的 `error.message`：`EACCES: permission denied, rename '<src>' -> '<dst>'`。
            let message =
                node_fs_error_message(&error, "rename", Some(&commands_dir), Some(&prompts_dir));
            output.write_line_stdout(&style.yellow(&format!(
                "Warning: Could not migrate {label} commands/ to prompts/: {message}"
            )));
            false
        }
    }
}

/// `migrateKeybindingsConfigFile()`：旧键名 → 命名空间 id，重写时末尾补 `\n`。
///
/// 读取 `<agentDir>/keybindings.json`；文件不存在、读取失败、JSON 解析失败、或顶层不是
/// 对象（`null`/数组/标量）时直接返回——顶层数组在 TS 里也不是普通对象，不做迁移。
/// 改名由 `kiana_config::keybindings::migrate_keybindings_config` 这个纯函数完成；只有
/// `migrated` 为 true 时才重写文件（`migrated` 也覆盖“新名已存在→丢弃旧名”的情况）；
/// 没有迁移迹象时不触碰文件。写回内容是
/// `JSON.stringify(config, null, 2)` 加一个尾随换行；写失败被忽略。
///
/// 这里只改键名与键序，**不**校验值是不是合法键位：那一步在 `KeybindingsManager` 加载
/// 配置时完成，非法值会被丢弃。
pub fn migrate_keybindings_config_file() {
    let config_path = get_agent_dir().join("keybindings.json");
    if !config_path.exists() {
        return;
    }
    let Some(parsed) = parse_json(&config_path) else {
        return;
    };
    let Value::Object(map) = parsed else {
        return;
    };
    let raw_config: IndexMap<String, Value> = map.into_iter().collect();
    let result = kiana_config::keybindings::migrate_keybindings_config(&raw_config);
    if !result.migrated {
        return;
    }
    let config: serde_json::Map<String, Value> = result.config.into_iter().collect();
    let _ = write_text(
        &config_path,
        &format!("{}\n", js_stringify_pretty(&Value::Object(config))),
    );
}

/// `migrateToolsToBin()`：把 `tools/{fd,rg,fd.exe,rg.exe}` 移到 `bin/`。
///
/// `tools/` 不存在时直接返回。四个名字按数组顺序逐个处理：
///
/// - `bin/` 只在确实要处理某个旧文件时才创建；这次 `create_dir_all` 在 TS 里位于 try
///   之外，失败会中止启动（`fatal_node_fs_error`），其余 IO 失败都被忽略。
/// - 目标已存在 → 删除 `tools/` 里的旧文件，保留 `bin/` 中的当前版本（不算“移动”）。
/// - 目标不存在 → `rename`；失败静默，文件留在 `tools/`。
///
/// 只有至少一次 `rename` 成功时才输出 `Migrated managed binaries tools/ → bin/`。
/// `tools/` 目录本身不会被删除，其中的其它文件留给扩展系统检查（见
/// [`migrate_extension_system`]）。
pub fn migrate_tools_to_bin() {
    let agent_dir = get_agent_dir();
    let tools_dir = agent_dir.join("tools");
    let bin_dir = get_bin_dir();

    if !tools_dir.exists() {
        return;
    }

    // 名字顺序即 TS 的处理顺序；`moved_any` 只在 rename 成功时置位，“目标已存在→删除
    // 源文件”不算移动，因此不会单独触发成功提示。
    let mut moved_any = false;
    for binary in ["fd", "rg", "fd.exe", "rg.exe"] {
        let old_path = tools_dir.join(binary);
        let new_path = bin_dir.join(binary);
        if !old_path.exists() {
            continue;
        }
        if !bin_dir.exists()
            && let Err(error) = std::fs::create_dir_all(&bin_dir)
        {
            // TS 的 `mkdirSync` 位于 try 之外：失败会让 runMigrations 抛出并中止启动。
            fatal_node_fs_error("mkdir", Some(&bin_dir), &error);
        }
        if !new_path.exists() {
            if std::fs::rename(&old_path, &new_path).is_ok() {
                moved_any = true;
            }
        } else {
            let _ = std::fs::remove_file(&old_path);
        }
    }

    if moved_any {
        let output = Output::process();
        output.write_line_stdout(&Style::detect().green("Migrated managed binaries tools/ → bin/"));
    }
}

/// `checkDeprecatedExtensionDirs(baseDir, label)`。
///
/// 只检查 `<baseDir>/hooks` 与 `<baseDir>/tools` 是否存在，不递归。`tools/` 的条目名做
/// 小写比较，因此 `FD`/`RG` 等大小写变体与四个受管二进制名一样被忽略；以 `.` 开头的
/// 隐藏文件不算自定义工具。读取 `tools/` 失败时视为没有自定义工具。
fn check_deprecated_extension_dirs(base_dir: &Path, label: &str) -> Vec<String> {
    let hooks_dir = base_dir.join("hooks");
    let tools_dir = base_dir.join("tools");
    let mut warnings: Vec<String> = Vec::new();

    if hooks_dir.exists() {
        warnings.push(format!(
            "{label} hooks/ directory found. Hooks have been renamed to extensions."
        ));
    }

    if tools_dir.exists()
        && let Ok(entries) = std::fs::read_dir(&tools_dir)
    {
        let ignored: BTreeSet<&str> = ["fd", "rg", "fd.exe", "rg.exe"].into_iter().collect();
        let has_custom_tools = entries.filter_map(Result::ok).any(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let lower = name.to_lowercase();
            !ignored.contains(lower.as_str()) && !name.starts_with('.')
        });
        if has_custom_tools {
            warnings.push(format!(
                "{label} tools/ directory contains custom tools. Custom tools have been merged into extensions."
            ));
        }
    }

    warnings
}

/// `migrateExtensionSystem(cwd)`。
///
/// 先对 Global（`get_agent_dir()`）和 Project（`cwd.join(".kiana")`）各做一次
/// `commands/` → `prompts/`，再按同样的 Global → Project 顺序收集弃用目录警告并返回。
/// 警告顺序被测试逐项锁定
/// （`reports_deprecated_hooks_and_custom_tools_directories_in_ts_order`）。
///
/// 每个 scope 最多两条警告：`hooks/` 存在；`tools/` 中存在非受管二进制且不以 `.` 开头的
/// 条目。警告文本里的 label 就是 `"Global"` / `"Project"`，由
/// [`show_deprecation_warnings`] 原样加 `Warning: ` 前缀打印。
pub fn migrate_extension_system(cwd: &Path) -> Vec<String> {
    let agent_dir = get_agent_dir();
    let project_dir = cwd.join(CONFIG_DIR_NAME);

    // 先完成两个 scope 的目录改名，再统一收集警告，保证警告顺序是 Global → Project。
    migrate_commands_to_prompts(&agent_dir, "Global");
    migrate_commands_to_prompts(&project_dir, "Project");

    let mut warnings = check_deprecated_extension_dirs(&agent_dir, "Global");
    warnings.extend(check_deprecated_extension_dirs(&project_dir, "Project"));
    warnings
}

// ---------------------------------------------------------------------------
// 弃用警告
// ---------------------------------------------------------------------------

/// `showDeprecationWarnings(warnings)`：逐条黄色 `Warning:`，随后迁移指南与“按任意键”。
///
/// 从 stdin 阻塞读取一个字节（TS 用 `setRawMode` 读取按键；Rust 侧不切换 termios，
/// 见规划说明的已知偏差）。输出写到进程通道；测试用
/// [`show_deprecation_warnings_with_reader`] 捕获。
///
/// `crate::app::main` 只在 Interactive 模式、且 `deprecation_warnings` 非空时调用。
/// 警告列表为空时立即返回，不读 stdin、也不输出空行。
pub async fn show_deprecation_warnings(warnings: &[String]) {
    show_deprecation_warnings_with_output(warnings, &Output::process()).await;
}

/// 指定 [`Output`] 的 [`show_deprecation_warnings`]。
///
/// 读取 stdin 的工作放到 `spawn_blocking`，避免阻塞 async 执行器；读取结果、EOF 与读取
/// 错误都被丢弃，随后固定输出一个空行（`console.log()` 的等价物）。
pub async fn show_deprecation_warnings_with_output(warnings: &[String], output: &Output) {
    if warnings.is_empty() {
        return;
    }
    write_deprecation_warning_lines(warnings, output);
    // 按键读取在独立阻塞线程中进行；`await` 只等待线程结束，不解释读到的字节。
    let _ = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut byte = [0u8; 1];
        let _ = std::io::stdin().read(&mut byte);
    })
    .await;
    output.write_line_stdout("");
}

/// [`show_deprecation_warnings`] 的可测试版本：用任意 [`tokio::io::AsyncRead`] 代替 stdin。
///
/// 输出顺序严格对齐 TS：警告 → 迁移指南 → “按任意键” → 读一个字节 → 空行。
/// `read` 返回 0（EOF）或错误时同样继续并输出最后的空行，不会挂起调用方。
pub async fn show_deprecation_warnings_with_reader<R>(
    warnings: &[String],
    output: &Output,
    reader: &mut R,
) where
    R: tokio::io::AsyncRead + Unpin,
{
    if warnings.is_empty() {
        return;
    }
    use tokio::io::AsyncReadExt;

    write_deprecation_warning_lines(warnings, output);

    let mut byte = [0u8; 1];
    let _ = reader.read(&mut byte).await;

    output.write_line_stdout("");
}

/// 打印警告正文（不含最后的空行与按键等待）。
///
/// 每条警告前加 `Warning: `；两处空行由字符串里的前导 `\n` 产生。整段文本被
/// `deprecation_warnings_print_the_exact_plain_lines` 逐字锁定。
fn write_deprecation_warning_lines(warnings: &[String], output: &Output) {
    let style = Style::detect();
    for warning in warnings {
        output.write_line_stdout(&style.yellow(&format!("Warning: {warning}")));
    }
    output.write_line_stdout(&style.yellow("\nMove your extensions to the extensions/ directory."));
    output.write_line_stdout(&style.yellow(&format!("Migration guide: {MIGRATION_GUIDE_URL}")));
    output.write_line_stdout(&style.yellow(&format!("Documentation: {EXTENSIONS_DOC_URL}")));
    output.write_line_stdout(&style.dim("\nPress any key to continue..."));
}

/// `runMigrations(cwd)`。
///
/// 按固定顺序执行全部启动迁移（见模块文档的“执行流程与失败语义”）。`cwd` 只用于定位
/// 项目级配置 `<cwd>/.kiana`；agent 目录每次通过 `get_agent_dir()` 解析。
///
/// 返回 [`MigrationResult`]：
///
/// - `migrated_auth_providers` 来自 [`migrate_auth_to_auth_json`]（oauth 的 provider 在前、
///   settings 的 `apiKeys` 在后）；
/// - `deprecation_warnings` 来自 [`migrate_extension_system`]（Global 在前、Project 在后）。
///
/// 除 `auth.json` 与 `bin/` 目录的致命 IO 失败（panic，见模块文档）外，其余失败都被忽略。
/// 函数同步执行、不做网络 IO；重复调用是安全的（每个迁移先检查目标状态）。
pub fn run_migrations(cwd: &Path) -> MigrationResult {
    // 顺序即 TS `runMigrations` 的调用顺序；返回值只聚合 auth 与扩展系统两步的结果。
    let migrated_auth_providers = migrate_auth_to_auth_json();
    migrate_sessions_from_agent_root();
    migrate_tools_to_bin();
    migrate_keybindings_config_file();
    let deprecation_warnings = migrate_extension_system(cwd);
    MigrationResult {
        migrated_auth_providers,
        deprecation_warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stringify_pretty_matches_json_stringify_for_objects_and_arrays() {
        let value = json!({
            "a": 1,
            "b": [true, null, "x\ny"],
            "c": { "d": 1.0, "e": {} },
            "f": [],
        });
        assert_eq!(
            js_stringify_pretty(&value),
            "{\n  \"a\": 1,\n  \"b\": [\n    true,\n    null,\n    \"x\\ny\"\n  ],\n  \"c\": {\n    \"d\": 1,\n    \"e\": {}\n  },\n  \"f\": []\n}"
        );
    }

    #[test]
    fn stringify_pretty_formats_numbers_like_js() {
        assert_eq!(js_stringify_pretty(&json!(1.0)), "1");
        assert_eq!(js_stringify_pretty(&json!(1.5)), "1.5");
        assert_eq!(js_stringify_pretty(&json!(-0.0)), "0");
        assert_eq!(js_stringify_pretty(&json!(1e21)), "1e+21");
        assert_eq!(js_stringify_pretty(&json!(1e-7)), "1e-7");
        assert_eq!(js_stringify_pretty(&json!(1.25e-8)), "1.25e-8");
        assert_eq!(
            js_stringify_pretty(&json!(1234567890123456789_u64)),
            "1234567890123456800"
        );
    }

    #[test]
    fn stringify_pretty_escapes_strings_like_js() {
        assert_eq!(
            js_stringify_pretty(&json!("\u{1}\"\\\u{8}\u{c}\n\r\t")),
            "\"\\u0001\\\"\\\\\\b\\f\\n\\r\\t\""
        );
    }

    #[tokio::test]
    async fn deprecation_warnings_print_the_exact_plain_lines() {
        let output = Output::capture();
        let warnings = vec![
            "Global hooks/ directory found. Hooks have been renamed to extensions.".to_string(),
            "Project tools/ directory contains custom tools. Custom tools have been merged into extensions.".to_string(),
        ];
        let mut reader: &[u8] = b"x";
        show_deprecation_warnings_with_reader(&warnings, &output, &mut reader).await;

        let plain = kiana_core::ansi::strip_ansi(&output.captured_stdout());
        assert_eq!(
            plain,
            "Warning: Global hooks/ directory found. Hooks have been renamed to extensions.\n\
Warning: Project tools/ directory contains custom tools. Custom tools have been merged into extensions.\n\
\nMove your extensions to the extensions/ directory.\n\
Migration guide: https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/CHANGELOG.md#extensions-migration\n\
Documentation: https://github.com/earendil-works/kiana/blob/main/packages/coding-agent/docs/extensions.md\n\
\nPress any key to continue...\n\
\n"
        );
    }

    #[tokio::test]
    async fn deprecation_warnings_are_silent_when_empty() {
        let output = Output::capture();
        let mut reader: &[u8] = b"";
        show_deprecation_warnings_with_reader(&[], &output, &mut reader).await;
        assert_eq!(output.captured_stdout(), "");
    }
}
