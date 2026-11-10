//! `kiana-ai` 集成测试的公共工具（`packages/ai/test/` 里那些辅助 `.ts` 的 Rust 等价物）。
//!
//! 只依赖 `kiana-ai` 自身、`tokio`、`futures` 与 `wiremock`（模块顶部的 import 即清单），
//! 不引入额外的 dev-dependency。每个测试二进制只用其中一部分，因此允许 `dead_code`。
//!
//! 对应关系：
//!
//! | 本模块 | TS 源 |
//! |---|---|
//! | [`event_stream`] | `test/event-stream.test.ts` 的 `message()`/`sleep()` 辅助 |
//! | [`messages`] | `test/message-types.test.ts`、`test/lax-message-content.test.ts` 的样本消息 |
//! | [`sse`] | 各 `stream.test.ts` 的 SSE 响应体构造 |
//! | [`wiremock_util`] | 各测试里的 `wiremock` 服务器启动/收尾 |
//! | [`credentials`] | `test/azure-utils.ts`、`test/bedrock-utils.ts`、`test/cloudflare-utils.ts` |
//! | [`memory`] | `test/memory-helpers.ts` |
//! | [`oauth`] | `test/oauth.ts`（从 `~/.kiana/agent/auth.json` 解析 API key） |

#![allow(dead_code)]

pub mod credentials;
pub mod event_stream;
pub mod memory;
pub mod messages;
pub mod oauth;
pub mod sse;
pub mod wiremock_util;
