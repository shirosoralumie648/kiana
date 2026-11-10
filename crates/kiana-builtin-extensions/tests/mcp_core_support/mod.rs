//! `mcp_config.rs` / `mcp_tools.rs` 共用的测试支持。
//!
//! 对应 TS `test/mcp-extension.test.ts` 的 `setup(global, project)` 与几个内容块构造器。

#![allow(dead_code)] // 每个测试二进制只用到其中一部分。

use std::fs;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use kiana_builtin_extensions::mcp::config::{
    LoadMcpConfigOptions, McpServerConfig, McpServerEntry, McpServerScope,
};
use kiana_builtin_extensions::mcp::resources::McpResourceServer;
use kiana_mcp::McpRequestOptions;
use kiana_mcp::auth_provider::McpTransportError;
use kiana_mcp::protocol_types::{
    BlobResourceContents, EmbeddedResourceContent, EmbeddedResourceKind,
    ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
    ResourceContents, ResourceLinkContent, ResourceLinkKind, ResourceTemplate,
    TextResourceContents,
};
use serde_json::Value;
use tempfile::TempDir;

/// TS `setup(global, project)` 的目录布局。
pub struct Paths {
    _root: TempDir,
    pub agent_dir: PathBuf,
    pub cwd: PathBuf,
}

impl Paths {
    /// `<agentDir>/mcp.json`。
    pub fn global(&self) -> PathBuf {
        self.agent_dir.join("mcp.json")
    }

    /// `<cwd>/.kiana/mcp.json`。
    pub fn project(&self) -> PathBuf {
        self.cwd.join(".kiana").join("mcp.json")
    }

    /// [`load_mcp_config`] 的参数。
    pub fn options(&self, project_trusted: bool) -> LoadMcpConfigOptions {
        LoadMcpConfigOptions {
            agent_dir: self.agent_dir.clone(),
            cwd: self.cwd.clone(),
            project_trusted,
        }
    }
}

/// 建临时目录、写 `<agentDir>/mcp.json` 与 `<cwd>/.kiana/mcp.json`（紧凑 JSON）。
pub fn setup(global: &Value, project: &Value) -> Paths {
    let root = tempfile::tempdir().expect("temp dir");
    let agent_dir = root.path().join("agent");
    let cwd = root.path().join("project");
    fs::create_dir_all(&agent_dir).expect("agent dir");
    fs::create_dir_all(cwd.join(".kiana")).expect("project .kiana dir");
    write_json(&agent_dir.join("mcp.json"), global);
    write_json(&cwd.join(".kiana").join("mcp.json"), project);
    Paths {
        _root: root,
        agent_dir,
        cwd,
    }
}

/// `writeFileSync(path, JSON.stringify(value))`。
pub fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_string(value).expect("serialize json")).expect("write json");
}

/// `JSON.parse(readFileSync(path, "utf8"))`。
pub fn read_json(path: &Path) -> Value {
    let text = fs::read_to_string(path).expect("read json file");
    serde_json::from_str(&text).expect("parse json file")
}

/// 读原始文本（逐字节断言用）。
pub fn read_text(path: &Path) -> String {
    fs::read_to_string(path).expect("read text file")
}

/// 配置的 JS 对象视图。
pub fn config_json(config: &McpServerConfig) -> Value {
    serde_json::to_value(config).expect("serialize config")
}

/// `servers.map(({ name, scope, config }) => [name, scope, config])`。
pub fn entry_tuples(entries: &[McpServerEntry]) -> Vec<(String, Option<McpServerScope>, Value)> {
    entries
        .iter()
        .map(|entry| (entry.name.clone(), entry.scope, config_json(&entry.config)))
        .collect()
}

/// `servers.map(server => [server.name, server.scope])`。
pub fn entry_scopes(entries: &[McpServerEntry]) -> Vec<(String, Option<McpServerScope>)> {
    entries
        .iter()
        .map(|entry| (entry.name.clone(), entry.scope))
        .collect()
}

/// `servers.map(server => server.name)`。
pub fn entry_names(entries: &[McpServerEntry]) -> Vec<String> {
    entries.iter().map(|entry| entry.name.clone()).collect()
}

/// 用一个 `mcpServers` 形状的值校验配置（测试里构造期望值用）。
pub fn validate(name: &str, value: &Value) -> McpServerConfig {
    kiana_core::mcp_servers::validate_mcp_server_config(name, value)
        .unwrap_or_else(|error| panic!("expected valid config for {name}: {error}"))
}

/// 一个 text 资源块。
pub fn text_resource(uri: &str, text: &str) -> ResourceContents {
    ResourceContents::Text(TextResourceContents {
        uri: uri.to_string(),
        mime_type: None,
        text: text.to_string(),
        meta: None,
        extra: serde_json::Map::new(),
    })
}

/// 一个 blob 资源块。
pub fn blob_resource(uri: &str, mime_type: Option<&str>, blob: &str) -> ResourceContents {
    ResourceContents::Blob(BlobResourceContents {
        uri: uri.to_string(),
        mime_type: mime_type.map(str::to_string),
        blob: blob.to_string(),
        meta: None,
        extra: serde_json::Map::new(),
    })
}

/// `{ type: "resource", resource }` 内容块。
pub fn embedded(resource: ResourceContents) -> kiana_mcp::protocol_types::ContentBlock {
    kiana_mcp::protocol_types::ContentBlock::EmbeddedResource(EmbeddedResourceContent {
        kind: EmbeddedResourceKind::Resource,
        resource,
        annotations: None,
        meta: None,
        extra: serde_json::Map::new(),
    })
}

/// `{ type: "resource_link", ... }` 内容块。
pub fn resource_link(
    uri: &str,
    name: &str,
    title: Option<&str>,
    description: Option<&str>,
    mime_type: Option<&str>,
    size: Option<f64>,
) -> kiana_mcp::protocol_types::ContentBlock {
    kiana_mcp::protocol_types::ContentBlock::ResourceLink(ResourceLinkContent {
        kind: ResourceLinkKind::ResourceLink,
        uri: uri.to_string(),
        name: name.to_string(),
        title: title.map(str::to_string),
        description: description.map(str::to_string),
        mime_type: mime_type.map(str::to_string),
        size,
        annotations: None,
        meta: None,
        extra: serde_json::Map::new(),
    })
}

/// 一个列出的资源。
pub fn resource(uri: &str, name: &str) -> Resource {
    Resource {
        uri: uri.to_string(),
        name: name.to_string(),
        ..Resource::default()
    }
}

/// 一个列出的资源模板。
pub fn template(uri_template: &str, name: &str) -> ResourceTemplate {
    ResourceTemplate {
        uri_template: uri_template.to_string(),
        name: name.to_string(),
        ..ResourceTemplate::default()
    }
}

/// 供资源工具测试使用的假服务器。
#[derive(Default)]
pub struct FakeResourceServer {
    pub name: String,
    pub timeout_ms: f64,
    pub resources: Vec<Resource>,
    pub templates: Vec<ResourceTemplate>,
    pub contents: Vec<ResourceContents>,
    pub fail: bool,
}

#[async_trait]
impl McpResourceServer for FakeResourceServer {
    fn name(&self) -> &str {
        &self.name
    }

    fn timeout_ms(&self) -> f64 {
        self.timeout_ms
    }

    async fn resources_page(
        &self,
        cursor: Option<&str>,
        _options: &McpRequestOptions,
    ) -> Result<ListResourcesResult, McpTransportError> {
        if self.fail {
            return Err(McpTransportError::new("boom"));
        }
        Ok(ListResourcesResult {
            resources: self.resources.clone(),
            next_cursor: cursor.is_none().then(|| "next".to_string()),
            ..ListResourcesResult::default()
        })
    }

    async fn resource_templates_page(
        &self,
        cursor: Option<&str>,
        _options: &McpRequestOptions,
    ) -> Result<ListResourceTemplatesResult, McpTransportError> {
        if self.fail {
            return Err(McpTransportError::new("boom"));
        }
        Ok(ListResourceTemplatesResult {
            resource_templates: self.templates.clone(),
            next_cursor: cursor.is_none().then(|| "next".to_string()),
            ..ListResourceTemplatesResult::default()
        })
    }

    async fn all_resources(
        &self,
        _options: &McpRequestOptions,
    ) -> Result<Vec<Resource>, McpTransportError> {
        if self.fail {
            return Err(McpTransportError::new("boom"));
        }
        Ok(self.resources.clone())
    }

    async fn all_resource_templates(
        &self,
        _options: &McpRequestOptions,
    ) -> Result<Vec<ResourceTemplate>, McpTransportError> {
        if self.fail {
            return Err(McpTransportError::new("boom"));
        }
        Ok(self.templates.clone())
    }

    async fn read_resource(
        &self,
        _uri: &str,
        _options: &McpRequestOptions,
    ) -> Result<ReadResourceResult, McpTransportError> {
        if self.fail {
            return Err(McpTransportError::new("boom"));
        }
        Ok(ReadResourceResult {
            contents: self.contents.clone(),
            ..ReadResourceResult::default()
        })
    }
}
