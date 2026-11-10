//! `/permissions` 面板的命令层（`tui-panels-design.md` §3）。
//!
//! 命令层只做三件事：
//!
//! 1. 从会话引擎与 settings 分层组装 [`PermissionSnapshot`]（组件不读文件）；
//! 2. 把组件的写回调映射到现有的 settings / 会话 API，并在成功后回填新快照；
//! 3. 用 `show_selector` 把 [`PermissionsDialog`] 挂到编辑器槽位。
//!
//! # 借用纪律
//!
//! 所有回调都在组件自己的 `handle_input` 借用栈上同步执行；此时组件已经被
//! `Rc<RefCell<PermissionsDialog>>` 可变借用，回调里**不能**再借同一个对话框。
//! 因此写操作分成两步：
//!
//! - 同步部分只做文件/settings/会话写入并返回 `Result`；
//! - 回填快照与 `request_render` 放进 `spawn_local`，在本次输入分发返回后执行。
//!
//! 模式切换成功后还要刷新 footer 的权限指示器；同样延后到 `spawn_local` 里做，
//! 避免在回调栈上重入渲染器。
//!
//! # 写回顺序（fail-closed）
//!
//! 先写文件/settings、成功后才更新运行期引擎。写失败时组件保留输入/确认视图并显示
//! 错误，运行期状态不变；这与设计文档 §3.5/§3.6 的“写失败不能改运行期”一致。
//! 唯一的例外是模式切换的 Session-only 作用域：它不写盘，直接调用
//! `session.set_permission_mode`。
//!
//! # 已知差距
//!
//! - 删除工作区目录需要 `AgentSession::set_permission_workspace_roots`（设计 §3.7），
//!   当前后端未提供；回调返回明确错误，不静默改内存。
//! - Local/User 的规则删除使用“解析后等值匹配”，与面板展示的去重规则一致；文件里
//!   无法解析的条目会原样保留。
//! - 写操作后的快照刷新会先 `SettingsManager::flush()` 再读取文件层，并把
//!   `drain_errors()` 里的持久化失败显示成面板 notice（写队列本身仍由管理器串行）。

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use kiana_config::settings::SettingsManager;
use kiana_config::settings::permissions as permissions_settings;
use kiana_permissions::{PermissionMode, PermissionRule, RuleDecision};
use kiana_session::agent_session::AgentSession;
use serde_json::{Map, Value};

use crate::components::permissions_dialog::{
    ModePersistenceScope, PanelNotice, PermissionRuleScope, PermissionRuleScopeChoice,
    PermissionSnapshot, PermissionSnapshotInput, PermissionsCallbacks, PermissionsDialog,
    PermissionsDialogOptions, collect_settings_layers, parse_settings_rule_value,
    parse_shorthand_rule_value,
};

use super::super::{InteractiveMode, InteractiveModeInner};
use super::{SelectorCreation, SelectorFocus};

/// 对话框的共享槽位：回调在组件构造前创建，通过它在写操作后回填快照。
type DialogSlot = Rc<RefCell<Option<Rc<RefCell<PermissionsDialog>>>>>;

impl InteractiveMode {
    /// 打开 `/permissions` 面板。
    pub(crate) fn show_permissions_selector(&mut self) {
        let session = self.session();
        let settings = self.settings_manager();
        let cwd = PathBuf::from(session.session_manager().get_cwd());
        let project_trusted = settings.is_project_trusted();
        let snapshot = build_snapshot(&session, &settings, &cwd);
        let weak = self.downgrade();

        self.show_selector(move |_mode, done| {
            let slot: DialogSlot = Rc::new(RefCell::new(None));

            let callbacks = PermissionsCallbacks {
                on_add_rule: {
                    let session = session.clone();
                    let settings = settings.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    let cwd = cwd.clone();
                    Box::new(move |rule, choice| {
                        add_rule_to_scope(&session, &settings, &cwd, rule, choice)?;
                        schedule_refresh(&slot, &session, &settings, &weak, true);
                        Ok(())
                    })
                },
                on_delete_rule: {
                    let session = session.clone();
                    let settings = settings.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    let cwd = cwd.clone();
                    Box::new(move |entry| {
                        delete_rule_from_scopes(&session, &settings, &cwd, entry)?;
                        schedule_refresh(&slot, &session, &settings, &weak, true);
                        Ok(())
                    })
                },
                on_set_mode: {
                    let session = session.clone();
                    let settings = settings.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    let cwd = cwd.clone();
                    Box::new(move |mode, scope| {
                        set_permission_mode(&session, &settings, &cwd, mode, scope)?;
                        // footer 指示器只在模式变化后刷新；组件自己的快照由它更新。
                        let weak_footer = weak.clone();
                        let mode_for_footer = Some(mode);
                        tokio::task::spawn_local(async move {
                            if let Some(inner) = weak_footer.upgrade() {
                                let mut mode_ctx = InteractiveMode { inner };
                                mode_ctx
                                    .inner
                                    .borrow()
                                    .footer
                                    .borrow_mut()
                                    .set_permission_mode(mode_for_footer);
                                mode_ctx.request_render();
                            }
                        });
                        schedule_refresh(&slot, &session, &settings, &weak, true);
                        Ok(())
                    })
                },
                on_add_directory: {
                    let session = session.clone();
                    let settings = settings.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    let cwd = cwd.clone();
                    Box::new(move |path, choice| {
                        add_directory_to_scope(&session, &settings, &cwd, path, choice)?;
                        schedule_refresh(&slot, &session, &settings, &weak, true);
                        Ok(())
                    })
                },
                on_remove_directory: Box::new(|_path| {
                    Err(
                        "Removing workspace directories is not supported yet; edit the settings file directly."
                            .to_string(),
                    )
                }),
                on_refresh: {
                    let session = session.clone();
                    let settings = settings.clone();
                    let slot = Rc::clone(&slot);
                    let weak = weak.clone();
                    Box::new(move || {
                        schedule_refresh(&slot, &session, &settings, &weak, false);
                    })
                },
                on_close: Box::new(move || done.finish()),
            };

            let dialog = Rc::new(RefCell::new(PermissionsDialog::new(
                PermissionsDialogOptions {
                    snapshot,
                    cwd,
                    project_trusted,
                    callbacks,
                },
            )));
            *slot.borrow_mut() = Some(Rc::clone(&dialog));
            SelectorCreation {
                component: kiana_tui::layout_node::shared(Rc::clone(&dialog)),
                focus: SelectorFocus::Component,
                dispose: None,
            }
        });
    }
}

/// 从引擎策略 + settings 分层组装快照。
///
/// `errors` 合并两类来源：settings 文件读取错误（`collect_settings_layers`）与引擎
/// 快照本身的缺失（引擎未启用时没有规则/目录，`mode = None`）。
fn build_snapshot(
    session: &AgentSession,
    settings: &SettingsManager,
    cwd: &Path,
) -> PermissionSnapshot {
    let policy = session.permission_policy();
    let (layers, errors) = collect_settings_layers(settings, cwd);
    PermissionSnapshot::build(PermissionSnapshotInput {
        cwd: cwd.to_path_buf(),
        mode: policy
            .as_ref()
            .map(|policy| policy.mode)
            .or_else(|| session.permission_mode()),
        bypass_disabled: session.bypass_permissions_disabled(),
        policy_rules: policy
            .as_ref()
            .map(|policy| policy.rules.clone())
            .unwrap_or_default(),
        workspace_roots: policy
            .as_ref()
            .map(|policy| policy.workspace_roots.clone())
            .unwrap_or_else(|| vec![cwd.to_path_buf()]),
        session_allow_count: session.permission_session_allow_count(),
        layers,
        errors,
    })
}

/// 写操作成功后的延迟刷新：回填新快照并请求重绘。
///
/// `report_write_errors` 为真时（写操作后）先 `flush()` 写队列再取快照，并把
/// `drain_errors()` 里的持久化失败显示成 notice。`SettingsManager` 的写入是同步
/// 入队、异步落盘；不 flush 的话面板展示成功，但磁盘失败永远没人报告。
fn schedule_refresh(
    slot: &DialogSlot,
    session: &AgentSession,
    settings: &SettingsManager,
    weak: &Weak<RefCell<InteractiveModeInner>>,
    report_write_errors: bool,
) {
    let slot = Rc::clone(slot);
    let session = session.clone();
    let settings = settings.clone();
    let weak = weak.clone();
    tokio::task::spawn_local(async move {
        if report_write_errors {
            settings.flush().await;
        }
        let errors = if report_write_errors {
            settings.drain_errors()
        } else {
            Vec::new()
        };
        let dialog = slot.borrow().clone();
        if let Some(dialog) = dialog {
            let cwd = dialog.borrow().cwd().to_path_buf();
            let snapshot = build_snapshot(&session, &settings, &cwd);
            let mut dialog = dialog.borrow_mut();
            dialog.set_snapshot(snapshot);
            if !errors.is_empty() {
                let message = errors
                    .iter()
                    .map(|error| error.error.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                dialog.set_notice(Some(PanelNotice::error(format!(
                    "Settings write failed: {message}"
                ))));
            }
        }
        if let Some(inner) = weak.upgrade() {
            InteractiveMode { inner }.request_render();
        }
    });
}

/// `permissions` 对象上追加规则（内存版）。
///
/// 调用前已经通过 [`preflight_permissions_object`] 校验形状，因此 `append_rule_to_permissions`
/// 只可能因为并发的外部修改失败；此时保持原值并让后续快照刷新暴露差异。
fn append_rule_value(current: Option<&Value>, rule: &Value) -> Value {
    let mut value = current
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let _ = permissions_settings::append_rule_to_permissions(&mut value, rule);
    value
}

/// 校验 settings 里的 `permissions` 形状是否可追加/删除。
fn preflight_permissions_object(current: Option<&Value>) -> Result<(), String> {
    let Some(value) = current else {
        return Ok(());
    };
    if value.is_null() {
        return Ok(());
    }
    let object = value
        .as_object()
        .ok_or_else(|| "permissions must be a JSON object".to_string())?;
    if let Some(rules) = object.get("rules")
        && !rules.is_array()
    {
        return Err("permissions.rules must be an array".to_string());
    }
    Ok(())
}

/// 按作用域写盘并更新运行期引擎。
fn add_rule_to_scope(
    session: &AgentSession,
    settings: &SettingsManager,
    cwd: &Path,
    rule: &PermissionRule,
    choice: PermissionRuleScopeChoice,
) -> Result<(), String> {
    let raw = rule.to_settings_value();
    match choice {
        PermissionRuleScopeChoice::Project => {
            preflight_permissions_object(settings.project_permissions().as_ref())?;
            settings
                .update_project_permissions(|current| append_rule_value(current, &raw))
                .map_err(|error| error.to_string())?;
        }
        PermissionRuleScopeChoice::Local => {
            let path = permissions_settings::local_settings_path(cwd);
            let existing = permissions_settings::read_permissions_file(&path)
                .map_err(|error| error.to_string())?;
            preflight_permissions_object(existing.as_ref())?;
            permissions_settings::append_permission_rule(&path, &raw)
                .map_err(|error| error.to_string())?;
        }
        PermissionRuleScopeChoice::User => {
            let existing = settings.get_global_settings().raw("permissions");
            preflight_permissions_object(existing.as_ref())?;
            settings
                .update_global_permissions(|current| append_rule_value(current, &raw))
                .map_err(|error| error.to_string())?;
        }
    }
    session.add_permission_rules([rule.clone()]);
    Ok(())
}

/// 从可编辑作用域删除规则，并在运行期移除同一条规则。
fn delete_rule_from_scopes(
    session: &AgentSession,
    settings: &SettingsManager,
    cwd: &Path,
    entry: &crate::components::permissions_dialog::PermissionRuleEntry,
) -> Result<(), String> {
    if !entry.removable {
        return Err(
            "This rule is enforced by managed policy or the CLI and cannot be removed here."
                .to_string(),
        );
    }
    for scope in &entry.scopes {
        match scope {
            PermissionRuleScope::User => {
                preflight_permissions_object(
                    settings.get_global_settings().raw("permissions").as_ref(),
                )?;
                settings
                    .update_global_permissions(|current| {
                        remove_rule_value(current, &entry.rule, cwd)
                    })
                    .map_err(|error| error.to_string())?;
            }
            PermissionRuleScope::Project => {
                preflight_permissions_object(settings.project_permissions().as_ref())?;
                settings
                    .update_project_permissions(|current| {
                        remove_rule_value(current, &entry.rule, cwd)
                    })
                    .map_err(|error| error.to_string())?;
            }
            PermissionRuleScope::Local => {
                let path = permissions_settings::local_settings_path(cwd);
                permissions_settings::update_permissions_file(&path, |permissions| {
                    Ok(remove_rule_value_in_place(permissions, &entry.rule, cwd))
                })
                .map_err(|error| error.to_string())?;
            }
            PermissionRuleScope::ManagedOrCli => {
                return Err(
                    "This rule is enforced by managed policy or the CLI and cannot be removed here."
                        .to_string(),
                );
            }
        }
    }

    // 先写盘、后改运行期：把已删除的规则从引擎里移除，其余有效规则（含 managed/CLI）
    // 原样保留。
    let mut rules = session
        .permission_policy()
        .map(|policy| policy.rules)
        .unwrap_or_default();
    rules.retain(|rule| rule != &entry.rule);
    session.set_permission_rules(rules);
    Ok(())
}

/// 在 `permissions` 内存对象上删除与 `rule` 等值的规则。
fn remove_rule_value(current: Option<&Value>, rule: &PermissionRule, cwd: &Path) -> Value {
    let mut value = current
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    remove_rule_value_in_place(&mut value, rule, cwd);
    value
}

/// 删除 `rules` 数组与 `allow`/`deny`/`ask` 简写里与 `rule` 等值的条目。
///
/// 文件里的简写字符串与面板的规范对象形态不同，因此逐项解析后比较，而不是只比较
/// `to_settings_value()`。
fn remove_rule_value_in_place(permissions: &mut Value, rule: &PermissionRule, cwd: &Path) -> bool {
    let Some(object) = permissions.as_object_mut() else {
        return false;
    };
    let mut removed = false;
    if let Some(rules) = object.get_mut("rules").and_then(Value::as_array_mut) {
        let before = rules.len();
        let canonical = rule.to_settings_value();
        rules.retain(|item| {
            if item == &canonical {
                return false;
            }
            match parse_settings_rule_value(item, cwd) {
                Some(parsed) => &parsed != rule,
                None => true,
            }
        });
        removed |= rules.len() != before;
    }
    for (key, decision) in [
        ("allow", RuleDecision::Allow),
        ("deny", RuleDecision::Deny),
        ("ask", RuleDecision::Ask),
    ] {
        if let Some(items) = object.get_mut(key).and_then(Value::as_array_mut) {
            let before = items.len();
            items.retain(|item| {
                let Some(text) = item.as_str() else {
                    return true;
                };
                // 与面板归因使用同一套解析：简写里的 `path:` 前缀由引擎按工作区根
                // 归一化，直接用 `parse_cli_rule` 得到的相对路径永远不等于面板条目
                // （绝对路径），规则删不掉、重启后又会出现。
                match parse_shorthand_rule_value(text, decision, cwd) {
                    Some(parsed) => &parsed != rule,
                    None => true,
                }
            });
            removed |= items.len() != before;
        }
    }
    removed
}

/// 切换模式：先持久化（按作用域），成功后再改运行期。
fn set_permission_mode(
    session: &AgentSession,
    settings: &SettingsManager,
    cwd: &Path,
    mode: PermissionMode,
    scope: ModePersistenceScope,
) -> Result<(), String> {
    let mode_value = Value::String(mode.as_str().to_string());
    match scope {
        ModePersistenceScope::Session => {}
        ModePersistenceScope::Project => {
            preflight_permissions_object(settings.project_permissions().as_ref())?;
            settings
                .update_project_permissions(|current| set_mode_value(current, &mode_value))
                .map_err(|error| error.to_string())?;
        }
        ModePersistenceScope::Local => {
            let path = permissions_settings::local_settings_path(cwd);
            permissions_settings::update_permissions_file(&path, |permissions| {
                set_mode_value_in_place(permissions, &mode_value);
                Ok(true)
            })
            .map_err(|error| error.to_string())?;
        }
        ModePersistenceScope::User => {
            preflight_permissions_object(
                settings.get_global_settings().raw("permissions").as_ref(),
            )?;
            settings
                .update_global_permissions(|current| set_mode_value(current, &mode_value))
                .map_err(|error| error.to_string())?;
        }
    }
    session.set_permission_mode(mode);
    Ok(())
}

fn set_mode_value(current: Option<&Value>, mode: &Value) -> Value {
    let mut value = current
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    set_mode_value_in_place(&mut value, mode);
    value
}

fn set_mode_value_in_place(permissions: &mut Value, mode: &Value) {
    if let Some(object) = permissions.as_object_mut() {
        object.insert("mode".to_string(), mode.clone());
    }
}

/// 新增工作区目录：写盘 + 同步运行期。
fn add_directory_to_scope(
    session: &AgentSession,
    settings: &SettingsManager,
    cwd: &Path,
    path: &Path,
    choice: PermissionRuleScopeChoice,
) -> Result<(), String> {
    let text = path.to_string_lossy().to_string();
    match choice {
        PermissionRuleScopeChoice::Project => {
            preflight_permissions_object(settings.project_permissions().as_ref())?;
            settings
                .update_project_permissions(|current| append_directory_value(current, &text))
                .map_err(|error| error.to_string())?;
        }
        PermissionRuleScopeChoice::Local => {
            let settings_path = permissions_settings::local_settings_path(cwd);
            permissions_settings::update_permissions_file(&settings_path, |permissions| {
                Ok(append_directory_in_place(permissions, &text))
            })
            .map_err(|error| error.to_string())?;
        }
        PermissionRuleScopeChoice::User => {
            preflight_permissions_object(
                settings.get_global_settings().raw("permissions").as_ref(),
            )?;
            settings
                .update_global_permissions(|current| append_directory_value(current, &text))
                .map_err(|error| error.to_string())?;
        }
    }
    session.add_permission_directory(path.to_path_buf());
    Ok(())
}

fn append_directory_value(current: Option<&Value>, path: &str) -> Value {
    let mut value = current
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    append_directory_in_place(&mut value, path);
    value
}

/// 返回是否有改动（用于 `update_permissions_file` 的写盘判断）。
fn append_directory_in_place(permissions: &mut Value, path: &str) -> bool {
    let Some(object) = permissions.as_object_mut() else {
        return false;
    };
    let entry = object
        .entry("addDirs".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(items) = entry.as_array_mut() else {
        return false;
    };
    if items.iter().any(|item| item.as_str() == Some(path)) {
        return false;
    }
    items.push(Value::String(path.to_string()));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorthand_path_rules_are_deleted_with_engine_normalization() {
        let cwd = Path::new("/repo");
        // 面板里的规则来自引擎（`path:generated` 已归一化为 `/repo/generated`）。
        let rule = parse_shorthand_rule_value("path:generated", RuleDecision::Allow, cwd)
            .expect("normalized rule");
        let mut permissions = serde_json::json!({
            "allow": ["path:generated", "command:cargo test"],
        });
        assert!(remove_rule_value_in_place(&mut permissions, &rule, cwd));
        assert_eq!(
            permissions,
            serde_json::json!({"allow": ["command:cargo test"]}),
            "the shorthand entry must be removed from settings"
        );
    }
}
