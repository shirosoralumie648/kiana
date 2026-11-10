//! `invocationName` 去重与 `getCommand` 查询。
//!
//! TS: `core/extensions/runner.ts#resolveRegisteredCommands`、`getRegisteredCommands`、
//! `getCommandDiagnostics`、`getCommand`。
//!
//! 规范：`spec/ca-extensions.md` §2.3.9、§3.2、§8.6 第 7 条。
//!
//! 命令顺序 = 扩展加载顺序 × 每个扩展内 `commands` Map 的插入顺序。只要同名命令出现
//! **多于一次**，所有实例都带 `:n` 后缀（第 1 个是 `name:1`）；与其它名字撞车时向后找
//! 空位。`getCommand` 只按 `invocationName` 精确匹配。
//!
//! # 调用方与数据流
//!
//! [`ExtensionRunner::get_registered_commands`](super::ExtensionRunner::get_registered_commands)
//! 与 [`ExtensionRunner::get_command`](super::ExtensionRunner::get_command) 是本模块纯函数的
//! 门面对应物。门面在调用前会用 loader 的实时扩展对象刷新快照（工厂结束后再注册的命令
//! 也能看到），但本模块内部**不缓存**任何结果：每次调用都重新收集、重新编号。
//!
//! # `invocationName` 算法
//!
//! 1. 第一遍统计每个原始 `name` 出现的次数；
//! 2. 第二遍按输入顺序计算 occurrence（该名字第几次出现，从 1 开始）；
//! 3. 名字出现多于一次时候选名是 `name:occurrence`，否则是原始名；
//! 4. 候选名已被之前的命令占用时（例如另一条命令的字面量名字恰好是 `x:1`），从
//!    `occurrence + 1` 开始线性向后找第一个空位。
//!
//! “占用”集合跨所有原始名字共享，因此不同名字之间也不会得到相同的 `invocationName`。
//! 冲突不会产生诊断——`commandDiagnostics` 始终为空；算法细节由
//! `tests/extension_commands.rs` 的 TS 黄金夹具锁定。
//!
//! # 边界
//!
//! 输入为空时返回空列表；`get_command` 对空扩展集合返回 `None`。所有函数都不执行 handler，
//! 只克隆 `Arc` 句柄与元数据；扩展集合含非 `Send` 的 handler，调用必须发生在持有它的线程上。

use std::collections::{HashMap, HashSet};

use kiana_core::diagnostics::ResourceDiagnostic;
use kiana_ext_api::{Extension, RegisteredCommand, ResolvedCommand};

/// 按扩展加载顺序收集全部已注册命令（不去重、不改名）。
///
/// 对应 TS 的 `for (const ext of this.extensions) for (const command of
/// ext.commands.values()) commands.push(command)`。
pub fn collect_registered_commands(extensions: &[Extension]) -> Vec<RegisteredCommand> {
    extensions
        .iter()
        .flat_map(|ext| ext.commands.values().cloned())
        .collect()
}

/// 对已收集的命令计算 `invocationName`。
///
/// TS: `runner.ts#resolveRegisteredCommands`。`RegisteredCommand` 的其余字段原样复制。
///
/// 输入顺序（扩展加载顺序 × 扩展内插入顺序）就是输出顺序；返回值与输入等长。函数是纯的：
/// 不修改输入、不执行 handler。编号规则与撞名回退见模块文档；输出的
/// [`ResolvedCommand::invocation_name`] 两两不同。
pub fn resolve_commands(commands: &[RegisteredCommand]) -> Vec<ResolvedCommand> {
    // 第一遍：原始名字的总出现次数，决定是否所有实例都要加 `:n` 后缀。
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for command in commands {
        *counts.entry(command.name.as_str()).or_insert(0) += 1;
    }

    // 第二遍：`seen` 记录每个名字已经处理到第几次；`taken_invocation_names` 记录所有已
    // 分配出去的调用名（跨名字共享），用于撞车回退。
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut taken_invocation_names: HashSet<String> = HashSet::new();
    let mut resolved = Vec::with_capacity(commands.len());

    for command in commands {
        let occurrence = seen.get(command.name.as_str()).copied().unwrap_or(0) + 1;
        seen.insert(command.name.as_str(), occurrence);

        let mut invocation_name = if counts.get(command.name.as_str()).copied().unwrap_or(0) > 1 {
            format!("{}:{occurrence}", command.name)
        } else {
            command.name.clone()
        };

        // 候选名可能被“字面量命令名”或更早的重复项占用：从 occurrence + 1 起线性探测。
        // 每个后缀只依赖已分配集合，因此分配结果是确定的。
        if taken_invocation_names.contains(&invocation_name) {
            let mut suffix = occurrence;
            loop {
                suffix += 1;
                invocation_name = format!("{}:{suffix}", command.name);
                if !taken_invocation_names.contains(&invocation_name) {
                    break;
                }
            }
        }

        taken_invocation_names.insert(invocation_name.clone());
        resolved.push(ResolvedCommand {
            name: command.name.clone(),
            source_info: command.source_info.clone(),
            description: command.description.clone(),
            get_argument_completions: command.get_argument_completions.clone(),
            handler: command.handler.clone(),
            invocation_name,
        });
    }

    resolved
}

/// 收集并解析全部扩展命令。TS: `runner.ts#getRegisteredCommands`。
///
/// 等价于 [`resolve_commands`] ∘ [`collect_registered_commands`]；门面每次调用都会重新执行
/// 这两步，不保留上一次的结果。
pub fn resolve_registered_commands(extensions: &[Extension]) -> Vec<ResolvedCommand> {
    resolve_commands(&collect_registered_commands(extensions))
}

/// 按 `invocationName` 精确查找命令。TS: `runner.ts#getCommand`。
///
/// 注意：只匹配 [`ResolvedCommand::invocation_name`]，不匹配注册名。同名命令出现多次时
/// `get_command(extensions, "name")` 返回 `None`，应使用 `"name:1"` 等。
///
/// 实现上每次都会完整解析一遍命令表再线性查找；调用方若要在同一快照上查多个名字，应自行
/// 复用 [`resolve_registered_commands`] 的结果，而不是重复调用本函数。
pub fn get_command(extensions: &[Extension], name: &str) -> Option<ResolvedCommand> {
    resolve_registered_commands(extensions)
        .into_iter()
        .find(|command| command.invocation_name == name)
}

/// `getCommandDiagnostics()`：恒为空。
///
/// TS 的 `getRegisteredCommands()` 每次先把 `commandDiagnostics` 重置为 `[]`，当前实现
/// 没有任何地方 push（`spec/ca-extensions.md` §B.8.6）。这里如实返回空列表；保留函数是
/// 为了让 runner-core 的门面与 TS 的公开方法一一对应。
pub fn get_command_diagnostics() -> Vec<ResourceDiagnostic> {
    Vec::new()
}
