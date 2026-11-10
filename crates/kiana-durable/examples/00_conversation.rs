//! 移植自 `packages/durable/test/examples/00-conversation.ts`：Session 存储会话、转录条目、任务与文档。
//!
//! 与 TS 一样，所有写入都在 `commit()` 里完成：回调成功则一起保存，抛错则一起丢弃。

use std::sync::Arc;

use kiana_chord::context::{BACKGROUND_CONTEXT, Context};
use kiana_durable::session::{SessionImpl, SessionOptions};
use kiana_durable::storage::MemoryStorage;
use kiana_durable::{BoxError, ConversationOwnership, CreateConversationOptions};

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    // MemoryStorage 把一切放在内存里；其他后端会落到磁盘。
    let session = SessionImpl::new(Arc::new(MemoryStorage::new()), SessionOptions::default());

    // 每次调用都带一个上下文：BACKGROUND_CONTEXT 表示"永不取消"。
    let context: &dyn Context = &**BACKGROUND_CONTEXT;

    // "ownerless" 表示没有任务创建这个会话。
    let standalone = session
        .commit_with(
            |tx| {
                Box::pin(async move {
                    tx.create_conversation(&CreateConversationOptions {
                        ownership: ConversationOwnership::Ownerless,
                    })
                    .await
                })
            },
            context,
            Default::default(),
        )
        .await?;
    println!("standalone conversation: {standalone:?}");

    session.close(context).await;
    Ok(())
}
