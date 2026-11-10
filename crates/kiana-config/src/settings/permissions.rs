//! 权限规则的配置文件读写。
//!
//! `SettingsManager` 只认识 global 与 project 两个作用域；「总是允许」还需要一个
//! **项目本地**作用域（`.kiana/settings.local.json`，通常被 `.gitignore` 忽略）。
//! 本模块提供最小、可测试的文件操作：
//!
//! - [`read_permissions_file`]：读取某个 settings 文件里的 `permissions` 对象；
//! - [`append_permission_rule`]：只往 `permissions.rules` 追加一条规则，其它字段与
//!   键序原样保留（`serde_json` 开启 `preserve_order`，`kiana_json` 负责
//!   `JSON.stringify(value, null, 2)` 的字节形态）；
//! - [`merge_permission_settings`]：启动时把 project settings 与 local settings 的
//!   `permissions` 合并（规则/allow/deny/addDirs 取并集，`mode` 以 local 为准）。
//!
//! 写文件前做「读现有 JSON → 修改目标字段 → 整体写回」，不会用内存快照覆盖外部
//! 编辑；文件损坏或根不是对象时返回错误，绝不静默重写。项目未受信时是否允许写入由
//! 调用方（`AgentSession`）决定，这里只处理文件本身。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::settings::{PROJECT_NOT_TRUSTED_WRITE_MESSAGE, SettingsValueError};

/// 项目 settings 文件（`<cwd>/.kiana/settings.json`）。
pub fn project_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(kiana_core::config::CONFIG_DIR_NAME)
        .join("settings.json")
}

/// 项目本地 settings 文件（`<cwd>/.kiana/settings.local.json`）。
pub fn local_settings_path(cwd: &Path) -> PathBuf {
    cwd.join(kiana_core::config::CONFIG_DIR_NAME)
        .join("settings.local.json")
}

/// 读取 settings 文件里的 `permissions` 对象；文件不存在或没有该键时返回 `None`。
///
/// 文件存在但不是合法 JSON、根不是对象、或 `permissions` 不是对象时返回错误：
/// 调用方应把错误展示给用户，而不是当成「没有配置」继续。
pub fn read_permissions_file(path: &Path) -> Result<Option<Value>, SettingsValueError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(SettingsValueError::new(format!(
                "Failed to read {}: {error}",
                path.display()
            )));
        }
    };
    if text.trim().is_empty() {
        return Ok(None);
    }
    let root: Value = serde_json::from_str(&text).map_err(|error| {
        SettingsValueError::new(format!("Invalid JSON in {}: {error}", path.display()))
    })?;
    let object = root.as_object().ok_or_else(|| {
        SettingsValueError::new(format!(
            "{} must contain a JSON object at the top level",
            path.display()
        ))
    })?;
    match object.get("permissions") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(permissions)) => Ok(Some(Value::Object(permissions.clone()))),
        Some(_) => Err(SettingsValueError::new(format!(
            "{}.permissions must be a JSON object",
            path.display()
        ))),
    }
}

/// 往 settings 文件的 `permissions.rules` 追加一条规则。
///
/// 已存在完全相同的规则时是空操作。文件/字段不存在时创建；`permissions` 或 `rules`
/// 类型不对时返回错误且不写盘。
pub fn append_permission_rule(path: &Path, rule: &Value) -> Result<(), SettingsValueError> {
    let mut root = read_root_or_empty(path)?;
    let root_object = root.as_object_mut().ok_or_else(|| {
        SettingsValueError::new(format!(
            "{} must contain a JSON object at the top level",
            path.display()
        ))
    })?;
    let permissions = root_object
        .entry("permissions".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    // `{"permissions": null}` 与「没有配置」等价（见 `read_permissions_file`）。
    if permissions.is_null() {
        *permissions = Value::Object(Map::new());
    }
    append_rule_to_permissions(permissions, rule)?;
    write_root(path, &root)
}

/// 读改写某个 settings 文件里的 `permissions` 对象（`/permissions` 面板的 Local 作用域）。
///
/// `update` 拿到可变的 `permissions` 对象，返回 `true` 表示有改动才写盘；返回 `false`
/// 时文件保持原样。文件不存在时按“空对象”处理并只创建所需字段；文件损坏、根不是对象
/// 或 `permissions` 不是对象时返回错误且不写盘。
pub fn update_permissions_file(
    path: &Path,
    update: impl FnOnce(&mut Value) -> Result<bool, SettingsValueError>,
) -> Result<bool, SettingsValueError> {
    let mut root = read_root_or_empty(path)?;
    let root_object = root.as_object_mut().ok_or_else(|| {
        SettingsValueError::new(format!(
            "{} must contain a JSON object at the top level",
            path.display()
        ))
    })?;
    let permissions = root_object
        .entry("permissions".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if permissions.is_null() {
        *permissions = Value::Object(Map::new());
    }
    let changed = update(permissions)?;
    if changed {
        write_root(path, &root)?;
    }
    Ok(changed)
}

/// 在已有的 `permissions` JSON 对象上追加规则（内存版，供 SettingsManager 写回使用）。
pub fn append_rule_to_permissions(
    permissions: &mut Value,
    rule: &Value,
) -> Result<(), SettingsValueError> {
    if permissions.is_null() {
        *permissions = Value::Object(Map::new());
    }
    let permissions_object = permissions
        .as_object_mut()
        .ok_or_else(|| SettingsValueError::new("permissions must be a JSON object".to_string()))?;
    let rules = permissions_object
        .entry("rules".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let rules_array = rules
        .as_array_mut()
        .ok_or_else(|| SettingsValueError::new("permissions.rules must be an array".to_string()))?;
    if !rules_array.iter().any(|existing| existing == rule) {
        rules_array.push(rule.clone());
    }
    Ok(())
}

/// 合并 project/base 与 local 的 `permissions` 对象。
///
/// - `rules`、`allow`、`deny`、`addDirs`、`workspaceRoots`：base 在前、local 在后，
///   去重后取并集；
/// - `mode`：local 有值时覆盖 base；
/// - 其它顶层键：local 覆盖 base。
///
/// 任一侧的 `permissions` 不是对象时，该侧按「没有配置」处理（启动路径不应因为一个
/// 坏字段把权限层整个关掉；写回路径会单独报错）。
pub fn merge_permission_settings(base: Option<&Value>, local: Option<&Value>) -> Option<Value> {
    let base_object = base.and_then(Value::as_object);
    let local_object = local.and_then(Value::as_object);
    if base_object.is_none() {
        return local.cloned();
    }
    if local_object.is_none() {
        return base.cloned();
    }
    let base_object = base_object.expect("checked above");
    let local_object = local_object.expect("checked above");
    let mut merged = base_object.clone();
    // 数组型字段取并集（顺序：base 现有顺序 + local 新增项）。
    for key in ["rules", "allow", "deny", "addDirs", "workspaceRoots"] {
        let base_items = merged.get(key).and_then(Value::as_array).cloned();
        let local_items = local_object.get(key).and_then(Value::as_array).cloned();
        let Some(local_items) = local_items else {
            continue;
        };
        let mut combined = base_items.unwrap_or_default();
        for item in local_items {
            if !combined.contains(&item) {
                combined.push(item);
            }
        }
        merged.insert(key.to_string(), Value::Array(combined));
    }
    // 其余键（含 mode）local 覆盖 base；上面的数组键已经单独处理过。
    for (key, value) in local_object {
        if ["rules", "allow", "deny", "addDirs", "workspaceRoots"].contains(&key.as_str()) {
            continue;
        }
        // `null` 表示「没有值」，不能覆盖 base 的有效配置（`{"mode": null}` 不应
        // 关掉项目里的 read-only）。
        if value.is_null() {
            continue;
        }
        merged.insert(key.clone(), value.clone());
    }
    Some(Value::Object(merged))
}

fn read_root_or_empty(path: &Path) -> Result<Value, SettingsValueError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Map::new()));
        }
        Err(error) => {
            return Err(SettingsValueError::new(format!(
                "Failed to read {}: {error}",
                path.display()
            )));
        }
    };
    if text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let root: Value = serde_json::from_str(&text).map_err(|error| {
        SettingsValueError::new(format!("Invalid JSON in {}: {error}", path.display()))
    })?;
    if !root.is_object() {
        return Err(SettingsValueError::new(format!(
            "{} must contain a JSON object at the top level",
            path.display()
        )));
    }
    Ok(root)
}

/// 拒绝通过符号链接写 settings。
///
/// 克隆下来的仓库可以把 `.kiana/settings.local.json`（或 `.kiana` 目录本身）做成指向
/// `~/.kiana/...` 的链接；若无条件写回，用户的一次「本机允许」就会把规则注入全局设置，
/// 把项目作用域悄悄升级成所有项目。
fn reject_symlink(path: &Path) -> Result<(), SettingsValueError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(SettingsValueError::new(format!(
            "Refusing to write {}: it is a symbolic link",
            path.display()
        ))),
        _ => Ok(()),
    }
}

fn write_root(path: &Path, root: &Value) -> Result<(), SettingsValueError> {
    reject_symlink(path)?;
    if let Some(parent) = path.parent() {
        reject_symlink(parent)?;
        std::fs::create_dir_all(parent).map_err(|error| {
            SettingsValueError::new(format!("Failed to create {}: {error}", parent.display()))
        })?;
    }
    let text = kiana_json::js_object::stringify_pretty(root, 2);
    // Unix 上再用 `O_NOFOLLOW` 关掉检查与写入之间的竞态；其它平台保留检查结果。
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(libc::O_NOFOLLOW)
            .mode(0o600)
            .open(path)
            .map_err(|error| {
                SettingsValueError::new(format!("Failed to write {}: {error}", path.display()))
            })?;
        file.write_all(text.as_bytes()).map_err(|error| {
            SettingsValueError::new(format!("Failed to write {}: {error}", path.display()))
        })
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, text).map_err(|error| {
            SettingsValueError::new(format!("Failed to write {}: {error}", path.display()))
        })
    }
}

/// 项目未受信时的统一错误（供调用方判定）。
pub fn project_not_trusted_error() -> SettingsValueError {
    SettingsValueError::new(PROJECT_NOT_TRUSTED_WRITE_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn append_preserves_other_fields_and_key_order() {
        let dir = tempfile::tempdir().expect("temp");
        let path = local_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(
            &path,
            "{\n  \"theme\": \"dark\",\n  \"permissions\": {\n    \"mode\": \"default\",\n    \"rules\": [{\"tool\": \"read\", \"decision\": \"allow\"}]\n  },\n  \"lastChangelogVersion\": \"1.0.0\"\n}",
        )
        .expect("write");

        append_permission_rule(
            &path,
            &json!({"command": ["git", "status"], "decision": "allow"}),
        )
        .expect("append");

        let text = std::fs::read_to_string(&path).expect("read");
        let theme_index = text.find("\"theme\"").expect("theme");
        let permissions_index = text.find("\"permissions\"").expect("permissions");
        let changelog_index = text.find("\"lastChangelogVersion\"").expect("changelog");
        assert!(theme_index < permissions_index && permissions_index < changelog_index);
        let value: Value = serde_json::from_str(&text).expect("json");
        let rules = value["permissions"]["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(value["permissions"]["mode"], json!("default"));
    }

    #[test]
    fn append_is_idempotent_for_identical_rules() {
        let dir = tempfile::tempdir().expect("temp");
        let path = local_settings_path(dir.path());
        let rule = json!({"tool": "bash", "decision": "allow"});
        append_permission_rule(&path, &rule).expect("first");
        append_permission_rule(&path, &rule).expect("second");
        let value = read_permissions_file(&path)
            .expect("read")
            .expect("permissions");
        assert_eq!(value["rules"].as_array().expect("rules").len(), 1);
    }

    #[test]
    fn merge_unions_rules_and_lets_local_mode_win() {
        let base = json!({
            "mode": "default",
            "rules": [{"tool": "read", "decision": "allow"}]
        });
        let local = json!({
            "mode": "accept-edits",
            "rules": [
                {"tool": "read", "decision": "allow"},
                {"command": ["git", "status"], "decision": "allow"}
            ],
            "addDirs": ["/tmp/extra"]
        });
        let merged = merge_permission_settings(Some(&base), Some(&local)).expect("merged");
        assert_eq!(merged["mode"], json!("accept-edits"));
        assert_eq!(merged["rules"].as_array().expect("rules").len(), 2);
        assert_eq!(merged["addDirs"], json!(["/tmp/extra"]));
    }

    #[test]
    fn local_null_values_do_not_override_base_settings() {
        let base = json!({"mode": "read-only"});
        let local = json!({"mode": null});
        let merged = merge_permission_settings(Some(&base), Some(&local)).expect("merged");
        assert_eq!(merged["mode"], json!("read-only"));
    }

    #[test]
    fn null_permissions_is_treated_as_absent_when_appending() {
        let dir = tempfile::tempdir().expect("temp");
        let path = local_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, r#"{"permissions": null}"#).expect("write");
        append_permission_rule(&path, &json!({"tool": "bash"})).expect("append");
        let root: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(
            root["permissions"]["rules"]
                .as_array()
                .expect("rules")
                .len(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_settings_files_are_not_followed() {
        let dir = tempfile::tempdir().expect("temp");
        let global = dir.path().join("global.json");
        std::fs::write(&global, r#"{"permissions": {"rules": []}}"#).expect("write");
        let path = local_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::os::unix::fs::symlink(&global, &path).expect("symlink");

        assert!(
            append_permission_rule(&path, &json!({"tool": "bash"})).is_err(),
            "符号链接必须被拒绝，否则会把规则写进链接目标"
        );
        let untouched = std::fs::read_to_string(&global).expect("read");
        assert!(
            !untouched.contains("bash"),
            "链接目标（全局设置）不能被修改：{untouched}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_directory_is_not_followed() {
        let dir = tempfile::tempdir().expect("temp");
        let outside = tempfile::tempdir().expect("outside");
        std::os::unix::fs::symlink(outside.path(), dir.path().join(".kiana")).expect("symlink");
        let path = local_settings_path(dir.path());
        assert!(append_permission_rule(&path, &json!({"tool": "bash"})).is_err());
        assert!(
            !outside.path().join("settings.local.json").exists(),
            "不能通过 .kiana 目录链接写到项目外"
        );
    }

    #[test]
    fn invalid_json_is_an_error_instead_of_overwriting() {
        let dir = tempfile::tempdir().expect("temp");
        let path = local_settings_path(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "{not json").expect("write");
        assert!(append_permission_rule(&path, &json!({"tool": "bash"})).is_err());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{not json");
    }
}
