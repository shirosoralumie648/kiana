//! `oauth/types.ts`：OAuth 元数据的结构化校验（`spec/telemetry-mcp.md` §18.1）。
//!
//! TS 源：`packages/mcp/src/oauth/types.ts`
//! （改编自 `modelcontextprotocol/typescript-sdk` v1.29.0，MIT，见模块底部的许可声明）。
//!
//! # 形状：Map 支撑的结构体
//!
//! TS 的每个 `parse*` 都是「展开输入 → 覆盖已知字段 → 丢弃 `undefined`」：
//! `compact({...input, known: validated, ...})`。因此**未知字段会被保留**，且键序 =
//! JS 对象序（整数样式键在前，其余插入序）。Rust 侧把这些类型建模成
//! `serde_json::Map<String, Value>` 支撑的结构体（[`OAuthProtectedResourceMetadata`]、
//! [`AuthorizationServerMetadata`]、[`OAuthTokens`]、[`OAuthClientInformation`]、
//! [`OAuthClientMetadata`]）：解析结果可以**逐字**往返，已知字段另有类型化访问器。
//!
//! 「写入位置」与 JS 的对象字面量一致：输入里已存在的键保持原位，新键按 TS 字面量的属性
//! 顺序追加；值为 `undefined`（即校验后不存在）的已知字段从结果里**删除**（`compact`）。
//! [`OAuthDiscoveryState`] 与 [`OAuthServerInfo`] 没有索引签名（TS 里不会出现未知字段），
//! 因此是普通字段结构体。
//!
//! # 校验规则（逐字复刻，勿"改进"）
//!
//! - 所有失败都是普通 `Error("Invalid {name}")`（[`OAuthFailure::Error`]）；`name` 的取值见
//!   §18.1，例如 `OAuth protected resource metadata resource`、`authorization server issuer`。
//! - [`required_string`]：非字符串或**空串** → 抛错。
//! - [`optional_string`] / [`optional_url`] 用 [`absent`]：`undefined`/`null`/`""` 都算缺省。
//! - [`optional_strings`] **只**把 `undefined`/`null` 当缺省；`""` 会抛 `Invalid {name}`（§30.1 第 16 条）。
//! - [`safe_url`] 只接受 `http:`/`https:`（服务器提供的 URL 会被平台 URL 处理器打开，
//!   `file:`/自定义协议会启动本地程序）。
//! - `expires_in` 走 JS `Number()`：`"0x10"`→16、`"1e3"`→1000、`[]`→0、`true`→1、
//!   超长数字串→非有限→`Invalid expires_in`；`null`/`""` 是缺省（不是 `Number(null) === 0`）。
//! - 数字字段用 `typeof === "number"` 判定，非数字（含 `null`、数字字符串）→ 省略。
//!
//! # 已知的被迫偏差（`ARCHITECTURE.md` §8 第 31、32 条）
//!
//! TS 的 `JSON.parse` 能表示 `Infinity`（`1e400`）与孤立代理项；`serde_json::Value` 不能。
//! `kiana_json::js_parse::parse` 按架构决策把它们落成 `Value::Null` 与 U+FFFD，因此本模块在
//! **病态 JSON** 上有两处与 TS 不同的可观察行为（正常响应用不到）：
//!
//! - `expires_in: 1e400`：TS 的 `Number.isFinite(Infinity)` 为假 → `Error("Invalid expires_in")`；
//!   Rust 收到 `null`（[`absent`] 为真）→ 当作缺省，解析成功且结果里没有 `expires_in`。
//! - `client_id_issued_at` / `client_secret_expires_at: 1e400`：TS 的 `typeof === "number"` 为真
//!   → 键保留（`Infinity` 序列化成 `null`）；Rust 收到 `null` → `typeof` 不是 number，键被删除。
//! - 字符串值里的孤立代理项：TS 原样往返成 `"\ud800"`，Rust 得到 `"\u{FFFD}"`。
//!
//! 三者都要改 §4.9 的数字/字符串契约才能消除，见 `ARCHITECTURE.md` §8 第 31、32 条与回归测试
//! `non_finite_numbers_and_lone_surrogates_follow_the_forced_deviations`。
//!
//! # 不进入包根导出（§30.1）
//!
//! [`safe_url`]、[`object`]、[`compact`]、[`required_string`]、[`absent`]、[`optional_string`]、
//! [`optional_strings`]、[`is_http_url`] 与四个 `parse_*` 在 TS 里只被包内使用：它们在
//! `kiana_mcp::oauth::types` 里是 `pub`，但**不**出现在 `oauth/index.ts` 的再导出清单里。
//!
//! # 在本 crate 中的位置
//!
//! 上游（crate 内）：只依赖 `crate::jsonrpc::is_object` 与 `crate::oauth::errors::OAuthFailure`
//! （错误信封），不依赖 `oauth` 的其它子模块（`discovery`/`flow`/`provider` 都是反过来依赖
//! 本文件；`callback` 不直接引用本文件，只经 `discovery::js_url` 间接位于下游）；此外用到
//! `kiana_json` 的 JS 语义辅助与 `serde`/`serde_json`/`url` 三个通用库。下游（路径相对
//! `crates/kiana-mcp/src/oauth/`）：
//!
//! - `discovery.rs`：用 [`parse_protected_resource_metadata`] /
//!   [`parse_authorization_server_metadata`] 校验 well-known 文档并构造 [`OAuthServerInfo`]；
//!   `parse_www_authenticate` 构造 [`OAuthChallenge`]。
//! - `flow.rs`：用 [`parse_oauth_tokens`] 解析令牌响应、[`parse_client_information`] 解析动态
//!   客户端注册响应，并在发现流程中读写 [`OAuthDiscoveryState`]。
//! - `provider.rs`：把 [`OAuthTokens`]、[`OAuthClientInformationMixed`]、[`OAuthDiscoveryState`]
//!   作为 `McpOAuthState` 的字段（其 serde 形状就是嵌入方写入 `mcp-auth.json` 的值形状），
//!   并用 `js_number` 从 `expires_in` 计算 `tokensExpireAt`。
//!
//! crate 外：`crates/kiana-builtin-extensions/src/mcp/oauth.rs` 消费 [`OAuthChallenge`]、
//! [`AuthorizationServerMetadata`] 的访问器与 [`OAuthClientInformationMixed`]。
//!
//! # 阅读顺序
//!
//! 1. 标量辅助（[`object`]、[`absent`]、[`required_string`]、[`optional_string`]、
//!    [`optional_strings`]、[`safe_url`]、[`js_number`]）：TS 校验规则的实际落点，错误文案
//!    里的 `name` 由调用方传入；
//! 2. `fields_object!` 宏：五个 Map 支撑类型的共同形状与访问器；
//! 3. 四个 `parse_*`：按「校验 → 写回 → `compact`」的顺序对照 TS 源码阅读；
//! 4. [`OAuthDiscoveryState`] / [`OAuthServerInfo`]（serde 结构体）与 [`OAuthChallenge`]
//!    （无 serde 派生，用 `to_value` 生成 JSON 值）。
//!
//! # 如何验证改动
//!
//! `crates/kiana-mcp/tests/oauth_core.rs` 用 TS 参照实现（Node 26）生成的黄金向量
//! （`crates/kiana-mcp/tests/fixtures/oauth_types_golden.json` 与
//! `oauth_types_fuzz_golden.json`）逐字比较四个 `parse_*` 的 JSON 输出与错误消息；
//! 本文件的单元测试另外锁定错误优先级与 `ARCHITECTURE.md` §8 的被迫偏差。改动解析逻辑时
//! 两处都必须通过，不能只更新其中一边。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

use crate::jsonrpc::is_object;
use crate::oauth::errors::OAuthFailure;

/// `object(value, name)`：非数组对象，否则 `Error("Invalid {name}")`。
pub fn object<'a>(value: &'a Value, name: &str) -> Result<&'a Map<String, Value>, OAuthFailure> {
    if !is_object(value) {
        return Err(OAuthFailure::error(format!("Invalid {name}")));
    }
    value
        .as_object()
        .ok_or_else(|| OAuthFailure::error(format!("Invalid {name}")))
}

/// `compact(value)`：丢弃值为 `undefined` 的自有可枚举键。
///
/// JSON 值里没有 `undefined`（Rust 用「键不存在」表达），因此这里原样返回；保留这个名字是
/// 为了让 §30.1 的模块级导出面对得上，并让 [`parse_protected_resource_metadata`] 等函数的
/// 「先展开再覆盖再 compact」结构一眼可辨。
pub fn compact(fields: Map<String, Value>) -> Map<String, Value> {
    fields
}

/// `requiredString(value, name)`：必须是**非空**字符串。
pub fn required_string(value: Option<&Value>, name: &str) -> Result<String, OAuthFailure> {
    match value {
        Some(Value::String(text)) if !text.is_empty() => Ok(text.clone()),
        _ => Err(OAuthFailure::error(format!("Invalid {name}"))),
    }
}

/// `absent(value)`：`undefined`/`null`/`""` 视为缺省（服务器常用它们表达"没有值"）。
pub fn absent(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => true,
        Some(Value::String(text)) => text.is_empty(),
        Some(_) => false,
    }
}

/// `optionalString(value, name)`：缺省 → `None`，否则必须是非空字符串。
pub fn optional_string(value: Option<&Value>, name: &str) -> Result<Option<String>, OAuthFailure> {
    if absent(value) {
        return Ok(None);
    }
    required_string(value, name).map(Some)
}

/// `optionalStrings(value, name)`：**只**把 `undefined`/`null` 当缺省（`""` 会抛错）。
///
/// 空数组 → `Some(vec![])`；任一项不是字符串（含嵌套数组/对象/null）→ `Invalid {name}`。
/// 返回的是新 `Vec`（逐项克隆），调用方随后通常用 `set_field` 写回结果对象。
pub fn optional_strings(
    value: Option<&Value>,
    name: &str,
) -> Result<Option<Vec<String>>, OAuthFailure> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(text) => out.push(text.clone()),
                    _ => return Err(OAuthFailure::error(format!("Invalid {name}"))),
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(OAuthFailure::error(format!("Invalid {name}"))),
    }
}

/// `isHttpUrl(url)`：协议为 `http:` 或 `https:`。
pub fn is_http_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// `safeUrl(value, name)`：非空字符串 + `URL.canParse` + http(s)，返回**原始文本**。
///
/// 只接受 http(s) 的理由：授权端点会被平台 URL 处理器打开，`file:`、`search-ms:`、`ms-msdt:`
/// 之类的协议会启动本地程序（CHANGELOG 里 1.2.0 之后的安全修复）。
pub fn safe_url(value: Option<&Value>, name: &str) -> Result<String, OAuthFailure> {
    let text = required_string(value, name)?;
    // TS 的 `URL.canParse(text)` + `isHttpUrl(new URL(text))`；解析失败同样是 `Invalid {name}`。
    match Url::parse(&text) {
        Ok(url) if is_http_url(&url) => Ok(text),
        _ => Err(OAuthFailure::error(format!("Invalid {name}"))),
    }
}

/// `optionalUrl(value, name)`：缺省 → `None`，否则 [`safe_url`]。
pub fn optional_url(value: Option<&Value>, name: &str) -> Result<Option<String>, OAuthFailure> {
    if absent(value) {
        return Ok(None);
    }
    safe_url(value, name).map(Some)
}

/// `Number(value)`（JS 的 `ToNumber`）：`expires_in` 的强制转换。
///
/// 本模块内只有 [`parse_oauth_tokens`] 调用它，且该调用点已排除 `undefined`/`null`/`""`。
/// `oauth::provider` 也直接调用它（`save_tokens` 对已保存的 `expires_in` 求 `Number()`，
/// 那里 `null`/`""` 按 0 处理，是有意复刻 JS 的 `ToNumber`）。
/// - 布尔 → `1`/`0`；数字 → 自身；
/// - 字符串 → [`kiana_json::js_num::parse_js_number`]（`"0x10"`→16、`"1e3"`→1000、`"abc"`→`NaN`）；
/// - 数组 → `String(array)`（逗号连接）再按字符串解析（`[]`→0、`[5]`→5、`[1,2]`→`NaN`）；
/// - 对象 → `NaN`（`"[object Object]"` 不是数字）。
pub fn js_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(boolean) => {
            if *boolean {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => kiana_json::js_num::parse_js_number(text),
        // `String(array)` 是 `Array.prototype.join(",")`：`[]` → `""` → 0、`[5]` → `"5"`、
        // `[1,2]` → `"1,2"` → NaN。
        Value::Array(_) => {
            kiana_json::js_num::parse_js_number(&kiana_json::js_string::js_string(Some(value)))
        }
        Value::Object(_) => f64::NAN,
    }
}

/// JS 数字 → JSON 值：整数（含 `-0`→`0`）写成整数字面量，其余写成 f64。
///
/// `JSON.stringify` 只看数值，`3600` 与 `3600.0` 都是 `"3600"`；统一存成整数能让
/// `Value` 的相等比较与外部期望（`json!(3600)`）一致。超过 2^53 的整数保留 f64 语义
/// （JS 的 `JSON.parse` 本来就会把它舍入成 f64）。
///
/// `debug_assert!` 记录「写进 JSON 的数字必须有限」这一不变量（`serde_json::Number`
/// 表示不了 NaN/Infinity）。release 构建下断言不生效，若真收到非有限值，
/// `Number::from_f64` 返回 `None`，`unwrap_or_else` 会把它写成 `0`。
fn js_number_value(value: f64) -> Value {
    debug_assert!(
        value.is_finite(),
        "JS numbers written to JSON must be finite"
    );
    if value == 0.0 {
        return Value::Number(serde_json::Number::from(0));
    }
    if value.fract() == 0.0 && value.abs() <= 9_007_199_254_740_992.0 {
        return Value::Number(serde_json::Number::from(value as i64));
    }
    Value::Number(
        serde_json::Number::from_f64(value).unwrap_or_else(|| serde_json::Number::from(0)),
    )
}

/// 输入对象按 JS 键序重排后的副本（`{...input}` 的键序）。
///
/// 必须克隆：`parse_*` 拿到的是借用的 `&Map`，而结果要在输入的基础上覆盖字段、因此需要
/// 自己的所有权。`sort_js_keys` 顺带重建整棵树（嵌套容器也会递归归一），所以这里是输入的
/// 深拷贝；元数据文档很大时这次复制的代价与输入规模成正比（见 `kiana_json::js_object`
/// 的模块文档）。
fn js_ordered(input: &Map<String, Value>) -> Map<String, Value> {
    match kiana_json::js_object::sort_js_keys(&Value::Object(input.clone())) {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// `{...result, key: value}` / `{...result, key: undefined}`（后者从结果里删除）。
///
/// 有值 → `Map::insert`：键已存在时覆盖并保持原位，否则追加到末尾（indexmap 语义）。
/// `parse_*` 的每个已知字段都经过它。对三个展开输入的 `parse_*`
/// （[`parse_protected_resource_metadata`]、[`parse_authorization_server_metadata`]、
/// [`parse_client_information`]），「校验通过就覆盖、缺省就删除」与 TS 的 `{...input, key}` +
/// `compact` 一一对应；[`parse_oauth_tokens`] 例外：TS 是 `compact({access_token, …})`
/// （没有 `...input`），Rust 从空 `Map` 开始，只按字面量顺序追加已知字段，未知字段不会出现。
///
/// 删除必须用 [`Map::shift_remove`]：`preserve_order` 下 [`Map::remove`] 是 `swap_remove`
/// （把最后一个元素挪到被删位置），而 JS 的 `delete`/`Object.fromEntries` 保留其余键的相对顺序。
fn set_field(fields: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    match value {
        Some(value) => {
            fields.insert(key.to_owned(), value);
        }
        None => {
            fields.shift_remove(key);
        }
    }
}

// 五个 Map 支撑类型的共同实现。用宏而不是手写五份：它们在 TS 里是五个接口，但 Rust
// 侧形状完全相同（一个 `serde_json::Map` + 同一组访问器），手写多份会让「对已知字段的
// 读写语义」悄悄分叉，而它们必须保持一致。
//
// `#[serde(transparent)]` 让序列化结果就是裸 JSON 对象（没有 `fields` 这层包装），
// 反序列化则直接委托给 `Map<String, Value>`：输入必须是 JSON 对象，数组/标量会失败。
//
// 访问器是**宽容读取**：类型不符或缺席都返回 `None`，`string_array` 还会跳过非字符串
// 元素。严格校验只发生在 `parse_*` 里；对 `from_fields` 手工构造、绕过了校验的对象，
// 这些访问器不做保证。
//
// 键序：workspace 的 `serde_json` 开了 `preserve_order`，`Map` 的插入序就是输出序。
// `set` 覆盖已有键时保持原位，`remove` 用 `shift_remove` 保住其余键的相对顺序；新键
// 追加到末尾（与 JS 的 `obj[key] = value` 一致）。这里**不做**整数样式键的 JS 排序，
// 那一步只在 `parse_*` 入口由 `js_ordered` 完成。
macro_rules! fields_object {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name {
            fields: Map<String, Value>,
        }

        impl $name {
            /// 空对象（`{}`）。
            pub fn new() -> Self {
                $name {
                    fields: Map::new(),
                }
            }

            /// 用已有键值构造（键序原样保留，调用方负责用 JS 键序）。
            pub fn from_fields(fields: Map<String, Value>) -> Self {
                $name { fields }
            }

            /// 原始键值（JS 对象；键序即 `Object.keys()` 的顺序）。
            pub fn fields(&self) -> &Map<String, Value> {
                &self.fields
            }

            /// 可变访问（`obj[key] = value` / `delete obj[key]`）。
            pub fn fields_mut(&mut self) -> &mut Map<String, Value> {
                &mut self.fields
            }

            /// `obj[key]`（缺失为 `undefined`）。
            pub fn get(&self, key: &str) -> Option<&Value> {
                self.fields.get(key)
            }

            /// `obj[key] = value`；已存在的键保持原位，新键追加到末尾。
            ///
            /// 注意：整数样式的新键不会像 JS 那样排到对象最前；需要 JS 键序时先走 `parse_*`
            /// 或自行调用 `kiana_json::js_object::sort_js_keys`。
            pub fn set(&mut self, key: impl Into<String>, value: Value) {
                self.fields.insert(key.into(), value);
            }

            /// `delete obj[key]`（`shift_remove`：保留其余键的相对顺序，见 `set_field`）。
            pub fn remove(&mut self, key: &str) -> Option<Value> {
                self.fields.shift_remove(key)
            }

            /// `JSON.stringify` 之前的 `Value` 形态（键序保留）。
            pub fn to_value(&self) -> Value {
                Value::Object(self.fields.clone())
            }

            /// 字符串数组字段（非数组或缺席 → `None`；非字符串元素被跳过）。
            ///
            /// 宽容读取：空数组 → `Some(vec![])`，元素类型不报错。解析输入时的严格版本是
            /// [`optional_strings`]（非字符串元素会抛 `Invalid {name}`）。
            pub fn string_array(&self, key: &str) -> Option<Vec<&str>> {
                let items = self.fields.get(key)?.as_array()?;
                Some(items.iter().filter_map(Value::as_str).collect())
            }

            /// 字符串字段（非字符串或缺席 → `None`）。
            pub fn string(&self, key: &str) -> Option<&str> {
                self.fields.get(key)?.as_str()
            }

            /// 布尔字段（非布尔或缺席 → `None`）。
            pub fn boolean(&self, key: &str) -> Option<bool> {
                self.fields.get(key)?.as_bool()
            }

            /// 数字字段（非数字或缺席 → `None`）。
            pub fn number(&self, key: &str) -> Option<f64> {
                self.fields.get(key)?.as_f64()
            }
        }

        impl From<Map<String, Value>> for $name {
            fn from(fields: Map<String, Value>) -> Self {
                $name { fields }
            }
        }
    };
}

fields_object! {
    /// `OAuthProtectedResourceMetadata`（RFC 9728 的受保护资源元数据）。
    ///
    /// 已知字段：`resource`（必填、安全 URL）、`authorization_servers`（每项安全 URL）、
    /// `scopes_supported`。未知字段保留。
    OAuthProtectedResourceMetadata
}

impl OAuthProtectedResourceMetadata {
    /// `resource`（解析后必定存在）。
    pub fn resource(&self) -> Option<&str> {
        self.string("resource")
    }

    /// `authorization_servers`（缺席 → `None`；空数组 → `Some(vec![])`）。
    pub fn authorization_servers(&self) -> Option<Vec<&str>> {
        self.string_array("authorization_servers")
    }

    /// `authorization_servers?.[0] ?? undefined`。
    pub fn first_authorization_server(&self) -> Option<&str> {
        self.string_array("authorization_servers")?
            .into_iter()
            .next()
    }

    /// `scopes_supported`（缺席 → `None`；空数组 → `Some(vec![])`）。
    pub fn scopes_supported(&self) -> Option<Vec<&str>> {
        self.string_array("scopes_supported")
    }
}

fields_object! {
    /// `AuthorizationServerMetadata`（RFC 8414 / OpenID Connect Discovery）。
    ///
    /// 已知字段：`issuer`、`authorization_endpoint`、`token_endpoint`、`registration_endpoint`、
    /// `scopes_supported`、`response_types_supported`（必填数组）、`grant_types_supported`、
    /// `token_endpoint_auth_methods_supported`、`code_challenge_methods_supported`、
    /// `client_id_metadata_document_supported`、
    /// `authorization_response_iss_parameter_supported`（RFC 9207）。未知字段保留。
    AuthorizationServerMetadata
}

impl AuthorizationServerMetadata {
    /// `issuer`（解析后必定存在）。
    pub fn issuer(&self) -> Option<&str> {
        self.string("issuer")
    }

    /// `authorization_endpoint`（解析后必定存在）。
    pub fn authorization_endpoint(&self) -> Option<&str> {
        self.string("authorization_endpoint")
    }

    /// `token_endpoint`（解析后必定存在）。
    pub fn token_endpoint(&self) -> Option<&str> {
        self.string("token_endpoint")
    }

    /// `registration_endpoint`。
    pub fn registration_endpoint(&self) -> Option<&str> {
        self.string("registration_endpoint")
    }

    /// `scopes_supported`。
    pub fn scopes_supported(&self) -> Option<Vec<&str>> {
        self.string_array("scopes_supported")
    }

    /// `response_types_supported`（解析后必定存在）。
    pub fn response_types_supported(&self) -> Option<Vec<&str>> {
        self.string_array("response_types_supported")
    }

    /// `grant_types_supported`。
    pub fn grant_types_supported(&self) -> Option<Vec<&str>> {
        self.string_array("grant_types_supported")
    }

    /// `token_endpoint_auth_methods_supported`（缺席按 TS 的 `?? []` 读作空列表）。
    pub fn token_endpoint_auth_methods_supported(&self) -> Vec<&str> {
        self.string_array("token_endpoint_auth_methods_supported")
            .unwrap_or_default()
    }

    /// `code_challenge_methods_supported`。
    pub fn code_challenge_methods_supported(&self) -> Option<Vec<&str>> {
        self.string_array("code_challenge_methods_supported")
    }

    /// `client_id_metadata_document_supported`（非布尔 → `None`）。
    pub fn client_id_metadata_document_supported(&self) -> Option<bool> {
        self.boolean("client_id_metadata_document_supported")
    }

    /// `authorization_response_iss_parameter_supported`（非布尔 → `None`）。
    pub fn authorization_response_iss_parameter_supported(&self) -> Option<bool> {
        self.boolean("authorization_response_iss_parameter_supported")
    }
}

fields_object! {
    /// `OAuthTokens`（RFC 6749 §5.1 的令牌响应）。
    ///
    /// 已知字段：`access_token`、`token_type`、`expires_in`、`scope`、`refresh_token`、`id_token`。
    ///
    /// **与同模块其它类型不同**：`parseOAuthTokens` 的结果只含这六个键（TS 里
    /// `compact({...})` 没有 `...input`），未知字段被丢弃、键序固定为字面量顺序。
    OAuthTokens
}

impl OAuthTokens {
    /// `access_token`（解析后必定存在）。
    pub fn access_token(&self) -> Option<&str> {
        self.string("access_token")
    }

    /// `token_type`（解析后必定存在）。
    pub fn token_type(&self) -> Option<&str> {
        self.string("token_type")
    }

    /// `expires_in`（秒；解析时只检查有限，不检查正负/范围，`0` 或负数会原样保留。
    /// `oauth::provider` 的 `McpOAuthProvider::save_tokens` 把它乘以 1000 加到当前时间）。
    pub fn expires_in(&self) -> Option<f64> {
        self.number("expires_in")
    }

    /// `scope`。
    pub fn scope(&self) -> Option<&str> {
        self.string("scope")
    }

    /// `refresh_token`。
    pub fn refresh_token(&self) -> Option<&str> {
        self.string("refresh_token")
    }

    /// `id_token`。
    pub fn id_token(&self) -> Option<&str> {
        self.string("id_token")
    }
}

fields_object! {
    /// `OAuthClientInformation`（动态客户端注册响应；同时承载 `OAuthClientInformationFull`
    /// 的客户端元数据字段与 `OAuthClientInformationMixed` 的两种形态）。
    ///
    /// 已知字段：`client_id`（必填）、`client_secret`、`client_id_issued_at`、
    /// `client_secret_expires_at`、`redirect_uris`（解析后总是数组），其余字段（客户端元数据）
    /// 原样保留。
    OAuthClientInformation
}

impl OAuthClientInformation {
    /// `client_id`（解析后必定存在）。
    pub fn client_id(&self) -> Option<&str> {
        self.string("client_id")
    }

    /// `client_secret`。
    pub fn client_secret(&self) -> Option<&str> {
        self.string("client_secret")
    }

    /// `client_id_issued_at`。
    pub fn client_id_issued_at(&self) -> Option<f64> {
        self.number("client_id_issued_at")
    }

    /// `client_secret_expires_at`。
    pub fn client_secret_expires_at(&self) -> Option<f64> {
        self.number("client_secret_expires_at")
    }

    /// `redirect_uris`（解析后总是数组；手工构造时可能缺席）。
    pub fn redirect_uris(&self) -> Option<Vec<&str>> {
        self.string_array("redirect_uris")
    }

    /// `token_endpoint_auth_method` 提示值（`OAuthClientInformationMixed` 的 Full 形态才有）。
    pub fn token_endpoint_auth_method(&self) -> Option<&str> {
        self.string("token_endpoint_auth_method")
    }
}

/// `OAuthClientInformationFull = OAuthClientInformation & OAuthClientMetadata`。
///
/// TS 是结构交叉类型；Rust 侧同一个 Map 支撑的类型可以承载两者（未知/元数据字段原样保留），
/// 因此是别名而不是第二个定义。
pub type OAuthClientInformationFull = OAuthClientInformation;

/// `OAuthClientInformationMixed = OAuthClientInformation | OAuthClientInformationFull`。
///
/// 后者是前者的超集，Rust 侧用同一个类型表达（见 [`OAuthClientInformationFull`]）。
pub type OAuthClientInformationMixed = OAuthClientInformation;

fields_object! {
    /// `OAuthClientMetadata`（RFC 7591 §2 的客户端元数据；DCR 请求体的主体）。
    ///
    /// 已知字段：`redirect_uris`、`token_endpoint_auth_method`、`grant_types`、`response_types`、
    /// `application_type`、`client_name`、`client_uri`、`logo_uri`、`scope`、`contacts`、
    /// `tos_uri`、`policy_uri`、`jwks_uri`、`jwks`、`software_id`、`software_version`、
    /// `software_statement`。未知字段原样保留（TS 的 DCR 体是 `JSON.stringify({...clientMetadata, ...})`）。
    OAuthClientMetadata
}

impl OAuthClientMetadata {
    /// `redirect_uris`（`OAuthClientMetadata` 里是必填数组；provider 的选项里可省略）。
    pub fn redirect_uris(&self) -> Option<Vec<&str>> {
        self.string_array("redirect_uris")
    }

    /// `token_endpoint_auth_method`。
    pub fn token_endpoint_auth_method(&self) -> Option<&str> {
        self.string("token_endpoint_auth_method")
    }

    /// `application_type`（`native` / `web`；缺席时由 `redirect_uris` 推导）。
    pub fn application_type(&self) -> Option<&str> {
        self.string("application_type")
    }

    /// `grant_types`。
    pub fn grant_types(&self) -> Option<Vec<&str>> {
        self.string_array("grant_types")
    }

    /// `response_types`。
    pub fn response_types(&self) -> Option<Vec<&str>> {
        self.string_array("response_types")
    }

    /// `scope`。
    pub fn scope(&self) -> Option<&str> {
        self.string("scope")
    }

    /// `client_name`。
    pub fn client_name(&self) -> Option<&str> {
        self.string("client_name")
    }
}

/// `OAuthDiscoveryState`：`McpOAuthProvider` 持久化的发现结果（coding-agent 写入
/// `mcp-auth.json` 的 schema 一部分）。
///
/// TS 是普通接口（无索引签名），键序 = 字面量顺序；Rust 用同序字段 +
/// `skip_serializing_if` 复刻 `JSON.stringify` 的省略语义。
///
/// 序列化：`#[serde(rename_all = "camelCase")]` 把字段名写成 `authorizationServerUrl` 等；
/// `authorization_server_url` 没有 `skip_serializing_if`，总会写出，三个 `Option` 字段为
/// `None` 时整个省略（对应 TS 的 `undefined`）。反序列化：缺失的可选字段由
/// `#[serde(default)]` 读成 `None`。本类型只描述值形状；落盘由嵌入方经
/// `oauth::provider::McpOAuthState` 完成，本文件不做文件 IO。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDiscoveryState {
    /// `authorizationServerUrl`（发现到的授权服务器 URL）。
    pub authorization_server_url: String,
    /// `authorizationServerMetadata`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server_metadata: Option<AuthorizationServerMetadata>,
    /// `resourceMetadata`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<OAuthProtectedResourceMetadata>,
    /// `resourceMetadataUrl`（传入的显式元数据 URL 的 `href`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata_url: Option<String>,
}

/// `OAuthServerInfo`：`discoverOAuthServerInfo` 的结果。
///
/// 字段名与 [`OAuthDiscoveryState`] 的同名项一致（camelCase、`None` 省略、
/// `authorization_server_url` 总会写出）。`oauth::flow` 在未配置
/// `authorization_server_metadata_url` 时把它转成 [`OAuthDiscoveryState`] 交给 provider
/// 持久化：判断条件只看这个选项，与 `resource_metadata_url` 无关；结果来自 discovery 缓存
/// （没有调用 `discover_oauth_server_info`）时同样会走这条路径。两者的同名字段因此需要
/// 保持同步。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthServerInfo {
    /// `authorizationServerUrl`。来源三选一（见 `oauth::discovery` 的
    /// `discover_oauth_server_info`）：显式给了 `authorizationServerMetadataUrl` 时取
    /// `metadata.issuer`；否则取 `resourceMetadata.authorization_servers[0]`；都没有时取
    /// server URL 的根 URL（`new URL('/', serverUrl)` 的序列化结果：带尾斜杠，保留 userinfo 与
    /// 端口；`file:` 例外。与去掉 userinfo 的 origin 不同）。
    pub authorization_server_url: String,
    /// `authorizationServerMetadata`（全部发现候选都 4xx/502 时为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_server_metadata: Option<AuthorizationServerMetadata>,
    /// `resourceMetadata`（发现失败被吞掉时为 `None`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_metadata: Option<OAuthProtectedResourceMetadata>,
}

/// `OAuthChallenge`：`WWW-Authenticate` 的解析结果（`parseWwwAuthenticate`）。
///
/// TS 返回的对象**总是**有这四个键（值可能是 `undefined`）；`JSON.stringify` 会省略
/// `undefined`，所以 Rust 用 `Option` 表达同样的线上形状。
///
/// 本类型没有 serde 派生：它在仓库里只在内存中流动（`oauth::discovery` 构造，
/// 授权流程与 `kiana-builtin-extensions` 消费）；需要 JSON 值时用
/// [`OAuthChallenge::to_value`]（当前仓库内只有测试调用它）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OAuthChallenge {
    /// `resourceMetadataUrl`（`new URL(...)` 解析失败时为 `None`）。
    pub resource_metadata_url: Option<Url>,
    /// `scope`。
    pub scope: Option<String>,
    /// `error`（例如 `insufficient_scope`、`invalid_token`）。
    pub error: Option<String>,
    /// `errorDescription`。
    pub error_description: Option<String>,
}

impl OAuthChallenge {
    /// `JSON.stringify(challenge)` 的值形态：只有存在（非 `undefined`）的键才出现。
    ///
    /// 键序同 TS 的对象字面量：`resourceMetadataUrl`、`scope`、`error`、`errorDescription`。
    ///
    /// `resource_metadata_url` 用 `Url::as_str()` 写成字符串（解析后的序列化文本，不是
    /// `Debug` 形式）；其余字段原样拷贝。
    pub fn to_value(&self) -> Value {
        let mut fields = Map::new();
        if let Some(url) = &self.resource_metadata_url {
            fields.insert(
                "resourceMetadataUrl".to_owned(),
                Value::String(url.as_str().to_owned()),
            );
        }
        if let Some(scope) = &self.scope {
            fields.insert("scope".to_owned(), Value::String(scope.clone()));
        }
        if let Some(error) = &self.error {
            fields.insert("error".to_owned(), Value::String(error.clone()));
        }
        if let Some(description) = &self.error_description {
            fields.insert(
                "errorDescription".to_owned(),
                Value::String(description.clone()),
            );
        }
        Value::Object(fields)
    }
}

/// `parseProtectedResourceMetadata(value)`。
///
/// 求值顺序固定为：`object` → `resource` → `authorization_servers`（数组校验后逐项
/// [`safe_url`]）→ `scopes_supported`。错误文案逐字见 §18.1。
pub fn parse_protected_resource_metadata(
    value: &Value,
) -> Result<OAuthProtectedResourceMetadata, OAuthFailure> {
    // 阶段 1：输入必须是 JSON 对象；先复制成 JS 键序的副本，未知字段原样留在副本里。
    let input = object(value, "OAuth protected resource metadata")?;
    let mut fields = js_ordered(input);
    // 阶段 2：按 TS 的求值顺序校验，错误名逐字见 §18.1。resource 缺失/非法最先报；
    // 数组本身不是字符串数组时报 "authorization_servers"，数组元素非法时报
    // "authorization server URL"。空数组保留为 `[]`，与「缺省」不同。
    let resource = safe_url(
        input.get("resource"),
        "OAuth protected resource metadata resource",
    )?;
    let authorization_servers =
        match optional_strings(input.get("authorization_servers"), "authorization_servers")? {
            Some(urls) => Some(
                urls.iter()
                    .map(|url| {
                        safe_url(
                            Some(&Value::String(url.clone())),
                            "authorization server URL",
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
    let scopes_supported = optional_strings(input.get("scopes_supported"), "scopes_supported")?;
    // 阶段 3：把校验后的值写回副本（已存在的键保持原位，缺失的键追加到末尾），再 compact。
    set_field(&mut fields, "resource", Some(Value::String(resource)));
    set_field(
        &mut fields,
        "authorization_servers",
        authorization_servers
            .map(|urls| Value::Array(urls.into_iter().map(Value::String).collect())),
    );
    set_field(
        &mut fields,
        "scopes_supported",
        scopes_supported
            .map(|scopes| Value::Array(scopes.into_iter().map(Value::String).collect())),
    );
    Ok(OAuthProtectedResourceMetadata::from_fields(compact(fields)))
}

/// `parseAuthorizationServerMetadata(value)`。
///
/// `response_types_supported` 先于其它字段校验（`!responseTypes` → `Invalid
/// response_types_supported`）；布尔字段用 `typeof === "boolean"` 判定，非布尔 → 省略。
pub fn parse_authorization_server_metadata(
    value: &Value,
) -> Result<AuthorizationServerMetadata, OAuthFailure> {
    let input = object(value, "authorization server metadata")?;
    let mut fields = js_ordered(input);
    // TS 先求值 `responseTypes` 再校验其它字段：缺失、null 或 "" 都先报
    // `Invalid response_types_supported`（空对象也先报它，而不是 issuer，测试锁定）。
    let response_types = optional_strings(
        input.get("response_types_supported"),
        "response_types_supported",
    )?;
    let Some(response_types) = response_types else {
        return Err(OAuthFailure::error("Invalid response_types_supported"));
    };
    let issuer = safe_url(input.get("issuer"), "authorization server issuer")?;
    let authorization_endpoint = safe_url(
        input.get("authorization_endpoint"),
        "authorization endpoint",
    )?;
    let token_endpoint = safe_url(input.get("token_endpoint"), "token endpoint")?;
    let registration_endpoint =
        optional_url(input.get("registration_endpoint"), "registration endpoint")?;
    let scopes_supported = optional_strings(input.get("scopes_supported"), "scopes_supported")?;
    let grant_types_supported =
        optional_strings(input.get("grant_types_supported"), "grant_types_supported")?;
    let token_endpoint_auth_methods_supported = optional_strings(
        input.get("token_endpoint_auth_methods_supported"),
        "token_endpoint_auth_methods_supported",
    )?;
    let code_challenge_methods_supported = optional_strings(
        input.get("code_challenge_methods_supported"),
        "code_challenge_methods_supported",
    )?;
    // `typeof input.x === "boolean" ? input.x : undefined`：非布尔（含 null、数字、
    // 字符串）一律省略，不报错。
    let client_id_metadata_document_supported = input
        .get("client_id_metadata_document_supported")
        .and_then(Value::as_bool);
    let authorization_response_iss_parameter_supported = input
        .get("authorization_response_iss_parameter_supported")
        .and_then(Value::as_bool);

    // 全部字段校验通过后才写回：任何 `Err` 都在构造结果前返回，不会留下部分对象。
    let strings = |items: Vec<String>| Value::Array(items.into_iter().map(Value::String).collect());
    set_field(&mut fields, "issuer", Some(Value::String(issuer)));
    set_field(
        &mut fields,
        "authorization_endpoint",
        Some(Value::String(authorization_endpoint)),
    );
    set_field(
        &mut fields,
        "token_endpoint",
        Some(Value::String(token_endpoint)),
    );
    set_field(
        &mut fields,
        "registration_endpoint",
        registration_endpoint.map(Value::String),
    );
    set_field(
        &mut fields,
        "scopes_supported",
        scopes_supported.map(strings),
    );
    set_field(
        &mut fields,
        "response_types_supported",
        Some(strings(response_types)),
    );
    set_field(
        &mut fields,
        "grant_types_supported",
        grant_types_supported.map(strings),
    );
    set_field(
        &mut fields,
        "token_endpoint_auth_methods_supported",
        token_endpoint_auth_methods_supported.map(strings),
    );
    set_field(
        &mut fields,
        "code_challenge_methods_supported",
        code_challenge_methods_supported.map(strings),
    );
    set_field(
        &mut fields,
        "client_id_metadata_document_supported",
        client_id_metadata_document_supported.map(Value::Bool),
    );
    set_field(
        &mut fields,
        "authorization_response_iss_parameter_supported",
        authorization_response_iss_parameter_supported.map(Value::Bool),
    );
    Ok(AuthorizationServerMetadata::from_fields(compact(fields)))
}

/// `parseOAuthTokens(value)`。
///
/// `expires_in` 的 `Number()` 转换与有限性检查在 `access_token` 之前完成（TS 里
/// `const expires = ...` 先于 `compact({...})` 求值），所以 `{access_token:"a", expires_in:"x"}`
/// 报 `Invalid expires_in`。
pub fn parse_oauth_tokens(value: &Value) -> Result<OAuthTokens, OAuthFailure> {
    let input = object(value, "OAuth token response")?;
    // 注意：本函数**不**展开输入（`compact({access_token, …})` 没有 `...input`），所以未知字段
    // 会被丢弃、键序固定为字面量顺序 —— 与 `parseProtectedResourceMetadata` 等相反。
    let mut fields = Map::new();
    // 先算 expires：TS 里 `const expires = ...` 位于对象字面量之前，因此
    // `Invalid expires_in` 的优先级高于下面所有必填字段。`absent` 恰好覆盖 TS 的
    // `undefined`/`null`/`""` 三种缺省——注意这里**不能**套用 `Number(null) === 0` 的直觉。
    let expires = match input.get("expires_in") {
        Some(item) if !absent(Some(item)) => Some(js_number(item)),
        _ => None,
    };
    if let Some(expires) = expires
        && !expires.is_finite()
    {
        return Err(OAuthFailure::error("Invalid expires_in"));
    }
    let access_token = required_string(input.get("access_token"), "access_token")?;
    let token_type = required_string(input.get("token_type"), "token_type")?;
    let scope = optional_string(input.get("scope"), "scope")?;
    let refresh_token = optional_string(input.get("refresh_token"), "refresh_token")?;
    let id_token = optional_string(input.get("id_token"), "id_token")?;
    set_field(
        &mut fields,
        "access_token",
        Some(Value::String(access_token)),
    );
    set_field(&mut fields, "token_type", Some(Value::String(token_type)));
    set_field(&mut fields, "expires_in", expires.map(js_number_value));
    set_field(&mut fields, "scope", scope.map(Value::String));
    set_field(
        &mut fields,
        "refresh_token",
        refresh_token.map(Value::String),
    );
    set_field(&mut fields, "id_token", id_token.map(Value::String));
    Ok(OAuthTokens::from_fields(compact(fields)))
}

/// `parseClientInformation(value)`。
///
/// 先展开输入（客户端元数据字段保留），然后校验 `client_id`（必填）、`client_secret`
/// （可选）、两个时间戳（`typeof === "number"`，否则省略）、`redirect_uris`
/// （可选数组，缺省为 `[]`）。
pub fn parse_client_information(value: &Value) -> Result<OAuthClientInformationFull, OAuthFailure> {
    let input = object(value, "OAuth client registration response")?;
    let mut fields = js_ordered(input);
    // 与 `parseOAuthTokens` 不同：这里先展开输入（`{...input}`），客户端元数据与未知字段
    // 全部保留；下面写回已知字段时，已存在的键原位覆盖、缺失的键追加到末尾。
    let client_id = required_string(input.get("client_id"), "client_id")?;
    let client_secret = optional_string(input.get("client_secret"), "client_secret")?;
    // `typeof value === "number"`：只有 JSON 数字保留（包括 0 与负数）；数字字符串、
    // null、布尔都被删除（不是报错）。
    let client_id_issued_at = input
        .get("client_id_issued_at")
        .filter(|item| item.is_number())
        .map(|item| js_number_value(item.as_f64().unwrap_or(f64::NAN)));
    let client_secret_expires_at = input
        .get("client_secret_expires_at")
        .filter(|item| item.is_number())
        .map(|item| js_number_value(item.as_f64().unwrap_or(f64::NAN)));
    // 缺省或 null → `[]`；`""` 不是缺省，会报 `Invalid redirect_uris`。写回的是新数组：
    // 键已存在时原位替换（值可能从 null 变成 `[]`），不存在时追加到末尾。
    let redirect_uris =
        optional_strings(input.get("redirect_uris"), "redirect_uris")?.unwrap_or_default();
    set_field(&mut fields, "client_id", Some(Value::String(client_id)));
    set_field(
        &mut fields,
        "client_secret",
        client_secret.map(Value::String),
    );
    set_field(&mut fields, "client_id_issued_at", client_id_issued_at);
    set_field(
        &mut fields,
        "client_secret_expires_at",
        client_secret_expires_at,
    );
    set_field(
        &mut fields,
        "redirect_uris",
        Some(Value::Array(
            redirect_uris.into_iter().map(Value::String).collect(),
        )),
    );
    Ok(OAuthClientInformationFull::from_fields(compact(fields)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn error_message(result: Result<impl std::fmt::Debug, OAuthFailure>) -> String {
        match result {
            Ok(value) => panic!("expected an error, got {value:?}"),
            Err(error) => error.message(),
        }
    }

    #[test]
    fn protected_resource_metadata_keeps_unknown_fields_and_js_key_order() {
        let parsed = parse_protected_resource_metadata(&json!({
            "zzz": [1, 2],
            "resource": "https://a.example",
            "extra": {"keep": 1},
            "authorization_servers": ["https://as.example"],
            "scopes_supported": ["a", "b"],
        }))
        .unwrap();
        assert_eq!(parsed.resource(), Some("https://a.example"));
        assert_eq!(
            parsed.authorization_servers(),
            Some(vec!["https://as.example"])
        );
        assert_eq!(parsed.scopes_supported(), Some(vec!["a", "b"]));
        // 输入里已存在的键保持原位（`resource` 是输入里的第二个键）。
        let keys: Vec<&str> = parsed.fields().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "zzz",
                "resource",
                "extra",
                "authorization_servers",
                "scopes_supported"
            ]
        );
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"zzz\":[1,2],\"resource\":\"https://a.example\",\"extra\":{\"keep\":1},\
             \"authorization_servers\":[\"https://as.example\"],\"scopes_supported\":[\"a\",\"b\"]}"
        );
    }

    /// §18.1：`optionalStrings` 只把 `undefined`/`null` 当缺省，`""` 会抛错。
    #[test]
    fn empty_strings_throw_for_string_arrays_but_not_for_scalars() {
        assert_eq!(
            error_message(parse_protected_resource_metadata(&json!({
                "resource": "https://a.example",
                "scopes_supported": "",
            }))),
            "Invalid scopes_supported"
        );
        let parsed = parse_protected_resource_metadata(&json!({
            "resource": "https://a.example",
            "scopes_supported": [],
            "authorization_servers": null,
        }))
        .unwrap();
        assert_eq!(parsed.scopes_supported(), Some(Vec::new()));
        assert!(parsed.authorization_servers().is_none());
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"resource\":\"https://a.example\",\"scopes_supported\":[]}"
        );
    }

    #[test]
    fn protected_resource_metadata_rejects_non_http_and_malformed_urls() {
        for input in [
            json!({"resource": ""}),
            json!({"resource": null}),
            json!({"resource": 5}),
            json!({"resource": "file:///etc/passwd"}),
            json!({"resource": "not a url"}),
            json!({}),
        ] {
            assert_eq!(
                error_message(parse_protected_resource_metadata(&input)),
                "Invalid OAuth protected resource metadata resource"
            );
        }
        assert_eq!(
            error_message(parse_protected_resource_metadata(&json!({
                "resource": "https://a.example",
                "authorization_servers": ["file:///x"],
            }))),
            "Invalid authorization server URL"
        );
        assert_eq!(
            error_message(parse_protected_resource_metadata(&json!({
                "resource": "https://a.example",
                "authorization_servers": [5],
            }))),
            "Invalid authorization_servers"
        );
        for input in [json!([]), json!(null), json!("x")] {
            assert_eq!(
                error_message(parse_protected_resource_metadata(&input)),
                "Invalid OAuth protected resource metadata"
            );
        }
    }

    #[test]
    fn authorization_server_metadata_validates_in_ts_order() {
        let parsed = parse_authorization_server_metadata(&json!({
            "issuer": "https://i.example",
            "authorization_endpoint": "https://i.example/authorize",
            "token_endpoint": "https://i.example/token",
            "response_types_supported": ["code"],
            "registration_endpoint": "https://i.example/register",
            "scopes_supported": ["a"],
            "grant_types_supported": ["authorization_code"],
            "token_endpoint_auth_methods_supported": ["none"],
            "code_challenge_methods_supported": ["S256"],
            "client_id_metadata_document_supported": true,
            "authorization_response_iss_parameter_supported": false,
            "extra": 1,
        }))
        .unwrap();
        assert_eq!(parsed.issuer(), Some("https://i.example"));
        assert_eq!(
            parsed.registration_endpoint(),
            Some("https://i.example/register")
        );
        assert_eq!(parsed.response_types_supported(), Some(vec!["code"]));
        assert_eq!(parsed.token_endpoint_auth_methods_supported(), vec!["none"]);
        assert_eq!(parsed.client_id_metadata_document_supported(), Some(true));
        assert_eq!(
            parsed.authorization_response_iss_parameter_supported(),
            Some(false)
        );
        assert_eq!(parsed.get("extra"), Some(&json!(1)));

        // 非布尔 → 省略（不是报错）。
        let parsed = parse_authorization_server_metadata(&json!({
            "issuer": "https://i.example",
            "authorization_endpoint": "https://i.example/authorize",
            "token_endpoint": "https://i.example/token",
            "response_types_supported": ["code"],
            "client_id_metadata_document_supported": "yes",
            "authorization_response_iss_parameter_supported": 1,
        }))
        .unwrap();
        assert_eq!(parsed.client_id_metadata_document_supported(), None);
        assert_eq!(
            parsed.authorization_response_iss_parameter_supported(),
            None
        );
        assert!(
            parsed
                .get("client_id_metadata_document_supported")
                .is_none()
        );
    }

    #[test]
    fn authorization_server_metadata_requires_response_types() {
        for response_types in [None, Some(Value::Null)] {
            let mut input = json!({
                "issuer": "https://i.example",
                "authorization_endpoint": "https://i.example/authorize",
                "token_endpoint": "https://i.example/token",
            });
            if let Some(value) = response_types {
                input["response_types_supported"] = value;
            }
            assert_eq!(
                error_message(parse_authorization_server_metadata(&input)),
                "Invalid response_types_supported"
            );
        }
        assert_eq!(
            error_message(parse_authorization_server_metadata(&json!({
                "issuer": "https://i.example",
                "authorization_endpoint": "https://i.example/authorize",
                "token_endpoint": "https://i.example/token",
                "response_types_supported": "",
            }))),
            "Invalid response_types_supported"
        );
        // response_types_supported 先校验：缺 issuer 时仍先报它。
        assert_eq!(
            error_message(parse_authorization_server_metadata(&json!({}))),
            "Invalid response_types_supported"
        );
        assert_eq!(
            error_message(parse_authorization_server_metadata(&json!({
                "response_types_supported": ["code"],
            }))),
            "Invalid authorization server issuer"
        );
    }

    #[test]
    fn authorization_server_metadata_rejects_insecure_or_malformed_endpoints() {
        let base = json!({
            "issuer": "https://i.example",
            "authorization_endpoint": "https://i.example/authorize",
            "token_endpoint": "https://i.example/token",
            "response_types_supported": ["code"],
        });
        for endpoint in [
            "file://attacker.example/share/x.exe",
            "search-ms:query=x",
            "ms-msdt:/id",
        ] {
            let mut input = base.clone();
            input["authorization_endpoint"] = json!(endpoint);
            assert_eq!(
                error_message(parse_authorization_server_metadata(&input)),
                "Invalid authorization endpoint"
            );
        }
        let mut input = base.clone();
        input["issuer"] = json!("ftp://i.example");
        assert_eq!(
            error_message(parse_authorization_server_metadata(&input)),
            "Invalid authorization server issuer"
        );
        let mut input = base.clone();
        input["token_endpoint"] = json!("");
        assert_eq!(
            error_message(parse_authorization_server_metadata(&input)),
            "Invalid token endpoint"
        );
        let mut input = base.clone();
        input["registration_endpoint"] = json!("file:///x");
        assert_eq!(
            error_message(parse_authorization_server_metadata(&input)),
            "Invalid registration endpoint"
        );
        let mut input = base;
        input["registration_endpoint"] = Value::Null;
        let parsed = parse_authorization_server_metadata(&input).unwrap();
        assert!(parsed.get("registration_endpoint").is_none());
    }

    /// §18.1 的 `Number()` 向量（本机 Node 26 实测，见 tests/fixtures/oauth_types_golden.json）。
    #[test]
    fn oauth_tokens_convert_expires_in_with_js_number_semantics() {
        let cases = [
            (json!("0x10"), Some(16.0)),
            (json!("1e3"), Some(1000.0)),
            (json!([]), Some(0.0)),
            (json!(true), Some(1.0)),
            (json!(3600), Some(3600.0)),
            (json!(1.5), Some(1.5)),
            (json!("3600"), Some(3600.0)),
            (json!(null), None),
            (json!(""), None),
        ];
        for (expires_in, expected) in cases {
            let parsed = parse_oauth_tokens(&json!({
                "access_token": "a",
                "token_type": "Bearer",
                "expires_in": expires_in,
            }))
            .unwrap();
            assert_eq!(parsed.expires_in(), expected, "expires_in = {expires_in}");
        }
        for expires_in in [
            json!("abc"),
            json!([1, 2]),
            json!(json!({"a": 1})),
            json!("9".repeat(400)),
        ] {
            assert_eq!(
                error_message(parse_oauth_tokens(&json!({
                    "access_token": "a",
                    "token_type": "Bearer",
                    "expires_in": expires_in,
                }))),
                "Invalid expires_in",
                "expires_in = {expires_in}"
            );
        }
        // expires_in 的校验先于 access_token。
        assert_eq!(
            error_message(parse_oauth_tokens(&json!({"expires_in": "abc"}))),
            "Invalid expires_in"
        );
    }

    #[test]
    fn oauth_tokens_require_access_token_and_token_type() {
        assert_eq!(
            error_message(parse_oauth_tokens(&json!({}))),
            "Invalid access_token"
        );
        assert_eq!(
            error_message(parse_oauth_tokens(&json!({"access_token": ""}))),
            "Invalid access_token"
        );
        assert_eq!(
            error_message(parse_oauth_tokens(&json!({"access_token": "a"}))),
            "Invalid token_type"
        );
        assert_eq!(
            error_message(parse_oauth_tokens(&json!([]))),
            "Invalid OAuth token response"
        );
        // 标量字段的空串视为缺省。
        let parsed = parse_oauth_tokens(&json!({
            "access_token": "a",
            "token_type": "Bearer",
            "scope": "",
            "refresh_token": null,
        }))
        .unwrap();
        assert!(parsed.scope().is_none());
        assert!(parsed.refresh_token().is_none());
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"access_token\":\"a\",\"token_type\":\"Bearer\"}"
        );
        // 未知字段被丢弃（与其它 parse* 不同：TS 这里没有 `...input`），键序固定为字面量顺序。
        let parsed = parse_oauth_tokens(&json!({
            "id_token": "i",
            "extra": [1],
            "access_token": "a",
            "token_type": "Bearer",
        }))
        .unwrap();
        assert!(parsed.get("extra").is_none());
        assert_eq!(parsed.id_token(), Some("i"));
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"access_token\":\"a\",\"token_type\":\"Bearer\",\"id_token\":\"i\"}"
        );
    }

    #[test]
    fn client_information_fills_redirect_uris_and_keeps_metadata() {
        let parsed = parse_client_information(&json!({
            "client_id": "c",
            "client_name": "n",
            "grant_types": ["authorization_code"],
            "custom": true,
        }))
        .unwrap();
        assert_eq!(parsed.client_id(), Some("c"));
        assert_eq!(parsed.redirect_uris(), Some(Vec::new()));
        assert_eq!(parsed.get("custom"), Some(&json!(true)));
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"client_id\":\"c\",\"client_name\":\"n\",\"grant_types\":[\"authorization_code\"],\
             \"custom\":true,\"redirect_uris\":[]}"
        );

        // 空 secret / 非数字时间戳 → 省略；数字时间戳保留。
        let parsed = parse_client_information(&json!({
            "client_id": "c",
            "client_secret": "",
            "client_id_issued_at": "1",
            "client_secret_expires_at": 0,
            "redirect_uris": ["http://127.0.0.1/cb"],
        }))
        .unwrap();
        assert!(parsed.client_secret().is_none());
        assert!(parsed.client_id_issued_at().is_none());
        assert_eq!(parsed.client_secret_expires_at(), Some(0.0));
        assert_eq!(parsed.redirect_uris(), Some(vec!["http://127.0.0.1/cb"]));

        assert_eq!(
            error_message(parse_client_information(&json!({"client_id": ""}))),
            "Invalid client_id"
        );
        assert_eq!(
            error_message(parse_client_information(&json!(null))),
            "Invalid OAuth client registration response"
        );
        assert_eq!(
            error_message(parse_client_information(&json!({
                "client_id": "c",
                "redirect_uris": "",
            }))),
            "Invalid redirect_uris"
        );
        // `redirect_uris: null` → `[]`（不是抛错）。
        let parsed =
            parse_client_information(&json!({"client_id": "c", "redirect_uris": null})).unwrap();
        assert_eq!(parsed.redirect_uris(), Some(Vec::new()));
    }

    /// `ARCHITECTURE.md` §8 第 31/32 条的两处**被迫**偏差在本模块的落点。
    ///
    /// TS 的 `JSON.parse` 保留 `Infinity`（`1e400`）与孤立代理项（`"\ud800"`）；Rust 的
    /// `serde_json::Value` 表示不了它们，`kiana_json::js_parse::parse` 按架构决策分别落成
    /// `Value::Null` 与 U+FFFD。本模块因此与 TS 有**两处**可观察差异，都只在服务器返回
    /// 病态 JSON 时出现：
    ///
    /// 1. `expires_in: 1e400`：TS 走 `Number.isFinite` → `Error("Invalid expires_in")`；
    ///    Rust 看到 `null` → 按「缺省」丢弃该键（`parseOAuthTokens` 成功，无 `expires_in`）。
    /// 2. `client_id_issued_at: 1e400`：TS 的 `typeof === "number"` 为真 → 键保留、值为
    ///    `Infinity`（`JSON.stringify` 写出 `null`）；Rust 看到 `null` → 键被 `compact` 删除。
    ///
    /// 字符串里的孤立代理项在 TS 里原样往返为 `"\ud800"`，Rust 往返成 `"\uFFFD"`。
    /// 三者都无法在 `Value` 里补回来（要改 §4.9 的数字/字符串契约），见 `ARCHITECTURE.md`
    /// §8 第 31、32 条。
    #[test]
    fn non_finite_numbers_and_lone_surrogates_follow_the_forced_deviations() {
        // 前提：`js_parse` 的落点（kiana-json 的契约，不是本模块选的）。
        let non_finite = kiana_json::js_parse::parse(r#"{"expires_in":1e400}"#).unwrap();
        assert_eq!(non_finite["expires_in"], Value::Null);

        // 1. `expires_in: 1e400`：TS 抛 `Invalid expires_in`，Rust 当缺省丢弃。
        let source = r#"{"access_token":"a","token_type":"Bearer","expires_in":1e400}"#;
        let tokens = parse_oauth_tokens(&kiana_json::js_parse::parse(source).unwrap()).unwrap();
        assert_eq!(tokens.expires_in(), None);
        assert_eq!(
            kiana_json::js_object::stringify(&tokens.to_value()),
            "{\"access_token\":\"a\",\"token_type\":\"Bearer\"}"
        );

        // 2. `client_id_issued_at: 1e400`：TS 保留该键（值 `Infinity` → 序列化为 `null`），
        //    Rust 因为 `null` 不是 `number` 而删除。
        let source = r#"{"client_id":"c","client_id_issued_at":1e400}"#;
        let client =
            parse_client_information(&kiana_json::js_parse::parse(source).unwrap()).unwrap();
        assert!(client.get("client_id_issued_at").is_none());
        assert_eq!(
            kiana_json::js_object::stringify(&client.to_value()),
            "{\"client_id\":\"c\",\"redirect_uris\":[]}"
        );

        // 对照：正常的 `null` 在两边表现一致（`absent` / `typeof` 都不是 number）。
        let tokens = parse_oauth_tokens(&json!({
            "access_token": "a",
            "token_type": "Bearer",
            "expires_in": null,
        }))
        .unwrap();
        assert_eq!(tokens.expires_in(), None);

        // 3. 孤立代理项 → U+FFFD（TS 原样保留 `"\ud800"`）。
        let tokens = parse_oauth_tokens(&json!({
            "access_token": "a",
            "token_type": "Bearer",
            "scope": kiana_json::js_string::js_string(Some(
                &kiana_json::js_parse::parse("\"\\ud800\"").unwrap()
            )),
        }))
        .unwrap();
        assert_eq!(tokens.scope(), Some("\u{FFFD}"));
    }

    #[test]
    fn numbers_are_stored_with_js_semantics() {
        let parsed = parse_oauth_tokens(&json!({
            "access_token": "a",
            "token_type": "Bearer",
            "expires_in": 1234567890123456789u64,
        }))
        .unwrap();
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"access_token\":\"a\",\"token_type\":\"Bearer\",\"expires_in\":1234567890123456800}"
        );
        let parsed = parse_oauth_tokens(&json!({
            "access_token": "a",
            "token_type": "Bearer",
            "expires_in": "-0",
        }))
        .unwrap();
        assert_eq!(
            kiana_json::js_object::stringify(&parsed.to_value()),
            "{\"access_token\":\"a\",\"token_type\":\"Bearer\",\"expires_in\":0}"
        );
    }

    #[test]
    fn discovery_state_and_server_info_use_camel_case_keys() {
        let state = OAuthDiscoveryState {
            authorization_server_url: "https://as.example".into(),
            authorization_server_metadata: Some(AuthorizationServerMetadata::from_fields(
                [("issuer".to_owned(), json!("https://as.example"))]
                    .into_iter()
                    .collect(),
            )),
            resource_metadata: None,
            resource_metadata_url: Some("https://a.example/meta".into()),
        };
        assert_eq!(
            kiana_json::js_object::stringify(&serde_json::to_value(&state).unwrap()),
            "{\"authorizationServerUrl\":\"https://as.example\",\
             \"authorizationServerMetadata\":{\"issuer\":\"https://as.example\"},\
             \"resourceMetadataUrl\":\"https://a.example/meta\"}"
        );
        let info = OAuthServerInfo {
            authorization_server_url: "https://as.example".into(),
            authorization_server_metadata: None,
            resource_metadata: None,
        };
        assert_eq!(
            kiana_json::js_object::stringify(&serde_json::to_value(&info).unwrap()),
            "{\"authorizationServerUrl\":\"https://as.example\"}"
        );
        // 反序列化往返。
        let back: OAuthDiscoveryState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(back, state);
    }

    /// `parse_*` 的输入是任意 JSON：极深/极宽的输入不能 panic。
    #[test]
    fn parsers_handle_degenerate_inputs() {
        // 注意：`serde_json::Value` 的 `Drop` 是递归的（见 `kiana_json::js_object` 的模块文档），
        // 深度保持在测试线程的 2 MiB 栈以内。TS 侧对任意深度的数组都立刻抛错（`isObject`）。
        let mut deep = json!(1);
        for _ in 0..200 {
            deep = json!([deep]);
        }
        let error = parse_protected_resource_metadata(&deep).unwrap_err();
        assert_eq!(error.message(), "Invalid OAuth protected resource metadata");
        drop(deep);
        // `scopes_supported` 里的非字符串元素（含嵌套数组/对象）→ Invalid scopes_supported。
        for item in [json!([1]), json!([{}]), json!([null]), json!([true])] {
            let input = json!({
                "resource": "https://a.example",
                "scopes_supported": item,
            });
            assert_eq!(
                parse_protected_resource_metadata(&input)
                    .unwrap_err()
                    .message(),
                "Invalid scopes_supported"
            );
        }
        // 超长字符串数组原样保留（`[...value]` 是浅拷贝）。
        let scopes: Vec<String> = (0..1000).map(|index| format!("s{index}")).collect();
        let parsed = parse_protected_resource_metadata(&json!({
            "resource": "https://a.example",
            "scopes_supported": scopes,
        }))
        .unwrap();
        assert_eq!(parsed.scopes_supported().unwrap().len(), 1000);
    }

    #[test]
    fn challenge_serializes_like_json_stringify() {
        let challenge = OAuthChallenge {
            resource_metadata_url: Some(Url::parse("https://a.example/meta").unwrap()),
            scope: Some("repo".into()),
            error: None,
            error_description: None,
        };
        assert_eq!(
            kiana_json::js_object::stringify(&challenge.to_value()),
            "{\"resourceMetadataUrl\":\"https://a.example/meta\",\"scope\":\"repo\"}"
        );
        assert_eq!(
            kiana_json::js_object::stringify(&OAuthChallenge::default().to_value()),
            "{}"
        );
    }

    #[test]
    fn scalar_helpers_follow_the_ts_absence_rules() {
        assert!(absent(None));
        assert!(absent(Some(&Value::Null)));
        assert!(absent(Some(&json!(""))));
        assert!(!absent(Some(&json!(" "))));
        assert!(!absent(Some(&json!(0))));
        assert_eq!(optional_string(Some(&json!("")), "x").unwrap(), None);
        assert_eq!(
            optional_string(Some(&json!("v")), "x").unwrap(),
            Some("v".into())
        );
        assert_eq!(
            optional_string(Some(&json!(5)), "x").unwrap_err().message(),
            "Invalid x"
        );
        assert_eq!(
            optional_url(Some(&json!("https://a.example")), "x").unwrap(),
            Some("https://a.example".into())
        );
        assert_eq!(optional_url(Some(&json!("")), "x").unwrap(), None);
        assert_eq!(
            optional_url(Some(&json!("ftp://a")), "x")
                .unwrap_err()
                .message(),
            "Invalid x"
        );
        assert!(is_http_url(&Url::parse("http://a.example/").unwrap()));
        assert!(is_http_url(&Url::parse("https://a.example/").unwrap()));
        assert!(!is_http_url(&Url::parse("file:///x").unwrap()));
    }
}
