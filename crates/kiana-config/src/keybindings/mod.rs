//! `core/keybindings.ts`（359 行）与 `core/keybindings-schema.ts`（21 行）。
//!
//! TS 源：
//! - `packages/coding-agent/src/core/keybindings.ts`
//! - `packages/coding-agent/src/core/keybindings-schema.ts`
//! - `packages/coding-agent/src/core/key-id-schema.ts`（校验与 schema 值在 [`key_id`]）
//!
//! 规范：`spec/ca-config-models.md` §2.25–2.27、§3.8、§4.7、§4.8、§8.3 第 7 条。
//!
//! ## 结构
//!
//! - `KEYBINDING_NAME_MIGRATIONS`：59 个旧键名 → 命名空间 id（§2.27.4）。
//! - [`keybindings`] / [`app_keybindings`]：TS `KEYBINDINGS`（47 个 TUI id + 4 个平台覆盖 +
//!   43 个 `app.*`，共 90）与未导出的 `APP_KEYBINDINGS`（43）。二者在首次访问时按
//!   `process.platform` / `process.env` 求值一次（`LazyLock`，对应 TS 的模块级 `const`）。
//! - [`migrate_keybindings_config`] / `order_keybindings_config`：纯函数（case 1–2 直接测它们，
//!   **不写文件**；真正的 `keybindings.json` 重写在 `ca-cli` 的 `runMigrations`）。
//! - [`KeybindingsManager`]：**包裹**（而非继承）`kiana_tui::keybindings::KeybindingsManager`，
//!   追加 `create` / `reload` / `get_effective_config` 与 `keybindings.json` 读取。
//!
//! ## 与 TS 的已知偏差
//!
//! - `KeybindingsManager` 在 Rust 没有子类化，改为组合；委托方法与
//!   `kiana_tui::keybindings::KeybindingsManager` 同名同义。
//! - [`keybindings_for`] 是 TS 没有的纯构建器：把平台/环境显式传入（TS 的
//!   `process.platform` / `process.env` 无法在测试中替换）。[`keybindings`] 用它按真实
//!   进程环境求值一次。
//!
//! ## 显式复刻的 JS 运行时语义
//!
//! - `isLegacyKeybindingName` 的 `in` 还会命中 `Object.prototype` 的成员（迁移表是对象
//!   字面量）。Rust 无原型链，用 `OBJECT_PROTOTYPE_KEY_TARGETS` 逐字复刻：
//!   `KEYBINDING_NAME_MIGRATIONS[key]` 取出函数/原型对象后按 `ToPropertyKey` 转成字符串
//!   （函数 → 原生源码文本，`__proto__` → `"[object Object]"`）。spec §2.27.6 的注允许
//!   只按显式表实现，但 TS 源会真的改写这些键，故按 TS 行为实现（TS 源优先）。
//! - JS 对象的自有键枚举序是「整数索引键（升序）→ 其余键（插入序）」。
//!   [`migrate_keybindings_config`] 返回的 `IndexMap` 因此显式重排；TS 返回普通对象，
//!   `Object.keys` / `Object.entries` 会自动做同样的提升。
//!
//! ## 设计原因与模块边界
//!
//! `kiana-tui` 的 `tui_keybindings()` 只提供 47 个 `tui.*` id，也不读取 `keybindings.json`。
//! 应用层还需要 43 个 `app.*` id、旧配置的改名兼容与「改文件后重新加载」，本模块因此做三件事：
//!
//! 1. 组合默认表：`build_keybindings` 以 TUI 表为基底，在原位置覆盖 4 个平台相关的 TUI
//!    默认键，再追加 43 个 `app.*`，得到 TS 的 `KEYBINDINGS`（共 90 项）。顺序固定为
//!    「TUI 47 项原序 → app 43 项原序」，测试
//!    `keybindings_is_tui_defaults_then_overrides_then_app_keybindings` 逐项锁定。
//! 2. 解析用户配置：读取 `<agentDir>/keybindings.json`，经 [`migrate_keybindings_config`]
//!    改名与重排，再逐值校验，得到 [`KeybindingsConfig`]。
//! 3. 包装运行时管理器：[`KeybindingsManager`] 持有 `kiana_tui` 的管理器，把匹配、解析与
//!    冲突检测委托给它，自己只负责「默认表 + 文件路径 + 重载」。
//!
//! 平台判定与默认表构造在**首次访问时求值一次**（[`keybindings`] / [`app_keybindings`]
//! 内部的 `LazyLock`），之后修改环境变量不会重建默认表；需要指定平台/环境时用
//! [`keybindings_for`] / [`use_windows_keybindings`] 显式传参。
//!
//! ## 一次 [`KeybindingsManager::create`] 的执行顺序
//!
//! 1. 把 `agentDir` 与 `keybindings.json` 拼接成配置路径；`agentDir` 的来源见
//!    [`KeybindingsManager::create_default`]；
//! 2. `load_raw_config`：文件不存在、读取失败、JSON 解析失败或顶层不是对象/数组时静默
//!    得到 `None`，等价于「没有任何用户绑定」；顶层数组会被当作键为 `"0"`、`"1"`… 的对象；
//! 3. [`migrate_keybindings_config`]：旧键名改成命名空间 id；旧名与新名同时存在时丢弃旧名；
//!    输出按 `KEYBINDINGS` 顺序（其余键按 UTF-16 码元序）重排；
//! 4. `to_keybindings_config`：跳过 `$schema`，逐值校验是否为合法 `KeyId`/`KeyId[]`，
//!    不合法项静默丢弃；
//! 5. [`KeybindingsManager::new`]：把用户配置的键序归一成 JS 对象枚举序，以
//!    [`keybindings`] 的副本为默认表构造 `kiana_tui` 管理器；该构造器内部完成一次全量解析，
//!    所以返回时 `get_resolved_bindings()` 已可用。
//!
//! 整条链路**只读** `keybindings.json`。真正的文件改写属于迁移流程：
//! `kiana-coding-agent` 的 `migrations::migrate_keybindings_config_file` 会复用
//! [`migrate_keybindings_config`] 并在 `migrated == true` 时写回。
//!
//! ## 注意事项
//!
//! - 本模块的读取路径用 `Option` / 空配置表达失败，不产生错误值、也不打日志；配置文件
//!   损坏的最终表现是「用户绑定为空、所有键位回到默认值」。
//! - [`keybindings`] / [`app_keybindings`] 返回 `&'static` 表，修改前必须先 `clone()`。
//! - 键序是行为的一部分：默认表顺序决定 `KeybindingsSchema` 的 `properties` 顺序与
//!   `orderKeybindingsConfig` 的输出顺序，测试会逐项比较，不能按字母排序。
//! - [`KeybindingsManager`] 没有内部共享状态，`reload` 需要 `&mut self`；跨组件共享时由
//!   调用方自行持有 `Arc` 等包装。

pub mod key_id;
pub mod schema;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use indexmap::IndexMap;
use kiana_core::config::get_agent_dir;
use kiana_core::text::strip_bom;
use serde_json::Value;

pub use kiana_tui::keybindings::{
    KeyIdList, Keybinding, KeybindingConflict, KeybindingDefinition, KeybindingDefinitions,
    KeybindingsConfig,
};
pub use kiana_tui::keys::KeyId;

/// TS `AppKeybinding = keyof typeof APP_KEYBINDINGS`。
///
/// 取值是键位 id 字符串（如 `"app.interrupt"`）。Rust 侧只是 `&'static str` 别名，
/// 编译期**不**限制它必须是 43 个 `app.*` 之一：未知 id 传给
/// [`KeybindingsManager::matches`] 恒为 `false`，传给
/// [`KeybindingsManager::get_keys`] 得到空列表。`kiana-interactive` 的
/// `CustomEditor::set_action_handler` 用它登记应用动作名，按键时再用
/// `matches(data, action)` 查表。
pub type AppKeybinding = &'static str;

/// TS `AppKeybindings = { [K in AppKeybinding]: true }`。
///
/// 纯类型层映射，运行时没有构造点；Rust 侧保留名字以免导出面缺失。TS 里值恒为 `true`，
/// 这里的 `bool` 也没有读取点——实际配置一律用 [`KeybindingsConfig`]。
pub type AppKeybindings = IndexMap<AppKeybinding, bool>;

/// `NodeJS.ProcessEnv` 的等价物（只关心字符串值）。
///
/// `None` 表示读取真实进程环境（与 TS 的默认参数一致）；空字符串视为 falsy。
/// 本模块只查询 `WSL_DISTRO_NAME` 与 `WSL_INTEROP` 两个变量，其余变量不参与判定。
pub type KeybindingEnv = IndexMap<String, String>;

// =============================================================================
// 平台判定
// =============================================================================

/// `useWindowsKeybindings(platform = process.platform, env = process.env)`。
///
/// `platform === "win32" || (platform === "linux" && Boolean(env.WSL_DISTRO_NAME || env.WSL_INTEROP))`。
/// 注意 `WT_SESSION` 不参与判定（Windows Terminal 也可能跑在非 WSL 的 Linux 上）。
///
/// - `platform = None` 取编译期平台（见 `process_platform`），`env = None` 查真实进程环境；
/// - `win32` 直接返回 `true`；只有 `platform == "linux"` 才会检查 WSL 变量；其余平台
///   （含 `"darwin"`）不检查 WSL 变量并返回 `false`；
/// - 变量「存在且非空」才算命中（`Boolean("")` 为 `false` 的对应语义）；
/// - 非 Unicode 的环境变量值在 `std::env::var` 下取不到，等同未设置。
pub fn use_windows_keybindings(platform: Option<&str>, env: Option<&KeybindingEnv>) -> bool {
    let platform: &str = match platform {
        Some(platform) => platform,
        None => process_platform(),
    };
    if platform == "win32" {
        return true;
    }
    // 只有 Linux 才可能通过 WSL 环境变量命中；darwin 与其它 Unix 在这里直接为 false。
    if platform != "linux" {
        return false;
    }
    let env_truthy = |name: &str| env_var(env, name).is_some_and(|value| !value.is_empty());
    env_truthy("WSL_DISTRO_NAME") || env_truthy("WSL_INTEROP")
}

/// 读取环境变量：`Some(map)` 优先，`None` 回落到 `std::env`。
///
/// 总是返回拷贝；调用方修改结果不会影响传入的 map。`std::env::var` 遇到非 Unicode
/// 值会返回 `Err`，这里按「未设置」处理。
fn env_var(env: Option<&KeybindingEnv>, name: &str) -> Option<String> {
    match env {
        Some(env) => env.get(name).cloned(),
        None => std::env::var(name).ok(),
    }
}

/// `process.platform` 的等价物（`node:os.platform()`）。
///
/// Node 的值来自编译期宏：`__APPLE__`（含 iOS 等）→ `"darwin"`，`_WIN32` → `"win32"`，
/// `__sun` → `"sunos"`，其余与 `target_os` 相同。键位只区分 `win32` / `linux` / `darwin`，
/// 但保留完整映射以免非 Linux 的 Unix 在 WSL 环境变量存在时误用 Windows 键位。
///
/// `std::env::consts::OS` 是编译期常量，同一二进制内恒定；本函数不做运行时探测。
fn process_platform() -> &'static str {
    platform_for_os(std::env::consts::OS)
}

/// [`std::env::consts::OS`] → `NodeJS.Platform`（见 [`process_platform`]）。
fn platform_for_os(os: &str) -> &'static str {
    match os {
        "macos" | "ios" | "tvos" | "watchos" | "visionos" => "darwin",
        "windows" => "win32",
        "android" => "android",
        "linux" => "linux",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        "dragonfly" => "dragonfly",
        "solaris" | "illumos" => "sunos",
        "aix" => "aix",
        "haiku" => "haiku",
        // 未列出的 OS 一律当作 linux；若这类系统上恰好设置了 WSL 环境变量，
        // `use_windows_keybindings` 会把它判成 WSL。
        _ => "linux",
    }
}

// =============================================================================
// APP_KEYBINDINGS（43 项，TS 未导出）
// =============================================================================

/// 单个键：`KeyId`。
fn one(key: &str) -> KeyIdList {
    KeyIdList::One(key.to_string())
}

/// 一组键（顺序即优先级）；空切片即 `[]`（默认不绑定）。
fn many(keys: &[&str]) -> KeyIdList {
    KeyIdList::Many(keys.iter().map(|key| (*key).to_string()).collect())
}

/// 按给定平台/Windows 键位判定构造 TS 的模块级 `APP_KEYBINDINGS`（43 项，顺序逐字）。
///
/// 两个平台开关含义不同，不能合并：
/// - `windows_keybindings` 含 WSL（`use_windows_keybindings` 的结果），决定
///   `app.model.cycleBackward`、`app.message.followUp` / `dequeue`、
///   `app.clipboard.pasteImage` 的默认键；
/// - `platform == "win32"` 只决定 `app.suspend`：原生 Windows 为 `[]`，WSL 仍保留 `ctrl+z`；
/// - `darwin` 只影响 `app.tree.foldOrUp` / `app.tree.unfoldOrDown` 两个键列表的优先顺序。
fn app_keybindings_for(platform: &str, windows_keybindings: bool) -> KeybindingDefinitions {
    let native_windows = platform == "win32";
    let darwin = platform == "darwin";

    let mut table = KeybindingDefinitions::new();
    let mut add = |id: &str, default_keys: KeyIdList, description: &str| {
        table.insert(
            id.to_string(),
            KeybindingDefinition {
                default_keys,
                description: Some(description.to_string()),
            },
        );
    };

    add("app.interrupt", one("escape"), "Cancel or abort");
    add("app.clear", one("ctrl+c"), "Clear editor");
    add("app.exit", one("ctrl+d"), "Exit when editor is empty");
    add(
        "app.suspend",
        if native_windows {
            many(&[])
        } else {
            one("ctrl+z")
        },
        "Suspend to background",
    );
    add(
        "app.thinking.cycle",
        one("shift+tab"),
        "Cycle thinking level",
    );
    add("app.thinking.save", one("ctrl+s"), "Save thinking level");
    add(
        "app.model.cycleForward",
        one("ctrl+p"),
        "Cycle to next model",
    );
    add(
        "app.model.cycleBackward",
        if windows_keybindings {
            one("alt+p")
        } else {
            one("shift+ctrl+p")
        },
        "Cycle to previous model",
    );
    add("app.model.select", one("ctrl+l"), "Open model selector");
    add("app.tools.expand", one("ctrl+o"), "Toggle tool output");
    add(
        "app.thinking.toggle",
        one("ctrl+t"),
        "Toggle thinking blocks",
    );
    add(
        "app.session.toggleNamedFilter",
        one("ctrl+n"),
        "Toggle named session filter",
    );
    add("app.editor.external", one("ctrl+g"), "Open external editor");
    add(
        "app.message.copy",
        one("ctrl+x"),
        "Copy selection or last assistant message",
    );
    add(
        "app.message.followUp",
        if windows_keybindings {
            one("ctrl+q")
        } else {
            one("alt+enter")
        },
        "Queue follow-up message",
    );
    add(
        "app.message.dequeue",
        if windows_keybindings {
            one("alt+q")
        } else {
            one("alt+up")
        },
        "Restore queued messages",
    );
    add(
        "app.clipboard.pasteImage",
        if windows_keybindings {
            one("alt+v")
        } else {
            one("ctrl+v")
        },
        "Paste files on macOS, images, or text from clipboard",
    );
    add("app.session.new", many(&[]), "Start a new session");
    add("app.session.tree", many(&[]), "Open session tree");
    add("app.session.fork", many(&[]), "Fork current session");
    add("app.session.resume", many(&[]), "Resume a session");
    add(
        "app.tree.foldOrUp",
        if darwin {
            many(&["alt+left", "ctrl+left"])
        } else {
            many(&["ctrl+left", "alt+left"])
        },
        "Fold tree branch or move up",
    );
    add(
        "app.tree.unfoldOrDown",
        if darwin {
            many(&["alt+right", "ctrl+right"])
        } else {
            many(&["ctrl+right", "alt+right"])
        },
        "Unfold tree branch or move down",
    );
    add("app.tree.editLabel", one("shift+l"), "Edit tree label");
    add(
        "app.tree.toggleLabelTimestamp",
        one("shift+t"),
        "Toggle tree label timestamps",
    );
    add(
        "app.session.togglePath",
        one("ctrl+p"),
        "Toggle session path display",
    );
    add(
        "app.session.toggleSort",
        one("ctrl+s"),
        "Toggle session sort mode",
    );
    add("app.session.rename", one("ctrl+r"), "Rename session");
    add("app.session.delete", one("ctrl+d"), "Delete session");
    add(
        "app.session.deleteNoninvasive",
        one("ctrl+backspace"),
        "Delete session when query is empty",
    );
    add("app.models.save", one("ctrl+s"), "Save model selection");
    add("app.models.enableAll", one("ctrl+a"), "Enable all models");
    add("app.models.clearAll", one("ctrl+x"), "Clear all models");
    add(
        "app.models.toggleProvider",
        one("ctrl+p"),
        "Toggle all models for provider",
    );
    add(
        "app.models.reorderUp",
        one("alt+up"),
        "Move model up in order",
    );
    add(
        "app.models.reorderDown",
        one("alt+down"),
        "Move model down in order",
    );
    add(
        "app.tree.filter.default",
        one("ctrl+d"),
        "Tree filter: default view",
    );
    add(
        "app.tree.filter.noTools",
        one("ctrl+t"),
        "Tree filter: hide tool results",
    );
    add(
        "app.tree.filter.userOnly",
        one("ctrl+u"),
        "Tree filter: user messages only",
    );
    add(
        "app.tree.filter.labeledOnly",
        one("ctrl+l"),
        "Tree filter: labeled entries only",
    );
    add(
        "app.tree.filter.all",
        one("ctrl+a"),
        "Tree filter: show all entries",
    );
    add(
        "app.tree.filter.cycleForward",
        one("ctrl+o"),
        "Tree filter: cycle forward",
    );
    add(
        "app.tree.filter.cycleBackward",
        one("shift+ctrl+o"),
        "Tree filter: cycle backward",
    );

    table
}

/// TS 模块级 `APP_KEYBINDINGS`（43 项；按当前进程的 `process.platform` / `process.env` 求值一次）。
///
/// 返回共享的静态表：首次调用时读取平台与环境（`LazyLock`），之后即使环境变量变化也不会
/// 重建。TS 里这个常量未导出、只能经 `KEYBINDINGS` 间接访问；Rust 侧把它作为公开函数，
/// 测试也直接用它校验 43 个 `app.*` 项。只需要完整默认表时用 [`keybindings`]。
pub fn app_keybindings() -> &'static KeybindingDefinitions {
    static APP_KEYBINDINGS_STATIC: LazyLock<KeybindingDefinitions> = LazyLock::new(|| {
        let platform = process_platform();
        let windows = use_windows_keybindings(Some(platform), None);
        app_keybindings_for(platform, windows)
    });
    &APP_KEYBINDINGS_STATIC
}

// =============================================================================
// KEYBINDINGS（TUI 47 + 4 覆盖 + APP 43 = 90）
// =============================================================================

/// 构造 TS 的 `KEYBINDINGS`：`TUI_KEYBINDINGS` 原序，4 个覆盖**在原位置**生效
/// （JS 展开后同名键保留首次插入位置），最后追加 `APP_KEYBINDINGS`。
fn build_keybindings(platform: &str, windows_keybindings: bool) -> KeybindingDefinitions {
    let native_windows = platform == "win32";
    // 阶段 1：取 TUI 的 47 项，顺序即 `TUI_KEYBINDINGS` 的源码顺序。
    let mut table = kiana_tui::keybindings::tui_keybindings();

    // 阶段 2：在原位置覆盖 4 个 TUI 默认键。`set_default_keys` 只改 `default_keys`，
    // 描述保留；只改值不增删条目，因此键序仍是 TUI 原序。
    // `tui.editor.undo`：win32 → ctrl+z；WSL → alt+z；其它 → ctrl+-。
    let undo = if native_windows {
        "ctrl+z"
    } else if windows_keybindings {
        "alt+z"
    } else {
        "ctrl+-"
    };
    set_default_keys(&mut table, "tui.editor.undo", one(undo));

    // `{ ...TUI_KEYBINDINGS[id], defaultKeys: ... }`：描述保留。
    set_default_keys(
        &mut table,
        "tui.altScreen.previousPrompt",
        if windows_keybindings {
            one("ctrl+up")
        } else {
            many(&["ctrl+shift+up", "ctrl+up"])
        },
    );
    set_default_keys(
        &mut table,
        "tui.altScreen.nextPrompt",
        if windows_keybindings {
            one("ctrl+down")
        } else {
            many(&["ctrl+shift+down", "ctrl+down"])
        },
    );
    set_default_keys(
        &mut table,
        "tui.altScreen.search",
        if windows_keybindings {
            one("ctrl+f")
        } else {
            one("ctrl+shift+f")
        },
    );

    // 阶段 3：追加 43 个 `app.*`。这些 id 与 TUI id 不重名，因此最终顺序是
    // 「47 个 tui.* → 43 个 app.*」，与 TS 对象展开的顺序一致。
    for (id, definition) in app_keybindings_for(platform, windows_keybindings) {
        table.insert(id, definition);
    }
    table
}

/// 覆盖 `TUI_KEYBINDINGS[id].defaultKeys`（`IndexMap::insert` 对已存在键保留位置）。
///
/// id 不在默认表里时静默跳过：调用方传入的 4 个 id 都来自
/// `kiana_tui::keybindings::KEYBINDING_IDS`；若这里查不到，说明两边的默认表 id 已经不同步，
/// 本函数不会报错或补建条目。
fn set_default_keys(table: &mut KeybindingDefinitions, id: &str, default_keys: KeyIdList) {
    if let Some(definition) = table.get_mut(id) {
        definition.default_keys = default_keys;
    }
}

/// TS `KEYBINDINGS`：按当前进程的 `process.platform` / `process.env` 在首次访问时求值一次。
///
/// 返回 90 项（47 个 TUI + 43 个 `app.*`）的共享静态表，顺序见模块文档。首次访问后表即固定；
/// 调用方包括 `schema.rs`（属性顺序）、[`KeybindingsManager::new`]（默认表）以及
/// `kiana-interactive`（克隆后装入 `kiana_tui` 的全局单例）。返回的是引用，需要修改时
/// 必须先 `clone()`。
pub fn keybindings() -> &'static KeybindingDefinitions {
    static KEYBINDINGS_STATIC: LazyLock<KeybindingDefinitions> = LazyLock::new(|| {
        let platform = process_platform();
        let windows = use_windows_keybindings(Some(platform), None);
        build_keybindings(platform, windows)
    });
    &KEYBINDINGS_STATIC
}

/// 纯构建器（TS 无对应导出）：按显式平台/环境构造 `KEYBINDINGS`。
///
/// `platform = None` 用 `process.platform`，`env = None` 用 `process.env`。
/// 用于测试 Windows/WSL/macOS 分支，以及在无法改变 `process.platform` 时复刻 TS 的
/// 「同一模块级常量」语义。
///
/// 每次调用都重新构建并返回**新的**表（不经过 `LazyLock`）。`env` 只在
/// `platform == "linux"` 时参与 WSL 判定；`Some(空 map)` 表示没有任何 WSL 变量，
/// 与 `None`（读取真实环境）不同。测试用四份 golden fixture（linux / wsl / win32 /
/// darwin）逐项比较整个表。
pub fn keybindings_for(
    platform: Option<&str>,
    env: Option<&KeybindingEnv>,
) -> KeybindingDefinitions {
    let platform: &str = match platform {
        Some(platform) => platform,
        None => process_platform(),
    };
    let windows = use_windows_keybindings(Some(platform), env);
    build_keybindings(platform, windows)
}

// =============================================================================
// 名称迁移（59 项）
// =============================================================================

/// `KEYBINDING_NAME_MIGRATIONS`（**【逐字】**59 项，顺序即源码顺序）。
///
/// 运行时由 `legacy_keybinding_name` 线性扫描（59 + 12 项）；这个顺序只用于与 TS 源码逐字
/// 对齐，不参与迁移结果的排序——输出顺序由 `order_keybindings_config` 决定。
const KEYBINDING_NAME_MIGRATIONS: [(&str, &str); 59] = [
    ("cursorUp", "tui.editor.cursorUp"),
    ("cursorDown", "tui.editor.cursorDown"),
    ("cursorLeft", "tui.editor.cursorLeft"),
    ("cursorRight", "tui.editor.cursorRight"),
    ("cursorWordLeft", "tui.editor.cursorWordLeft"),
    ("cursorWordRight", "tui.editor.cursorWordRight"),
    ("cursorLineStart", "tui.editor.cursorLineStart"),
    ("cursorLineEnd", "tui.editor.cursorLineEnd"),
    ("jumpForward", "tui.editor.jumpForward"),
    ("jumpBackward", "tui.editor.jumpBackward"),
    ("pageUp", "tui.editor.pageUp"),
    ("pageDown", "tui.editor.pageDown"),
    ("deleteCharBackward", "tui.editor.deleteCharBackward"),
    ("deleteCharForward", "tui.editor.deleteCharForward"),
    ("deleteWordBackward", "tui.editor.deleteWordBackward"),
    ("deleteWordForward", "tui.editor.deleteWordForward"),
    ("deleteToLineStart", "tui.editor.deleteToLineStart"),
    ("deleteToLineEnd", "tui.editor.deleteToLineEnd"),
    ("yank", "tui.editor.yank"),
    ("yankPop", "tui.editor.yankPop"),
    ("undo", "tui.editor.undo"),
    ("newLine", "tui.input.newLine"),
    ("submit", "tui.input.submit"),
    ("tab", "tui.input.tab"),
    ("copy", "tui.input.copy"),
    ("selectUp", "tui.select.up"),
    ("selectDown", "tui.select.down"),
    ("selectPageUp", "tui.select.pageUp"),
    ("selectPageDown", "tui.select.pageDown"),
    ("selectConfirm", "tui.select.confirm"),
    ("selectCancel", "tui.select.cancel"),
    ("interrupt", "app.interrupt"),
    ("clear", "app.clear"),
    ("exit", "app.exit"),
    ("suspend", "app.suspend"),
    ("cycleThinkingLevel", "app.thinking.cycle"),
    ("cycleModelForward", "app.model.cycleForward"),
    ("cycleModelBackward", "app.model.cycleBackward"),
    ("selectModel", "app.model.select"),
    ("expandTools", "app.tools.expand"),
    ("toggleThinking", "app.thinking.toggle"),
    ("toggleSessionNamedFilter", "app.session.toggleNamedFilter"),
    ("externalEditor", "app.editor.external"),
    ("followUp", "app.message.followUp"),
    ("dequeue", "app.message.dequeue"),
    ("pasteImage", "app.clipboard.pasteImage"),
    ("newSession", "app.session.new"),
    ("tree", "app.session.tree"),
    ("fork", "app.session.fork"),
    ("resume", "app.session.resume"),
    ("treeFoldOrUp", "app.tree.foldOrUp"),
    ("treeUnfoldOrDown", "app.tree.unfoldOrDown"),
    ("treeEditLabel", "app.tree.editLabel"),
    ("treeToggleLabelTimestamp", "app.tree.toggleLabelTimestamp"),
    ("toggleSessionPath", "app.session.togglePath"),
    ("toggleSessionSort", "app.session.toggleSort"),
    ("renameSession", "app.session.rename"),
    ("deleteSession", "app.session.delete"),
    ("deleteSessionNoninvasive", "app.session.deleteNoninvasive"),
];

/// `Object.prototype` 的自有属性名及其 `KEYBINDING_NAME_MIGRATIONS[key]` 成员经
/// `ToPropertyKey` 转换后的字符串。
///
/// TS 的 `isLegacyKeybindingName` 用 `in` 判断，对象字面量的原型链是 `Object.prototype`，
/// 因此这 12 个名字也会命中。对应的成员是原生函数（`String(fn)` → 源码文本）或
/// `__proto__` 访问器返回的 `Object.prototype`（→ `"[object Object]"`），赋值
/// `config[nextKey] = value` 时再转成属性键。Rust 无原型链，用显式表复刻同样的结果
/// （顺序即 V8 `Object.getOwnPropertyNames(Object.prototype)` 的顺序）。
const OBJECT_PROTOTYPE_KEY_TARGETS: [(&str, &str); 12] = [
    ("constructor", "function Object() { [native code] }"),
    (
        "__defineGetter__",
        "function __defineGetter__() { [native code] }",
    ),
    (
        "__defineSetter__",
        "function __defineSetter__() { [native code] }",
    ),
    (
        "hasOwnProperty",
        "function hasOwnProperty() { [native code] }",
    ),
    (
        "__lookupGetter__",
        "function __lookupGetter__() { [native code] }",
    ),
    (
        "__lookupSetter__",
        "function __lookupSetter__() { [native code] }",
    ),
    (
        "isPrototypeOf",
        "function isPrototypeOf() { [native code] }",
    ),
    (
        "propertyIsEnumerable",
        "function propertyIsEnumerable() { [native code] }",
    ),
    ("toString", "function toString() { [native code] }"),
    ("valueOf", "function valueOf() { [native code] }"),
    ("__proto__", "[object Object]"),
    (
        "toLocaleString",
        "function toLocaleString() { [native code] }",
    ),
];

/// `isLegacyKeybindingName(key)`：自有表命中，或 `Object.prototype` 成员命中
/// （Rust 无原型链，见模块文档）。
///
/// 自有表优先：若某个 `Object.prototype` 名字将来也加入迁移表，走表里的映射而不是原型
/// 复刻值。两张表都没命中时返回 `None`，调用方据此保留原名。
fn legacy_keybinding_name(key: &str) -> Option<&'static str> {
    if let Some((_, next)) = KEYBINDING_NAME_MIGRATIONS
        .iter()
        .find(|(legacy, _)| *legacy == key)
    {
        return Some(next);
    }
    OBJECT_PROTOTYPE_KEY_TARGETS
        .iter()
        .find_map(|(prototype, next)| (*prototype == key).then_some(*next))
}

/// `migrateKeybindingsConfig(rawConfig)` 的返回值（`{config, migrated}`）。
///
/// `config` 的值仍是原始 `Value`，**尚未**经过 `KeyId` 校验：非法值会原样留在这里；只有走
/// [`KeybindingsManager`] 的加载链路时，它们才会在 `to_keybindings_config` 中被丢弃。
/// `migrated` 只描述名称层面的变化，不代表配置最终可用。
#[derive(Debug, Clone, PartialEq)]
pub struct MigrateKeybindingsConfigResult {
    /// 迁移并按 `KEYBINDINGS` 顺序整理后的配置。
    ///
    /// 顺序为：已知 id 按 [`keybindings`] 的顺序 → 其余键按 UTF-16 码元序 → 最后把
    /// 数组索引形态的键提升到最前（JS 对象枚举序）。`$schema` 与未知/扩展键原样保留。
    pub config: IndexMap<String, Value>,
    /// 是否发生了改名或「新名已存在而丢弃旧名」。
    ///
    /// 旧名被改写成新名即置 `true`（即使目标值随后校验失败）；未知键原样保留不算迁移。
    pub migrated: bool,
}

/// `migrateKeybindingsConfig(rawConfig)`（**纯函数**，不写文件）。
///
/// 旧名 → 命名空间 id；若原名与新名同时存在，**新名获胜**（丢弃旧名），且记为已迁移。
///
/// 输入只读（值被克隆），不校验值的合法性，也不访问文件系统。输出顺序与输入顺序无关：
/// 已知 id 全部按 [`keybindings`] 的顺序重排，其余键按 UTF-16 码元序排序。
pub fn migrate_keybindings_config(
    raw_config: &IndexMap<String, Value>,
) -> MigrateKeybindingsConfigResult {
    let mut config: IndexMap<String, Value> = IndexMap::with_capacity(raw_config.len());
    let mut migrated = false;

    // 阶段 1：逐个输入键决定目标键名并收集值。两个易错点：
    // - 「新名已存在」查的是**原始输入** `raw_config`，不是正在构建的 `config`，所以
    //   `{"cursorUp": …, "tui.editor.cursorUp": …}` 无论谁先遍历，都会丢弃旧名；
    // - 目标键名唯一（迁移表的目标两两不同），因此 `insert` 只会新增，不会覆盖另一个输入项。
    for (key, value) in raw_config {
        let next_key = legacy_keybinding_name(key).unwrap_or(key.as_str());
        if next_key != key {
            migrated = true;
        }
        // `Object.hasOwn(rawConfig, nextKey)`：新名已存在 → 丢弃旧名。
        if key != next_key && raw_config.contains_key(next_key) {
            migrated = true;
            continue;
        }
        // `config[nextKey] = value`：已存在键保留首次插入位置（IndexMap 语义与 JS 一致）。
        config.insert(next_key.to_string(), value.clone());
    }

    // 阶段 2：统一排序并返回（详见 `order_keybindings_config`）。
    MigrateKeybindingsConfigResult {
        config: order_keybindings_config(&config),
        migrated,
    }
}

/// `orderKeybindingsConfig(config)`：先按 `KEYBINDINGS` 顺序，其余键按 JS 默认排序追加，
/// 最后按 JS 对象的自有键枚举序返回。
///
/// 排序依据是进程当前的 [`keybindings`] 静态表，而不是调用方传入的表；`$schema` 由于不在
/// 表中，会被当作 rest 键参与排序（其码元序通常最靠前）。未知/扩展键不会被丢弃。
fn order_keybindings_config(config: &IndexMap<String, Value>) -> IndexMap<String, Value> {
    let mut ordered: IndexMap<String, Value> = IndexMap::with_capacity(config.len());
    // 阶段 1：已知 id 按 KEYBINDINGS 的固定顺序输出。
    for keybinding in keybindings().keys() {
        if let Some(value) = config.get(keybinding) {
            ordered.insert(keybinding.clone(), value.clone());
        }
    }

    // 阶段 2：其余键（`$schema`、扩展动作、未知 id）按 JS 默认排序追加。
    let mut extras: Vec<&String> = config
        .keys()
        .filter(|key| !ordered.contains_key(key.as_str()))
        .collect();
    // 默认 `Array.prototype.sort`：UTF-16 码元序（键唯一，稳定性无关）。
    extras.sort_by(|left, right| js_code_unit_cmp(left, right));
    for key in extras {
        ordered.insert(key.clone(), config[key].clone());
    }
    // 阶段 3：JS 对象会把整数索引键提升到最前，必须在上面两步排完之后再做。
    js_own_property_order(ordered)
}

/// 把 `ordered` 重排成 JS 对象的自有键枚举序（`OrdinaryOwnPropertyKeys`）：
/// 数组索引键按数值升序在前，其余键保持插入序。
///
/// `orderKeybindingsConfig` 返回的是普通 JS 对象：即使整数形态的键最后才插入，
/// `Object.keys` / `Object.entries` 仍会先把它们按升序取出。`IndexMap` 只保插入序，
/// 因此返回前显式重排（否则 `migrateKeybindingsConfig(...).config` 与
/// `getUserBindings()` 的键序和 TS 不同）。
fn js_own_property_order(ordered: IndexMap<String, Value>) -> IndexMap<String, Value> {
    js_own_property_order_map(ordered)
}

/// [`js_own_property_order`] 的泛型版本（`KeybindingsConfig` 的值不是 `Value`）。
///
/// 按值消费输入 map，一次遍历分成「数组索引键」与「其余键」两桶后重建：索引键按数值升序，
/// 其余键保持原来的相对顺序。调用点是 [`js_own_property_order`]（`order_keybindings_config`
/// 的收尾阶段）、[`KeybindingsManager::new`] 与 [`KeybindingsManager::set_user_bindings`]；
/// 它们借此把来自 Rust 调用方的任意
/// `IndexMap` 归一成 TS 普通对象的枚举序。
fn js_own_property_order_map<V>(map: IndexMap<String, V>) -> IndexMap<String, V> {
    let mut indices: Vec<(u32, String, V)> = Vec::new();
    let mut others: Vec<(String, V)> = Vec::new();
    // 分桶：索引键带上数值用于排序；其余键只保留出现顺序。
    for (key, value) in map {
        match js_array_index(&key) {
            Some(index) => indices.push((index, key, value)),
            None => others.push((key, value)),
        }
    }
    // 同一对象里索引键唯一，稳定性无关。
    indices.sort_by_key(|(index, _, _)| *index);
    // 两段式输出对应 `OrdinaryOwnPropertyKeys`：先索引键，后其余键。
    let mut result = IndexMap::with_capacity(indices.len() + others.len());
    for (_, key, value) in indices {
        result.insert(key, value);
    }
    for (key, value) in others {
        result.insert(key, value);
    }
    result
}

/// ECMA-262 的「数组索引」键：`0`…`4294967294` 的规范十进制写法（无前导零）。
///
/// 判定与 `kiana_json::js_object` 的同名私有函数一致（该函数未导出，见 `kiana-codemode` 的
/// 同类注释）。`"4294967295"`（`2^32-1`）、`"01"`、`"-0"`、`"1.5"` 都不是索引键。
fn js_array_index(key: &str) -> Option<u32> {
    if key.is_empty() || key.len() > 10 {
        return None;
    }
    if key.len() > 1 && key.starts_with('0') {
        return None;
    }
    if !key.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let value: u32 = key.parse().ok()?;
    (value != u32::MAX).then_some(value)
}

/// `Array.prototype.sort()` 默认比较：逐 UTF-16 码元比较。
///
/// UTF-16 码元序与 Unicode 码点序不同：补充平面字符（代理对 `0xD800`–`0xDFFF`）会排在
/// `U+E000`–`U+FFFF` 之前。测试 `sort_uses_utf16_code_units` 锁定了这一差异。
fn js_code_unit_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

// =============================================================================
// keybindings.json 读取
// =============================================================================

/// `loadRawConfig(path)`：文件不存在/读取失败/JSON 解析失败/顶层非对象 → `None`。
///
/// `JSON.parse` 的 `typeof parsed === "object"` 对**数组**也为 true，因此顶层数组同样被
/// 接受（键为 `"0"`、`"1"`…，与 `Object.entries` 一致）。
///
/// 读取前先 `exists()`，因此 `exists()` 与 `read()` 之间文件被删除也会走 `None` 分支。
/// 解析走 `kiana_json::js_parse`（与 JS `JSON.parse` 相同的键序处理），解析前剥离 UTF-8
/// BOM；非法 UTF-8 字节被替换为 U+FFFD 后继续解析，不会在这里报错。
fn load_raw_config(path: &Path) -> Option<IndexMap<String, Value>> {
    if !path.exists() {
        return None;
    }
    // IO 失败（权限、竞态删除等）同样按「没有配置」处理，不向上传播。
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let parsed = kiana_json::js_parse::parse(&strip_bom(&text)).ok()?;
    match parsed {
        // 顶层对象：`serde_json` 启用了 `preserve_order`，迭代即解析后的键序。
        Value::Object(object) => Some(object.into_iter().collect()),
        // 顶层数组：索引转成字符串键，值原样保留。
        Value::Array(items) => Some(
            items
                .into_iter()
                .enumerate()
                .map(|(index, value)| (index.to_string(), value))
                .collect(),
        ),
        _ => None,
    }
}

/// `toKeybindingsConfig(value)`：跳过 `$schema`；值不满足 `KeybindingValueSchema` 时静默丢弃。
///
/// 输入顺序保留；合法的空数组会保留为 `Many(vec![])`（表示显式解绑）。任何非法项都只是
/// 让该键消失，不会产生错误，也不影响其它键。
fn to_keybindings_config(value: IndexMap<String, Value>) -> KeybindingsConfig {
    let mut config = KeybindingsConfig::new();
    for (key, binding) in value {
        if key == "$schema" {
            continue;
        }
        let Some(keys) = key_id_list_from_value(&binding) else {
            continue;
        };
        config.insert(key, keys);
    }
    config
}

/// `validateKeybindingValue.Check(binding)` 通过时的类型化转换；否则 `None`。
///
/// 字符串要求 `key_id::is_valid_key_id` 通过；数组要求**每个**元素都是合法字符串，任一元素
/// 失败即整体返回 `None`（不保留合法前缀），空数组返回 `KeyIdList::Many(vec![])`。数字、
/// 对象、嵌套数组等其它 JSON 类型一律 `None`。
fn key_id_list_from_value(value: &Value) -> Option<KeyIdList> {
    match value {
        Value::String(binding) if key_id::is_valid_key_id(binding) => {
            Some(KeyIdList::One(binding.clone()))
        }
        Value::Array(bindings) => {
            let mut keys = Vec::with_capacity(bindings.len());
            for binding in bindings {
                match binding {
                    Value::String(binding) if key_id::is_valid_key_id(binding) => {
                        keys.push(binding.clone());
                    }
                    _ => return None,
                }
            }
            Some(KeyIdList::Many(keys))
        }
        _ => None,
    }
}

// =============================================================================
// KeybindingsManager
// =============================================================================

/// 应用层键位管理器：`KeybindingsManager extends TuiKeybindingsManager` 的组合版本。
///
/// 持有 `kiana_tui` 的管理器（默认表为 [`keybindings`]）与可选的 `keybindings.json` 路径。
/// `create` 时读取文件（内存中先迁移旧名，文件本身**不**改写——重写在 `ca-cli` 的
/// `runMigrations`）；`reload()` 重新读取。
///
/// 状态分工：`inner` 保存默认定义、用户绑定、解析后的每个 id 的键以及冲突列表；
/// `config_path` 只被 [`KeybindingsManager::reload`] 使用，`None` 表示不可重载。
/// 类型本身没有共享可变状态（`reload` 要求 `&mut self`），`Clone` 是深拷贝——克隆体与
/// 原体之后的修改互不影响。匹配、解析与冲突检测全部委托给 `inner`，本类型只负责
/// 「默认表 + 文件路径」。
#[derive(Debug, Clone)]
pub struct KeybindingsManager {
    inner: kiana_tui::keybindings::KeybindingsManager,
    config_path: Option<PathBuf>,
}

impl KeybindingsManager {
    /// `new KeybindingsManager(userBindings = {}, configPath?)`。
    ///
    /// TS 的 `userBindings` 是普通对象，自有键枚举序恒为「整数索引键升序 → 其余插入序」；
    /// Rust 的 `IndexMap` 可能来自任意顺序，构造时统一归一（`getUserBindings()` 的返回值
    /// 在 TS 里同样经对象展开，键序不变）。
    ///
    /// 默认表取调用时刻 [`keybindings`] 的克隆（90 项），构造器内部立即完成一次全量解析，
    /// 因此返回后 `get_resolved_bindings()` / `get_conflicts()` 已可用。`config_path` 仅
    /// 保存给 [`KeybindingsManager::reload`]，`None` 时重载是 no-op。
    pub fn new(user_bindings: KeybindingsConfig, config_path: Option<PathBuf>) -> Self {
        KeybindingsManager {
            inner: kiana_tui::keybindings::KeybindingsManager::new(
                keybindings().clone(),
                js_own_property_order_map(user_bindings),
            ),
            config_path,
        }
    }

    /// `static create(agentDir = getAgentDir())`：读取 `<agentDir>/keybindings.json`。
    ///
    /// 路径固定拼接 `keybindings.json`。文件问题（不存在、不可读、JSON 非法、顶层不是
    /// 对象/数组）全部退化为空用户绑定，不会失败也不会创建文件；该路径会被保存，供
    /// [`KeybindingsManager::reload`] 再次读取。
    pub fn create(agent_dir: impl AsRef<Path>) -> Self {
        let config_path = agent_dir.as_ref().join("keybindings.json");
        let user_bindings = Self::load_from_file(&config_path);
        Self::new(user_bindings, Some(config_path))
    }

    /// `static create()`（默认 `getAgentDir()`）。
    ///
    /// `getAgentDir()` 每次调用都会重新解析：`KIANA_CODING_AGENT_DIR` 非空时用它（展开
    /// `~`），否则 `<home>/.kiana/agent`；home 取不到时退化为相对路径。
    pub fn create_default() -> Self {
        Self::create(get_agent_dir())
    }

    /// `reload()`：重新读取配置文件；没有配置路径时什么也不做。
    ///
    /// 是**整体替换**：重新读取的结果（包括读取失败得到的空配置）会覆盖当前用户绑定，
    /// 不会在失败时保留上一次成功加载的内容。文件中的旧键名同样会在内存中迁移。
    pub fn reload(&mut self) {
        let Some(config_path) = self.config_path.as_ref() else {
            return;
        };
        let user_bindings = Self::load_from_file(config_path);
        self.set_user_bindings(user_bindings);
    }

    /// `getEffectiveConfig()`：等价于 `getResolvedBindings()`。
    ///
    /// 返回全部 90 个已定义 id 的最终绑定：用户覆盖优先，`Many(vec![])` 表示显式解绑。
    /// `kiana-interactive` 把它交给扩展 runner 查询快捷键，并用于生成 `/hotkeys` 文案
    /// （`handle_hotkeys_command` 生成的 Keyboard Shortcuts 列表）。
    pub fn get_effective_config(&self) -> KeybindingsConfig {
        self.get_resolved_bindings()
    }

    /// `private static loadFromFile(path)`：原始读取 → 迁移 → 校验。
    ///
    /// 读取或解析失败直接得到空配置；迁移是纯函数不会失败；校验阶段逐值丢弃非法项。
    /// 全程只读，不会写回文件。
    fn load_from_file(path: &Path) -> KeybindingsConfig {
        let Some(raw_config) = load_raw_config(path) else {
            // 对应 TS `loadRawConfig` 返回 undefined 时的 `return {}`。
            return KeybindingsConfig::new();
        };
        // 先改名/排序，再校验；两步都不产生错误，非法值在最后一步消失。
        to_keybindings_config(migrate_keybindings_config(&raw_config).config)
    }

    /// `matches(data, keybinding)`。
    ///
    /// 该键位的任一键匹配即 `true`；`data` 必须是一条**完整**按键序列（与
    /// `kiana_tui::keys::matches_key` 的约定一致，不做前缀匹配）。未知 id 恒为 `false`，
    /// 被显式解绑的 id 同样恒为 `false`（键列表为空）。
    pub fn matches(&self, data: &str, keybinding: &str) -> bool {
        self.inner.matches(data, keybinding)
    }

    /// `getKeys(keybinding)`。
    ///
    /// 返回解析后的键列表**副本**；未知 id 与被显式空列表解绑的 id 都返回空 `Vec`。
    pub fn get_keys(&self, keybinding: &str) -> Vec<KeyId> {
        self.inner.get_keys(keybinding)
    }

    /// `getDefinition(keybinding)`。
    ///
    /// 返回默认定义表里的引用（`default_keys` 与 `description`），**不**反映用户覆盖；
    /// 未知 id 返回 `None`。
    pub fn get_definition(&self, keybinding: &str) -> Option<&KeybindingDefinition> {
        self.inner.get_definition(keybinding)
    }

    /// `getConflicts()`。
    ///
    /// 只统计用户绑定之间的重复键：默认表里不同 id 共用同一个键（例如 `up` 同时出现在
    /// `tui.editor.cursorUp` 与 `tui.select.up`）不算冲突，用户配置里的未知 id 也不参与。
    /// 顺序为冲突键在用户配置中首次出现的顺序；返回副本。
    pub fn get_conflicts(&self) -> Vec<KeybindingConflict> {
        self.inner.get_conflicts()
    }

    /// `setUserBindings(config)`：与构造器一样先把键序归一成 JS 对象的枚举序。
    ///
    /// 是整体替换而非合并：新配置中没出现的 id 会回退到默认键；替换后立即重建解析结果
    /// 与冲突列表。未知 id 会被静默忽略。
    pub fn set_user_bindings(&mut self, user_bindings: KeybindingsConfig) {
        self.inner
            .set_user_bindings(js_own_property_order_map(user_bindings));
    }

    /// `getUserBindings()`。
    ///
    /// 返回当前用户绑定的副本（不是与默认值合并后的结果），键序已归一成 JS 对象枚举序；
    /// 修改返回值不影响管理器。
    pub fn get_user_bindings(&self) -> KeybindingsConfig {
        self.inner.get_user_bindings()
    }

    /// `getResolvedBindings()`。
    ///
    /// 每个已定义 id 一条，顺序等于默认表顺序（TUI 原序 + app 原序）：只剩 1 个键时给
    /// `KeyIdList::One`，0 个或多于 1 个时给 `KeyIdList::Many`（空 `Many` 表示显式解绑）。
    /// 用户配置中的未知 id 不会出现在结果里。
    pub fn get_resolved_bindings(&self) -> KeybindingsConfig {
        self.inner.get_resolved_bindings()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_table_has_fifty_nine_entries() {
        assert_eq!(KEYBINDING_NAME_MIGRATIONS.len(), 59);
        assert_eq!(
            legacy_keybinding_name("cursorUp"),
            Some("tui.editor.cursorUp")
        );
        assert_eq!(
            legacy_keybinding_name("deleteSessionNoninvasive"),
            Some("app.session.deleteNoninvasive")
        );
        assert_eq!(legacy_keybinding_name("app.clear"), None);
        // `in` 命中原型链：迁移表是对象字面量，Object.prototype 的 12 个成员都算旧名。
        assert_eq!(
            legacy_keybinding_name("toString"),
            Some("function toString() { [native code] }")
        );
        assert_eq!(legacy_keybinding_name("__proto__"), Some("[object Object]"));
        assert_eq!(
            legacy_keybinding_name("constructor"),
            Some("function Object() { [native code] }")
        );
        assert_eq!(OBJECT_PROTOTYPE_KEY_TARGETS.len(), 12);
    }

    #[test]
    fn array_index_definition_matches_ecmascript() {
        assert_eq!(js_array_index("0"), Some(0));
        assert_eq!(js_array_index("12"), Some(12));
        assert_eq!(js_array_index("4294967294"), Some(4_294_967_294));
        assert_eq!(js_array_index("4294967295"), None);
        assert_eq!(js_array_index("01"), None);
        assert_eq!(js_array_index("-0"), None);
        assert_eq!(js_array_index("1.5"), None);
        assert_eq!(js_array_index(""), None);
        assert_eq!(js_array_index("10000000000"), None);
    }

    #[test]
    fn default_tables_have_expected_sizes() {
        assert_eq!(app_keybindings().len(), 43);
        assert_eq!(keybindings().len(), 90);
    }

    #[test]
    fn platform_names_match_node() {
        assert_eq!(platform_for_os("macos"), "darwin");
        assert_eq!(platform_for_os("windows"), "win32");
        assert_eq!(platform_for_os("linux"), "linux");
    }

    #[test]
    fn sort_uses_utf16_code_units() {
        // U+E000..U+FFFF 在 UTF-16 码元序里排在补充平面字符之后。
        assert_eq!(
            js_code_unit_cmp("\u{FFFD}", "\u{10000}"),
            std::cmp::Ordering::Greater
        );
        assert_eq!(js_code_unit_cmp("a", "b"), std::cmp::Ordering::Less);
    }
}
