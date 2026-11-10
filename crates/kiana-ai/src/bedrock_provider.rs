//! `bedrock-provider.ts`：Bedrock 适配器的静态模块注册入口。
//!
//! TS 源码只有一行：`export const bedrockProviderModule = { stream, streamSimple }`
//! （来自 `api/bedrock-converse-stream.ts`）。Bun 单文件构建无法打包可变 specifier 的
//! 动态 `import()`，因此由 `coding-agent/src/bun/runtime-setup.ts` 把这个静态模块交给
//! `setBedrockProviderModule()`（`api/bedrock-converse-stream.lazy.ts`），替代动态导入。
//!
//! Rust 是静态链接，本来就不需要这层转接；本模块保留同名入口以对齐 TS 的公开面：
//! [`bedrock_provider_module`] 返回一个只含 `stream`/`streamSimple` 的
//! [`ProviderStreams`] 实现，可直接传给
//! [`set_bedrock_provider_module`](crate::api::bedrock_converse_stream::set_bedrock_provider_module)。
//!
//! 模块级 `const` 在 Rust 里用 `LazyLock` 表达（首次调用时构造一次，与 TS 的单例一致）。
//!
//! # 一次典型调用
//!
//! 1. 上层取共享模块：[`bedrock_provider_module`]（每次返回同一个 `Arc`）。
//! 2. 传给
//!    [`set_bedrock_provider_module`](crate::api::bedrock_converse_stream::set_bedrock_provider_module)，
//!    覆盖 `bedrock_converse_stream_api()` 的默认返回值。
//! 3. 之后所有 `bedrock_converse_stream_api()` 调用都拿到这个模块，而不是新建默认适配器。
//!
//! 该步骤是可选的：不调用 `set_*` 时，工厂仍回落到内置的 `BedrockProvider`（见
//! `api/bedrock_converse_stream.rs` 的 `bedrock_converse_stream_api`）。当前仓库里没有
//! 生产代码调用 `set_bedrock_provider_module`（只有测试使用），因此默认行为就是“未接线”
//! 的等价路径；本模块只为对齐 TS 的公开面而保留。
//!
//! # 注意事项
//!
//! - `LazyLock` 保证只构造一次；返回值的 `Arc` 指针相等（测试用 `Arc::ptr_eq` 锁定）。
//! - 模块本身无状态，可跨线程共享；真正的流状态在每次调用新建的
//!   [`AssistantMessageEventStream`] 里。
//! - [`BedrockProviderModule::stream_simple`] 的同步错误会被映射成流上的 `error` 事件，
//!   而不是 panic：`ProviderStreams` 的冻结签名没有 `Result` 通道（见 `api/mod.rs` 的
//!   “适配器入口的同步错误契约”）。

use std::sync::{Arc, LazyLock};

use crate::api::bedrock_converse_stream::{
    bedrock_options_from_stream_options, stream, stream_simple,
};
use crate::api::lazy::lazy_stream_error;
use crate::event_stream::AssistantMessageEventStream;
use crate::models::ProviderStreams;
use crate::types::{Model, SimpleStreamOptions, StreamOptions, TranscriptContext};

/// `bedrockProviderModule`：只暴露 `stream` / `streamSimple` 的实现。
///
/// TS 的模块对象直接持有 `api/bedrock-converse-stream.ts` 的两个模块级函数；
/// Rust 侧用实现 [`ProviderStreams`] 的单位结构体复刻（trait 方法即这两个函数）。
pub struct BedrockProviderModule;

impl ProviderStreams for BedrockProviderModule {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: StreamOptions,
    ) -> AssistantMessageEventStream {
        // `api::bedrock_converse_stream::stream` 没有同步失败路径：请求在后台任务里执行，
        // 任何失败都由 `finish_stream_error` 转成流上的 `error` 事件，所以这里无需 `Result`。
        stream(model, context, bedrock_options_from_stream_options(options))
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        // TS 的 `streamSimple` 在缺少鉴权时同步抛错；`ProviderStreams` 的冻结签名同步返回流，
        // 与其它适配器一致地映射成流上的 `error` 事件（见 `api/mod.rs` 的契约说明）。
        //
        // 当前 `api::bedrock_converse_stream::stream_simple` 总是返回 `Ok`（其文档说明保留
        // `Result` 只为对齐适配器契约），因此 `Err` 分支眼下不可达；保留它是为了让本模块在
        // 该函数将来出现同步错误时仍满足“不 panic、错误只走事件流”的约定。
        match stream_simple(model, context, options) {
            Ok(stream) => stream,
            Err(error) => lazy_stream_error(model, error),
        }
    }
}

/// 模块加载时构造一次（TS 的 `const bedrockProviderModule`）。
static BEDROCK_PROVIDER_MODULE: LazyLock<Arc<dyn ProviderStreams>> =
    LazyLock::new(|| Arc::new(BedrockProviderModule));

/// `bedrockProviderModule`。
///
/// 每次返回同一个 `Arc`（`Arc::clone`，O(1)），调用方可以自由克隆并跨线程传递；模块本身
/// 无内部状态，所有状态都属于每次调用新建的事件流。
///
/// TS: `bedrock-provider.ts#bedrockProviderModule`
pub fn bedrock_provider_module() -> Arc<dyn ProviderStreams> {
    Arc::clone(&BEDROCK_PROVIDER_MODULE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Context, ModelInputModality};

    fn model() -> Model {
        Model {
            id: "amazon.nova-pro-v1:0".into(),
            name: "Nova Pro".into(),
            api: "bedrock-converse-stream".into(),
            provider: "amazon-bedrock".into(),
            base_url: "https://bedrock-runtime.us-east-1.amazonaws.com".into(),
            input: vec![ModelInputModality::Text],
            ..Model::default()
        }
    }

    #[tokio::test]
    async fn module_delegates_stream_simple_and_surfaces_errors() {
        let module = bedrock_provider_module();
        // 同一份静态模块（TS 的 `const` 语义）。
        assert!(Arc::ptr_eq(&module, &bedrock_provider_module()));

        let context = TranscriptContext::from_normalized(Context::default());
        let stream = module.stream_simple(&model(), &context, SimpleStreamOptions::default());
        let message = stream.result().await.expect("the adapter ends the stream");
        // 凭据/网络不可用时请求无法完成：流必须以 error 结束（TS 的同步抛错在这里成为
        // error 事件）。具体文本取决于凭据链与网络环境（缺少 key、连接失败、超时都可能），
        // 因此只断言可稳定观察的形状。
        assert_eq!(message.stop_reason, crate::types::StopReason::Error);
        assert!(message.error_message.is_some());
    }
}
