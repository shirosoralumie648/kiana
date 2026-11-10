//! `RuntimeCredentials`（非持久化的运行时 API key 覆盖层）。
//!
//! TS: `core/runtime-credentials.ts`
//!
//! 覆盖项按 provider 保存于 [`IndexMap`]（`Map.set` 语义：已存在的键保持位置，新键追加）。
//! `modify` **不**触碰覆盖层（登录后运行时 key 仍优先是刻意行为）；`delete` 在持久层删除
//! 成功后清除覆盖项。
//!
//! # 为什么需要
//!
//! 运行时 API key 由 `ModelRuntime::set_runtime_api_key` 在进程内注入，必须立即对所有
//! 读取路径生效，但**不能**写进 `auth.json`——进程退出即失效是刻意的。因此这里用装饰器
//! 包住任意 [`CredentialStore`]：读路径先看覆盖层，未覆盖才落到持久层。
//!
//! # 读写语义（调用方需要知道的）
//!
//! - `read`：覆盖层存在且值非空 → 直接合成一条 `Credential::ApiKey`（只带 `key`，`env`
//!   为空、`extra` 为空），不读取持久层；否则连同 options 一起转交持久层。
//! - `list`：先取持久层条目，再把覆盖层的每个 provider 覆盖成 `AuthType::ApiKey`
//!   （已存在的 provider 保持原位置，新 provider 按覆盖层的插入顺序追加）；结果只含
//!   provider id 与类型，不含 key。
//! - `modify`：完全转交持久层，不读写覆盖层——登录/刷新得到的持久凭证仍被运行时 key
//!   遮蔽（刻意的优先级）。
//! - `delete`：先删持久层，成功后才移除覆盖项；持久层删除失败（含中止）时覆盖项保留。
//!
//! # 并发与取消
//!
//! 覆盖层由 `std::sync::Mutex<IndexMap<...>>` 保护：`read`/`set`/`remove`/
//! `has_runtime_api_key` 的临界区只做一次 IndexMap 操作，`list` 的合并循环在遍历覆盖层键
//! 的整个期间持锁（循环体不含 await）；所有临界区都在 await 之前结束（`read` 先克隆出 key
//! 再等待持久层）。`AuthOperationOptions::signal` 是协作式
//! 取消：`read`/`delete` 在进入时检查，`list` 在持久层返回后、合并覆盖层前检查，`modify`
//! 不自己检查而是把 options 原样转交。`list` 的 `options.clone()` 共享同一个取消状态
//! （`AbortScope` 克隆是浅克隆），测试 `forwards_operation_signals_to_the_persistent_store`
//! 验证调用方稍后 abort 时持久层收到的 token 也会被取消。
//!
//! # 注意
//!
//! 覆盖项是明文 API key，只存在进程内存；本类型的 `Debug` 会打印覆盖层内容（见 `Debug`
//! 实现上方的注释），不要把它的 Debug 输出放进外部日志。

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use indexmap::IndexMap;
use kiana_ai::auth::types::{
    ApiKeyCredential, AuthError, AuthOperationOptions, AuthType, Credential, CredentialInfo,
    CredentialModifyFn, CredentialStore, throw_if_aborted,
};

/// 取 `std::sync::Mutex` 锁；中毒时取回内部值继续。
///
/// 覆盖层是可在下一次 `set_runtime_api_key` 重建的进程内状态，单次临界区只有一次表操作；因一次
/// panic 就永久拒绝访问会让之后所有凭证读取失败，所以忽略中毒标志。
fn lock_state<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Async credential store overlay for non-persistent runtime API keys。
///
/// TS: `runtime-credentials.ts#RuntimeCredentials`
///
/// 装饰器：`store` 是所有未被覆盖 provider 的真实来源，`overrides` 只保存进程内的
/// API key 覆盖项（provider id → key）。类型可通过 `Arc` 共享；所有方法都取 `&self`，
/// 覆盖层由互斥锁保护，因此可以从多个任务并发调用。覆盖层不落盘，`Debug` 会包含明文
/// key。
pub struct RuntimeCredentials {
    store: Arc<dyn CredentialStore>,
    overrides: Mutex<IndexMap<String, String>>,
}

// 注意：Debug 输出包含覆盖层的键与值（即明文 API key）；`finish_non_exhaustive` 只隐藏
// 未列出的字段，不会隐藏已列出的 overrides。不要整体打印到外部日志/错误报告。
impl std::fmt::Debug for RuntimeCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeCredentials")
            .field("overrides", &*lock_state(&self.overrides))
            .finish_non_exhaustive()
    }
}

impl RuntimeCredentials {
    /// `new RuntimeCredentials(store)`。
    ///
    /// `store` 是持久层（通常是 `AuthStorage`），本类型不感知它的具体实现：未覆盖的读写
    /// 连同取消信号与错误都原样转交。
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self {
            store,
            overrides: Mutex::new(IndexMap::new()),
        }
    }

    /// `setRuntimeApiKey(providerId, apiKey)`。
    ///
    /// 只写内存，不持久化、不会失败；同名覆盖项替换值但保持它在 `IndexMap` 中的原位置，
    /// 新 provider 追加到末尾。
    ///
    /// 边界：空串也会被存下——`read` 把它当“未覆盖”（TS 的 falsy 语义）回落持久层，但
    /// `has_runtime_api_key` 与 `list` 仍把该 provider 视为已覆盖。要撤销覆盖请用
    /// [`RuntimeCredentials::remove_runtime_api_key`]。
    pub fn set_runtime_api_key(&self, provider_id: &str, api_key: &str) {
        lock_state(&self.overrides).insert(provider_id.to_string(), api_key.to_string());
    }

    /// `removeRuntimeApiKey(providerId)`。
    ///
    /// 只移除内存覆盖项，不触碰持久层；provider 不在覆盖层时静默成功（幂等）。
    pub fn remove_runtime_api_key(&self, provider_id: &str) {
        lock_state(&self.overrides).shift_remove(provider_id);
    }

    /// `hasRuntimeApiKey(providerId)`。
    ///
    /// 只看覆盖层是否有这个键：不读持久层，也不过滤空串（见
    /// [`RuntimeCredentials::set_runtime_api_key`]）。`ModelRuntime::get_provider_auth_status`
    /// 用它把来源判定为 `Runtime`。
    pub fn has_runtime_api_key(&self, provider_id: &str) -> bool {
        lock_state(&self.overrides).contains_key(provider_id)
    }
}

#[async_trait]
impl CredentialStore for RuntimeCredentials {
    async fn read(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        // 中止检查在覆盖层查找之前：已中止的调用不应因为命中覆盖层而“成功”。
        if let Some(signal) = options.signal.as_ref() {
            throw_if_aborted(signal)?;
        }
        // TS: `const override = this.overrides.get(id); return override ? ... : ...`。
        // 空串是 falsy，因此落到持久层。
        // 先克隆出 key 再 await：锁不会跨越持久层调用。
        let override_key = lock_state(&self.overrides)
            .get(provider_id)
            .filter(|key| !key.is_empty())
            .cloned();
        match override_key {
            // 合成凭证只带 key：运行时覆盖没有 env/额外字段可复制。
            Some(key) => Ok(Some(Credential::ApiKey(ApiKeyCredential {
                key: Some(key),
                env: None,
                extra: IndexMap::new(),
            }))),
            None => self.store.read(provider_id, options).await,
        }
    }

    async fn list(&self, options: AuthOperationOptions) -> Result<Vec<CredentialInfo>, AuthError> {
        // 先完整取回持久层列表；覆盖层只在合并阶段替换元数据，不改变持久层原有顺序。
        let mut entries: IndexMap<String, CredentialInfo> = self
            .store
            .list(options.clone())
            .await?
            .into_iter()
            .map(|entry| (entry.provider_id.clone(), entry))
            .collect();
        // 持久层返回后、合并之前再检查一次中止：中止时整体返回错误，而不是给出部分合并
        // 的结果。
        if let Some(signal) = options.signal.as_ref() {
            throw_if_aborted(signal)?;
        }
        for provider_id in lock_state(&self.overrides).keys() {
            // `Map.set` 语义：已存在的 provider 保持原位置，只替换值；新 provider 追加。
            entries.insert(
                provider_id.clone(),
                CredentialInfo {
                    provider_id: provider_id.clone(),
                    r#type: AuthType::ApiKey,
                },
            );
        }
        Ok(entries.into_values().collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: CredentialModifyFn,
        options: AuthOperationOptions,
    ) -> Result<Option<Credential>, AuthError> {
        // 不触碰覆盖层（登录/刷新后的持久凭证不覆盖运行时 key）。
        // 取消检查交给持久层：trait 契约要求排队中的调用在拿到串行门后、执行 `f` 前再
        // 检查一次中止。
        self.store.modify(provider_id, f, options).await
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: AuthOperationOptions,
    ) -> Result<(), AuthError> {
        if let Some(signal) = options.signal.as_ref() {
            throw_if_aborted(signal)?;
        }
        // 顺序固定：先删持久层，成功后才移除覆盖项。持久层失败（含中止）时覆盖项保留，
        // 运行时 key 继续生效（测试
        // keeps_a_runtime_override_when_persistent_deletion_is_cancelled）。
        self.store.delete(provider_id, options).await?;
        lock_state(&self.overrides).shift_remove(provider_id);
        Ok(())
    }
}
