//! `compat/extension-oauth-types.ts`：旧扩展 OAuth 提示类型（`OAuthPrompt` / `OAuthAuthInfo` /
//! `OAuthDeviceCodeInfo` / `OAuthSelectOption` …）。
//!
//! 这些类型是**扩展作者可见的问题面**，字段名与可选性必须保持稳定；它们由
//! `kiana-ext-api` 的扩展 provider 注册类型（`kiana_ext_api::types::ProviderOAuthConfig`
//! 的 `login` 签名）使用，本模块只做定义与 re-export。
//!
//! TS 的 `OAuthLoginCallbacks` 是一个由宿主**构造**并传给扩展 `login(config)` 的回调对象
//! （`coding-agent/src/core/provider-composer.ts#adaptOAuth`），因此 Rust 侧用「必选回调 +
//! 可选回调字段」的结构体表示；`signal` 对应 `AbortSignal`。
//!
//! TS: `src/compat/extension-oauth-types.ts`
//!
//! ## 为什么单独一套类型
//!
//! 这些结构体不是 `crate::auth::types` 里 `AuthPrompt` / `AuthEvent` 的别名：后者是宿主内部的
//! 进程内表示（枚举、无 serde 派生），本模块则是**扩展作者看到的线格式**。字段名、可选性与
//! 缺省语义一旦改动就会破坏已发布的扩展，因此两套类型分开演化，由 `kiana-config` 的 provider
//! 合成层（`models/composer.rs` 的 `ExtensionOAuthCallbacks`，对应 TS `adaptOAuth`）在两者之间
//! 转换。
//!
//! ## 数据流（三个方向）
//!
//! 1. 宿主 → 扩展：`kiana-extensions` 的加载器把 `OAuthLoginCallbacks` 物化成 JS 对象
//!    （`oauth_callbacks_value`）。`onAuth`/`onDeviceCode`/`onPrompt`/`onSelect` 总是存在；
//!    `onProgress`/`onManualCodeInput` 仅在宿主提供时出现；`signal` 缺失时是 `undefined`。
//! 2. 扩展 → 宿主：扩展调用 `onPrompt(prompt)` / `onSelect(prompt)` 时，JS 对象先转 JSON，
//!    再用本模块的 `Deserialize` 解析成 `OAuthPrompt` / `OAuthSelectPrompt`。键名必须是
//!    camelCase；解析失败时加载器从回调函数里抛出 `invalid OAuthPrompt: …` /
//!    `invalid OAuthSelectPrompt: …`（是同步抛出，不是拒绝 Promise）。
//! 3. 宿主内部：`kiana-extensions` 的 runner 把宿主 `AuthInteraction` 包装成同一回调面
//!    （`extension_login_callbacks`），供 Rust 实现的 `login` 使用；`kiana-config` 再把回调
//!    映射回 `AuthEvent` / `AuthPrompt`。
//!
//! `kiana_ai::auth::oauth_compat` 与 crate 根的 `index` 只做类型再导出，本模块是其中 6 个类型的
//! 唯一定义处（`OAuthCredentials` 例外，定义在 `auth::types`）。
//!
//! ## 线格式约束（改字段前必读）
//!
//! - 全部结构体 `#[serde(rename_all = "camelCase")]`：Rust 字段 `allow_empty` 在 JSON 里是
//!   `allowEmpty`。这是扩展源码直接书写的键名，不能为了 Rust 风格改名。
//! - 可选字段是 `Option` + `skip_serializing_if = "Option::is_none"`：省略键表示 `undefined`，
//!   与 TS 可选属性一致；反序列化时缺键得到 `None`。
//! - 未启用 `deny_unknown_fields`：TS 是结构化类型，扩展多传的字段会被忽略而不是报错。
//! - 只有 `OAuthDeviceCodeInfo` 含 `f64`，因此只派生 `PartialEq`；其余结构体可派生 `Eq`。
//!   可选数字按 JS `JSON.stringify` 规则写出（`serialize_optional_js_number`），不是 serde 的
//!   默认 `5.0`。
//! - 回调本身不是数据：`OAuthLoginCallbacks` 没有 `Serialize`/`Deserialize`/`PartialEq`，
//!   只有 `Clone`（共享 `Arc`）与手写的 `Debug`（`on_progress`/`on_manual_code_input`
//!   打印是否存在，`signal` 打印其 `Debug` 值，即 `AbortScope` 的 token 与 reason）。
//!
//! ## 阅读顺序
//!
//! 先看 5 个提示/信息结构体与它们的 serde 属性，再看 `OAuthCallbackFuture` 与 6 个 handler
//! 别名，最后看 `OAuthLoginCallbacks` 及其手写的 `Clone`/`Debug`。文件末尾的测试锁定上述
//! 线格式，可作为行为规格阅读。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::auth::types::AuthError;
use crate::utils::abort::AbortScope;

/// `OAuthCredentials` 由 `auth/types.ts` 定义、经本模块再导出：TS 的 `oauth.ts` / `index.ts`
/// 也只做类型再导出，扩展看到的始终是 `auth/types.ts` 的形状。
// TS 的 `export type { OAuthCredentials }`（最终来自 `auth/types.ts`）。
pub use crate::auth::types::OAuthCredentials;

/// `#[serde(serialize_with = ...)]`：可选数字按 JS `JSON.stringify` 规则写出
/// （`5` 而不是 `5.0`；`NaN`/`Inf` → `null`）。
///
/// TS 的 `intervalSeconds`/`expiresInSeconds` 是 JS number：扩展/RPC 面序列化这些类型时，
/// `JSON.stringify(5)` 是 `5`，serde 的 `f64` 默认会写出 `5.0`。
fn serialize_optional_js_number<S>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::Serialize as _;
    match value {
        Some(value) => crate::auth::types::JsNumber(*value).serialize(serializer),
        None => serializer.serialize_none(),
    }
}

/// 旧扩展 OAuth 提示。
///
/// 可选字段为 `None` 时序列化会省略对应键；反序列化时缺键得到 `None`。
///
/// TS: `OAuthPrompt`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthPrompt {
    /// 提示文本。
    pub message: String,
    /// 占位符。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// 是否允许空输入。
    ///
    /// 只作为线格式字段保留：canonical 的文本提示（`AuthPrompt::Text`）没有等价字段，宿主
    /// 适配层（`kiana-config` 的 `ExtensionOAuthCallbacks::on_prompt`、`kiana-extensions`
    /// runner 的回调包装）都只转交 `message`/`placeholder`，因此该值目前不影响宿主是否接受
    /// 空输入。保留它是为了让扩展源码与 TS 类型逐字对齐。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_empty: Option<bool>,
}

/// 旧扩展 OAuth 授权链接。
///
/// 对应 TS `AuthEvent` 的 `auth_url` 分支：只能携带 `url` 与 `instructions` 两个字段
/// （spec/ai-auth.md §A.9），新增信息需要同时确认 TS 侧的形状。
///
/// TS: `OAuthAuthInfo`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthAuthInfo {
    /// URL。
    pub url: String,
    /// 操作说明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// 旧扩展 OAuth 设备码通知。
///
/// 对应 TS `AuthEvent` 的 `device_code` 分支。两个布尔字段在 TS 注释里默认 `true`，Rust 用
/// `Option` 保留“未指定”，是否采用默认值由宿主客户端决定。因含 `f64`，本结构只派生
/// `PartialEq`，不派生 `Eq`。
///
/// TS: `OAuthDeviceCodeInfo`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthDeviceCodeInfo {
    /// 用户码。
    pub user_code: String,
    /// 验证地址。
    pub verification_uri: String,
    /// 用户确认后尝试打开浏览器；默认 true，客户端可忽略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_browser_on_confirm: Option<bool>,
    /// 用户确认后复制 `userCode`（替换剪贴板）；默认 true，客户端可忽略。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_code_on_confirm: Option<bool>,
    /// 轮询间隔（秒）。
    ///
    /// JS number：序列化经 `serialize_optional_js_number` 按 JS 规则写成 `5` 而非 `5.0`
    /// （测试 `serializes_prompts_with_camel_case_and_omits_absent_options` 锁定）。`None`
    /// 表示扩展未给出该值；宿主 UI 只透传/展示，不据此轮询；实际轮询节奏由发起登录的流程
    /// 自行决定（内置设备码流程见 `auth/oauth/device_code.rs`，缺省 5s）。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number"
    )]
    pub interval_seconds: Option<f64>,
    /// 有效期（秒）。序列化规则同 `interval_seconds`。
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_js_number"
    )]
    pub expires_in_seconds: Option<f64>,
}

/// 扩展选择项。
///
/// `id` 是用户选中后 `onSelect` 解析出的字符串，`label` 只用于展示；两者都由扩展提供，
/// 宿主不校验 `id` 是否唯一。
///
/// TS: `OAuthSelectOption`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSelectOption {
    /// 选项 id；即 `onSelect` 的返回值。
    pub id: String,
    /// 显示文本。
    pub label: String,
}

/// 扩展选择提示。
///
/// `options` 的顺序就是展示顺序（`Vec` 保留 TS 数组顺序）；`onSelect` 返回选中项的 `id`，
/// `None`/`undefined` 表示取消。
///
/// TS: `OAuthSelectPrompt`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthSelectPrompt {
    /// 提示文本。
    pub message: String,
    /// 选项。
    pub options: Vec<OAuthSelectOption>,
}

/// 兼容回调返回的 future（`Promise<string>` / `Promise<string | undefined>`）。
///
/// `T` 由具体回调决定：文本输入与手动码是 `String`，选择是 `Option<String>`（`None` 对应
/// JS 的 `undefined`）。错误统一为 [`AuthError`]；在 `kiana-extensions` 的 JS 桥里，`Err`
/// 只会以 `error.message` 作为 Promise 的 rejection 文本交给扩展。
pub type OAuthCallbackFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, AuthError>> + Send + 'static>>;

/// `onAuth(info)`。
///
/// 同步通知（TS 返回 `void`），在调用线程上执行；实现应尽快返回，不要在 handler 里阻塞等待
/// 用户操作。
pub type OAuthAuthHandler = Arc<dyn Fn(OAuthAuthInfo) + Send + Sync>;
/// `onDeviceCode(info)`。
///
/// 同 [`OAuthAuthHandler`]：同步通知，通常发生在开始轮询设备码之前。
pub type OAuthDeviceCodeHandler = Arc<dyn Fn(OAuthDeviceCodeInfo) + Send + Sync>;
/// `onPrompt(prompt)`。
///
/// 每次文本提示调用一次，返回的 future 解析为用户输入；`Err` 让扩展侧 Promise 以
/// `error.message` reject。
pub type OAuthPromptHandler = Arc<dyn Fn(OAuthPrompt) -> OAuthCallbackFuture<String> + Send + Sync>;
/// `onProgress(message)`。
///
/// 同步通知，无返回值；`message` 是直接展示给用户的进度文案。
pub type OAuthProgressHandler = Arc<dyn Fn(String) + Send + Sync>;
/// `onManualCodeInput()`。
///
/// 没有参数：提示文本由宿主固定为 `Paste the authorization code`（无冒号、无 placeholder），
/// 是当前稳定契约（spec/ai-auth.md §A.9）。
pub type OAuthManualCodeInputHandler = Arc<dyn Fn() -> OAuthCallbackFuture<String> + Send + Sync>;
/// `onSelect(prompt)`（返回 `undefined` 表示取消）。
///
/// `Some(id)` 是选中项的 `id`；`None` 在 JS 侧解析为 `undefined`（加载器写
/// `HostValue::Undefined`），与 TS 的 `Promise<string | undefined>` 对齐。
pub type OAuthSelectHandler =
    Arc<dyn Fn(OAuthSelectPrompt) -> OAuthCallbackFuture<Option<String>> + Send + Sync>;

/// 仅为 coding-agent 扩展兼容保留的回调面。
///
/// TS 的 `signal?: AbortSignal` 在 Rust 侧是 [`AbortScope`]；`onProgress?` 与
/// `onManualCodeInput?` 是可选方法，对应 `Option` 字段。`onPrompt`/`onSelect` 的 Promise
/// 用 boxed future 表示（与 [`AuthError`] 对齐，避免在兼容层再引入一套错误类型）。
///
/// # 回调分工与约束
///
/// - 同步通知：`on_auth`、`on_device_code`、`on_progress`（TS 返回 `void`），在调用线程上
///   执行，应尽快返回；
/// - 异步提问：`on_prompt`、`on_select`、`on_manual_code_input`，返回
///   [`OAuthCallbackFuture`]，由登录流程等待；
/// - 取消：`signal` 作用于整个登录流程。TS 的旧扩展 `OAuthPrompt` 没有 per-prompt
///   `signal`，因此单次提示无法单独取消（canonical `AuthPrompt` 的 per-prompt signal 不会
///   穿过这一层）。
///
/// # 共享与调试
///
/// `Clone` 通过 `Arc::clone` 共享同一批 handler，而不是复制其捕获状态；`Debug` 中
/// `on_progress`/`on_manual_code_input` 只打印 `is_some()`，`signal` 打印其 `Debug` 值
/// （`AbortScope` 会输出 token 与 reason）；必选 handler 由 `finish_non_exhaustive` 省略。
/// 本结构不是数据：未派生 `Serialize`/`Deserialize`/`PartialEq`。
///
/// TS: `OAuthLoginCallbacks`
pub struct OAuthLoginCallbacks {
    /// `onAuth(info)`：授权链接。
    pub on_auth: OAuthAuthHandler,
    /// `onDeviceCode(info)`：设备码。
    pub on_device_code: OAuthDeviceCodeHandler,
    /// `onPrompt(prompt)`：文本输入。
    pub on_prompt: OAuthPromptHandler,
    /// `onProgress?(message)`：进度。
    pub on_progress: Option<OAuthProgressHandler>,
    /// `onManualCodeInput?()`：手动粘贴授权码。
    pub on_manual_code_input: Option<OAuthManualCodeInputHandler>,
    /// `onSelect(prompt)`：选择。
    pub on_select: OAuthSelectHandler,
    /// `signal?`：取消整个登录流程。
    ///
    /// 由宿主提供；加载器会把它转成 JS `AbortSignal`，`None` 时该键为 `undefined`。
    pub signal: Option<AbortScope>,
}

// 手写 `Clone` 显式表达共享语义；`derive(Clone)` 会得到同样的逐字段 `Arc` 克隆，效果相同。
// 逐个克隆（`Arc`/`Option`）让克隆出的回调面与原值共享同一批 handler（不复制其捕获状态），
// 因此两边看到的是同一次登录的同一组回调。
impl Clone for OAuthLoginCallbacks {
    fn clone(&self) -> Self {
        OAuthLoginCallbacks {
            on_auth: Arc::clone(&self.on_auth),
            on_device_code: Arc::clone(&self.on_device_code),
            on_prompt: Arc::clone(&self.on_prompt),
            on_progress: self.on_progress.clone(),
            on_manual_code_input: self.on_manual_code_input.clone(),
            on_select: Arc::clone(&self.on_select),
            signal: self.signal.clone(),
        }
    }
}

// 手写 `Debug`：闭包 trait object 不实现 `Debug`。不打印 handler 捕获的数据：
// `on_progress`/`on_manual_code_input` 只打印 `is_some()`；`signal` 打印其 `Debug` 值
// （`AbortScope` 会输出 token 与 reason）；`finish_non_exhaustive` 明确还有未展示的必选回调。
impl std::fmt::Debug for OAuthLoginCallbacks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthLoginCallbacks")
            .field("on_progress", &self.on_progress.is_some())
            .field("on_manual_code_input", &self.on_manual_code_input.is_some())
            .field("signal", &self.signal)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    // 这些测试锁定线格式：camelCase 键名、`None` 字段省略、可选数字写成 `5`/`600` 而不是
    // `5.0`/`600.0`。改 serde 属性前先对照这里。
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_prompts_with_camel_case_and_omits_absent_options() {
        let prompt = OAuthPrompt {
            message: "Enter code".into(),
            placeholder: Some("abc".into()),
            allow_empty: None,
        };
        assert_eq!(
            serde_json::to_value(&prompt).unwrap(),
            json!({ "message": "Enter code", "placeholder": "abc" })
        );

        let device = OAuthDeviceCodeInfo {
            user_code: "ABCD".into(),
            verification_uri: "https://example.com/device".into(),
            open_browser_on_confirm: Some(false),
            copy_code_on_confirm: None,
            interval_seconds: Some(5.0),
            expires_in_seconds: Some(600.0),
        };
        // JS `JSON.stringify` 写 `5` / `600`，不是 `5.0` / `600.0`。
        assert_eq!(
            serde_json::to_string(&device).unwrap(),
            r#"{"userCode":"ABCD","verificationUri":"https://example.com/device","openBrowserOnConfirm":false,"intervalSeconds":5,"expiresInSeconds":600}"#
        );
    }

    #[test]
    fn round_trips_all_legacy_prompt_types() {
        let select = OAuthSelectPrompt {
            message: "Choose".into(),
            options: vec![
                OAuthSelectOption {
                    id: "a".into(),
                    label: "A".into(),
                },
                OAuthSelectOption {
                    id: "b".into(),
                    label: "B".into(),
                },
            ],
        };
        let json = serde_json::to_string(&select).unwrap();
        assert_eq!(
            json,
            r#"{"message":"Choose","options":[{"id":"a","label":"A"},{"id":"b","label":"B"}]}"#
        );
        assert_eq!(
            serde_json::from_str::<OAuthSelectPrompt>(&json).unwrap(),
            select
        );

        let info = OAuthAuthInfo {
            url: "https://example.com".into(),
            instructions: None,
        };
        assert_eq!(
            serde_json::to_value(&info).unwrap(),
            json!({ "url": "https://example.com" })
        );
    }
}
