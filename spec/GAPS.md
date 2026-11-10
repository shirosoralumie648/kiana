# 阶段 1 完整性审计：未覆盖 / 未决事项

本文件是 `/home/ctyun/code/kiana_rev/kiana`（TypeScript）→ `/home/ctyun/code/kiana_rev/kiana` 第一阶段的完整性审计结果。
已直接修好的小问题记在文末 §A；这里逐条列出**需要架构者/各单元 owner 决策**的较大缺口，每条给出涉及的
源文件与规范文件、影响与建议落点。所有路径以 `kiana/`（TS 只读参考）或 `kiana/` 为根。

分类：

- **G1 依赖缺失**：spec 要求的行为在骨架 crate 里没有可用的依赖（`Cargo.toml` 无 pin）。按 `ARCHITECTURE.md` §7.0 规则 2，
  加依赖只能由架构者改 `[workspace.dependencies]`。
- **G2 构建期产物**：`build.rs`、生成的静态表、`include_str!` 资源在骨架里不存在或未接线。
- **G3 未分配单元**：源文件在 28 份 spec / `ARCHITECTURE.md` 里没有明确 owner，或明确说“保留 TS”但没有决策。
- **G4 规范互相矛盾**：两份规范对同一符号/布局给出不同结论，且不能靠 §3.8 自动仲裁。
- **G5 版本与发行**：`VERSION`/`KIANA_AI_VERSION` 的取值来源、npm 包是否继续发布（与 G3.4 的构建/CI 范围相关）。

**所有影响阶段 0–1 crate 的条目已在下面的 §0 决策总表里给出结论**（2026-10-26，由架构者决策并落地）；
各条目正文末尾也追加了「结论（已决 / decided）」段落。仍待后续阶段 owner 决策的条目见 §0 末尾的
「不在阶段 0–1 范围」小节。

---

## §0 决策总表（2026-10-26，架构者已决 / decided）

所有影响阶段 0–1 crate 的条目已在下表给出结论；详细理由写在 `ARCHITECTURE.md` §8 的对应条目里
（下表最后一列）。改动已落地在代码/`Cargo.toml`，实现者直接照此编码，不需要再等决策。

| 条目 | 结论 | ARCH §8 |
|---|---|---|
| G1.1 | `kiana-core` 补 `reqwest`/`base64`/`flate2`/`crc32fast` + feature `images`；**不引入 `arboard`** | 第 16 条 |
| G1.2 | **方案 (a)**：`kiana-agent-core` 直接加 `reqwest`，`stream_proxy` 保持 TS 的直连签名 | 第 18 条 |
| G1.3 | `kiana-ai` 加 `reqwest`；`src/http.rs` → `src/http/{mod,reqwest_client}.rs`，契约冻结 | 第 19 条 |
| G1.4 | `kiana-env-client` 补 `sha2`/`base64`/`hex`/`getrandom`/`url`；`utf16le` 手写 | 第 28 条 |
| G1.5 | **方案 (a)**：原生层在 `kiana-tui/src/native/**`；feature `x11`/`darwin`/`win32`/`native`；trait 进 §4.9 | 第 17 条 |
| G1.6 | 根 `Cargo.toml` 已有全部 pin，各 owner 在自己阶段写 `{ workspace = true }`，**不需要**再找架构者 | 第 29 条 |
| G2.1 | 模型目录 **vendored** 到 `crates/kiana-ai/data/`；`KIANA_MODEL_DATA_DIR` → 仓库内目录 → panic | 第 5、21 条 |
| G2.2 | codemode：`wasm`（wasmtime + 同一份 `quickjs.wasm`）优先，`quickjs` 备选；prelude 已导出并校验哈希 | 第 22 条 |
| G2.4 | Photon：原生 `photon-rs 0.3.3`，JPEG 逐字节一致、PNG 允许末位舍入；**不引入 wasm 运行时** | 第 20 条 |
| G4.3 | `node:path` 唯一实现在 `kiana-durable::env::path::{posix,win32}`，`kiana-env-client` 复用 | 第 23 条 |
| G5.1 | `KIANA_AI_VERSION` / `VERSION` = **字面量 `"1.4.0"`**（不读 `CARGO_PKG_VERSION`、不做构建期注入） | 第 24 条 |
| G5.2 | M1 **不发 npm 包**，只发二进制；`package-distribution.test.ts` 不移植 | 第 25 条 |
| （平台） | Windows 分支按 `cfg(windows)` 移植，CI/验收目标 linux + macOS | 第 26 条 |
| （locale） | en-US 为基线（无 locale 数据时的唯一路径）；ICU4X `locale-data` feature 阶段 5 末随 `runtime` 进 default | 第 27 条 |

新增 pin（全部 `cargo info` 校验 + `cargo check` 解析）：`hmac =0.13.0`、`zstd =0.14.3`、
`photon-rs =0.3.3`、`x11rb =0.14.0`、`objc2 =0.6.5`、`objc2-foundation =0.3.2`、
`objc2-app-kit =0.3.2`；`tokio-tungstenite` 补 features，`windows-sys` 补 8 个 feature。
**没有**新增任何 spec 未提到的 crate，也没有放宽任何已有 pin。

### 不在阶段 0–1 范围（本例未决策，留给对应阶段的 owner）

下列条目涉及的 crate 都在阶段 2 之后，不影响阶段 0–1 开工；到各自阶段开工前由该单元的 owner
按同样的格式补决策（写进 `ARCHITECTURE.md` §8 并在本文件记「已决」）：

- **G2.3** 打包资源（主题、HTML 导出模板、`clankolas.png`、docs/examples/schemas/README）——
  `kiana-theme`（阶段 3）、`kiana-export`（阶段 4）、`kiana-interactive`（阶段 7）。建议方向已写在 §G2.3：
  `include_bytes!` 嵌入 + `KIANA_PACKAGE_DIR` 覆盖开发期。
- **G3.1** examples 语料清单 —— `spec/ca-extensions.md`（`kiana-extensions`，阶段 5）。
- **G3.2** docs 的 6 个未被引用的文件 —— 随 G2.3 一起处理（阶段 8 的 `getDocsPath()`）。
- **G3.3** 7 个未在规范中出现的测试/夹具文件 —— 归 `kiana-ai`（4 个 provider 重试 regression）与
  `kiana-interactive`（`plan-mode-utils`/`test-harness`/`streaming-render-debug`/`test-theme-colors`）；
  各 spec 的 §7 清单由 owner 补齐。
- **G3.4** `scripts/`、CI、发布工具链 —— 阶段 8（G5.2 已定“不发 npm”，但产物矩阵与 pin 检查脚本仍未定）。
- **G3.5** `migrate-sessions.sh` 与 `install-lock/` —— 阶段 8，建议两条都“不移植”。
- **G4.1** `spec/ca-config-models.md` 正文里的旧 `kiana-coding-agent` 措辞 —— `kiana-config` owner 全文再扫。
- **G4.2** `LoadExtensionsResult` 的最终落点 —— ca-extensions owner 确认（建议 `kiana-ext-api`）。
- **G4.4** `kiana-ext-api` 与 `kiana-extensions` 的 `types/` 逐类型归属 —— 阶段 4 开工前由两方共同列清单。
- **G4.5** `kiana update` 自更新执行器的分工（`kiana-core::config` 检测 vs `kiana-resources::pkg_cli` 执行）——
  阶段 2/5；`docs/windows.md`/`termux.md` 的语义并入时一并处理。

---

## G1 依赖缺失（需要架构者补 pin 或确认替代方案）

### G1.1 `kiana-core` 缺 HTTP / 图片 / ZIP / base64 依赖

`crates/kiana-core/Cargo.toml` 当前只有 serde/tokio/regex/glob/semver/chrono/mime 等，但它的模块树要求：

| 模块（骨架文件） | 需要的依赖 | TS 依据 |
|---|---|---|
| `management_http.rs` | `reqwest`（`redirect: manual`、AbortSignal 预算） | `coding-agent/src/utils/management-http.ts`（`fetch`、超时跨尝试共享、重试） |
| `http_dispatcher.rs` | `reqwest`（`ClientBuilder`：`read_timeout`、`Proxy::all`、`happy_eyeballs_timeout`） | `coding-agent/src/core/http-dispatcher.ts`（undici `EnvHttpProxyAgent`） |
| `image_process.rs` | `image`、`kamadak-exif`（EXIF 方向）、`base64`、Photron 等价的 resize/encode | `utils/{image-process,image-convert,image-resize-core,image-resize,image-resize-worker,exif-orientation,photon,clipboard-image,tool-result-images}.ts` |
| `clipboard.rs` / `config.rs` | `base64`（OSC 52、data URL）、`arboard` 或平台原生剪贴板 | `utils/{clipboard,clipboard-command}.ts`、`config.ts` 的 data URL 读取 |
| `zip.rs` | `flate2` + `crc32fast`（`deflateRawSync`、`crc32`） | `utils/zip.ts`（72 行） |

规范侧：`spec/ca-cli.md` §6.1/§6.2 明确要求 `reqwest`/`flate2`/`crc32fast`/`photon-node` 等价物；
`spec/ca-config-models.md` §2.12 与 §8.1 的 `http_dispatcher.rs` 注释写明是 `reqwest` Client 构建。
`ARCHITECTURE.md` §3.3 说 “`http-dispatcher.ts` 留在 `kiana-core`”，但没有说明 `kiana-core` 为何没有 reqwest。

影响：阶段 2 一开工就撞墙；`kiana-ai`/`kiana-config` 都要从 `kiana-core` 拿 HTTP 客户端装配（见 G1.2 的契约问题）。

建议：架构者在根 `Cargo.toml` 已有 pin（`reqwest 0.13.5`、`flate2 1.1.10`、`crc32fast 1.5.0`、`image 0.25.10`、
`kamadak-exif 0.6.1`、`base64 0.23.1`），只需把它们加进 `kiana-core` 的 `[dependencies]`。
`arboard`/`x11rb`/`objc2-app-kit` 尚未 pin（见 G1.5）。

**结论（已决 / decided，ARCH §8 第 16 条）**：已按上表补齐 —— `crates/kiana-core/Cargo.toml` 现有
`reqwest`/`base64`/`flate2`/`crc32fast`，并新增 feature `images = [photon-rs, image, kamadak-exif]`
（阶段 2 开工时进 default）。**不引入 `arboard`**：`clipboard.rs` 的 OS 剪贴板走
`kiana-tui::native`（G1.5）与 OSC 52，`base64` 只用于 OSC 52/data URL。

### G1.2 `kiana-agent-core` 缺 `reqwest`（`streamProxy`）

`crates/kiana-agent-core/src/proxy.rs` 对应 `packages/agent/src/proxy.ts`，规范 `spec/agent-codemode.md` §2.1.6
要求在 `POST ${proxyUrl}/api/stream` 上做 SSE 风格逐行读取（`data: ` 前缀、`Authorization: Bearer`），
并且 `spec/agent-codemode.md` §6 的依赖表建议 `reqwest`。骨架 `kiana-agent-core/Cargo.toml` 没有 `reqwest`、
没有 `http`，也没有对应的 `kiana-ai::http::HttpClient` 注入点说明（`streamProxy` 是独立于 provider 的直连）。

影响：`kiana-agent-core` 作为 L2 crate，无法在不新增依赖的前提下实现 `stream_proxy`。

建议：二选一（需要架构者决定并写进 ARCH §3）：
(a) 给 `kiana-agent-core` 加 `reqwest`；或
(b) 把 `stream_proxy` 的 HTTP 传输改为接收 `kiana_ai::http::SharedHttpClient`（`kiana-agent-core` 已依赖 `kiana-ai`），
    这样不新增依赖，但 `streamProxy` 的签名与 TS 不同，需要在 `spec/agent-codemode.md` 显式记录。

**结论（已决 / decided，ARCH §8 第 18 条）：选 (a)**。`crates/kiana-agent-core/Cargo.toml` 已加
`reqwest = { workspace = true }`；`src/proxy.rs` 的 `stream_proxy(model, context, options)` 保持
TS 形状（`ProxyStreamOptions` 带 `signal`/`auth_token`/`proxy_url`），**不**改成
`SharedHttpClient`，因为 TS 的 `proxy.ts` 用全局 `fetch`、与 provider 的 `options.fetch` 注入点无关。

### G1.3 `kiana-ai` 缺 HTTP 实现与 `src/http/` 目录

`crates/kiana-ai/src/http.rs` 的 `//!` 头写明默认实现位于 `crates/kiana-ai/src/http/reqwest_client.rs`（“实施阶段加入”），
但骨架 `crates/kiana-ai/src/` 下没有 `http/` 目录，`Cargo.toml` 也没有 `reqwest` 依赖。所有 4 份 `ai-*.md` 都按
`reqwest + rustls-tls` 设计（`spec/ai-api-other.md` §8、`spec/ai-core.md` §6）。

影响：`kiana-ai` 的 5 个 api 实现（`api/*.rs`）没有可用的默认 `HttpClient`；`spec/ai-api-other.md` 说
“默认实现是 reqwest”，但没有 crate 承载。

建议：架构者给 `kiana-ai` 加 `reqwest`（含 `stream` feature），并新建 `src/http/{mod.rs,reqwest_client.rs}`；
同时确认 §4.9 的 `HttpClient` trait 是否需要 `reqwest::Error` 的转换（错误文本要与 TS 一致，
见 `spec/ca-builtin-extensions.md` §6.3 对 `reqwest::Error` 文本的要求）。

**结论（已决 / decided，ARCH §8 第 19 条）**：已加 `reqwest`（含 `stream`），并新建
`crates/kiana-ai/src/http/{mod.rs,reqwest_client.rs}`（`src/http.rs` 已删除）。`mod.rs` 契约冻结、
进 ARCH §4.9；`reqwest_client.rs` 由 ai-team 实现。`HttpError` 的 `message` 必须转写成 TS
`error.message` 的逐字文本，`status` 只在“头已到、读体失败”时出现；重试留在 provider 层。

### G1.4 `kiana-env-client` 缺 `sha2` / `base64`（SSH 部署）

`packages/env/src/ssh.ts` 用 `createHash("sha256")` 校验上传的 daemon（`ssh.ts:412`）、用
`Buffer.from(script,"utf16le").toString("base64")` 组 PowerShell `-EncodedCommand`（`ssh.ts:292`）、
用 `bytes.toString("base64").replace(/.{1,76}/g,"$&\n")` 分块传二进制（`ssh.ts:439`）。
规范 `spec/env-evals-durable-testing.md` §2.3/§8.3 记录了这些语义，但 `crates/kiana-env-client/Cargo.toml`
没有 `sha2`/`base64`。

影响：`deployDaemon` 的完整性校验与 Windows `EncodedCommand` 无法实现。

建议：给 `kiana-env-client` 加 `sha2` + `base64`（根 `Cargo.toml` 已有 pin）。另外 `utf16le` 编码需要手写
（`base64` crate 不含 UTF-16 转换），规范里应写明。

**结论（已决 / decided，ARCH §8 第 28 条）**：`crates/kiana-env-client/Cargo.toml` 已加
`sha2`/`base64`/`hex`/`getrandom`/`url`。`utf16le` → base64（`-EncodedCommand`）与
`base64` 的 76 列分块都在 `ssh.rs` 手写，注释里写明 TS 行号。

### G1.5 `kiana-tui` 原生平台层无契约、无 pin

`spec/tui-core.md` §2.16/§2.17 定义 `NativeClipboard`（`getText`/`setText`/`getImage`）与
`getNativeClipboard()`，并要求 Linux 私有 3 s 线程 + 2000 ms XCB 超时、Darwin AppKit、Win32。骨架：

- `crates/kiana-tui/src/image.rs`、`keys.rs` 只是合并后的空模块，**没有** `NativeClipboard` trait；
- `crates/kiana-tui/Cargo.toml` 没有 `x11rb`/`objc2-app-kit`/`windows`/`arboard` 任何 pin；
- `crates/kiana-core/src/clipboard.rs`（L3）要调用 `getNativeClipboard`，方向是 `kiana-core -> kiana-tui`（ARCH §2 允许），
  但 trait 定义在哪个 crate 未定。

影响：`spec/tui-core.md` §2.16 的验收（三态 “unavailable/empty/transfer failed”、Linux 无 `DISPLAY` 返回
`undefined`）无法开始；`kiana-core`/`kiana-interactive` 的剪贴板路径都依赖它。

建议：架构者决定：
(a) 在 `kiana-tui` 内建 `native/{mod,linux,darwin,win32}.rs` + `#[cfg]` 平台依赖（x11rb/objc2-app-kit/windows-sys）；
(b) 或独立 `kiana-tui-native` crate（`spec/tui-core.md` §2.16 提到的选项）。
无论哪种，`NativeClipboard`/`NativeModifiers` 的 trait 必须放进 `ARCHITECTURE.md` §4.9 共享契约表
（`kiana-core` 与 `kiana-interactive` 都要用），并补 `x11rb` 等 pin。

**结论（已决 / decided，ARCH §8 第 17 条）：选 (a)**。已建 `crates/kiana-tui/src/native/`
（`mod.rs`、`module_path.rs`、`linux_x11.rs`、`darwin.rs`、`win32.rs`），`image.rs`/`keys.rs`
改为再导出；`Cargo.toml` 加 `tokio`/`tokio-util`/`async-trait`/`futures-core` 与
`x11rb`/`objc2`/`objc2-foundation`/`objc2-app-kit`/`windows-sys`（全部 `optional`，按 `cfg` 分区），
feature 为 `default = []`/`x11`/`darwin`/`win32`/`native`。trait 已进 ARCH §4.9；
`tests/native_clipboard.rs` 覆盖三态、`DISPLAY` 每次重检、候选路径顺序。

### G1.6 `kiana-builtin-extensions` / `kiana-resources` / `kiana-config` / `kiana-interactive` 缺 HTTP/crypto

对应 TS 直接使用 `fetch` / `createHash` / `Buffer.from(...,"base64")`：

| crate | 源文件 | 需求 |
|---|---|---|
| `kiana-builtin-extensions` | `extensions/llama/{client,huggingface}.ts`、`extensions/mcp/{oauth,tools}.ts`、`extensions/codemode/execute.ts` | `reqwest`、`sha2`（MCP OAuth PKCE S256）、`base64` |
| `kiana-resources` | `core/bug-report-upload.ts`（multipart 上传）、`core/package-manager.ts`（`createHash`） | `reqwest`（multipart）、`sha2` |
| `kiana-config` | `core/model-runtime.ts`、`core/remote-catalog-provider.ts`（`requestCatalog` 等） | `reqwest`（`models/catalog_http.rs` 已写明） |
| `kiana-interactive` | `modes/interactive/session-share.ts`（会话分享上传）、`interactive-mode.ts` 的安装遥测 / Radius 上传 | `reqwest` |
| `kiana-tools` | 无直接 `fetch`；`core/tools/read.ts` 走图片处理 | 依赖 `kiana-core::image_process`（G1.1） |

`kiana-session` 本身**不**直接 `fetch`（网络由 model-runtime / ai-api 负责，`spec/ca-session.md` §6 也这样写），
所以上表第一节里的 `kiana-session` 已移除。

规范侧：`spec/ca-builtin-extensions.md` §6.3 与 §8.1 的 `Cargo.toml` 示例写了 `reqwest = "0.12"`、
`sha2 = "0.10"`、`base64 = "0.22"`（版本是建议，不是权威，见 ARCH §8 第 13 条）；`spec/ca-resources.md`
§6.3 同样写 `reqwest` multipart。骨架里这些 crate 全都没有 `reqwest`。

影响：内置 llama/MCP/codemode 扩展、Bug 报告上传、模型目录远端刷新、会话分享都无法实现。

建议：架构者按 crate 逐个补 `reqwest`（建议共用同一个 `reqwest::Client`，通过 `kiana-core::http_dispatcher`
构造，避免每个 crate 各自建 Client 导致代理/超时行为不一致）。这条与 G1.1 的 `http_dispatcher` 归属一起决策。

**结论（已决 / decided，ARCH §8 第 29 条）**：不需要新 pin —— 根 `Cargo.toml` 已有
`reqwest`/`sha2`/`base64`/`hex`/`flate2`/`url`/`uuid`/`getrandom`。这 4 个 crate 的 owner
在各自阶段开工时把 `{ workspace = true }` 写进 `Cargo.toml` 即可，**不必**再找架构者；
`reqwest::Client` 统一由 `kiana-core::http_dispatcher` 构造后共享，避免各 crate 各建 Client
导致代理/超时不一致。

---

## G2 构建期产物未接线

### G2.1 `kiana-ai` 缺 `build.rs`（模型目录生成）

`crates/kiana-ai/src/providers/catalog.rs` 的 `//!` 头已写明：由 `build.rs` 读取 `KIANA_MODEL_DATA_DIR` 下的
`providers/data/<id>.json`，生成 `MODELS` / `IMAGE_MODELS` / `CLASSIFIER_MODELS` 三张表，键顺序按
`spec/ai-core.md` §2.3.1 的 42 个 provider。骨架里**没有 `crates/kiana-ai/build.rs`**，也没有 `providers/data/` 目录
（TS 侧该目录是 gitignored 的 hydrate 产物）。

规范侧：`ARCHITECTURE.md` §4.5 与 §8 第 5 条、`spec/ai-core.md` §8.1 都提到 `build.rs`，`spec/ai-core.md`
§8.1 的树里写的是“build.rs # 可选”。`spec/ai-core.md` 没有定义缺数据时的行为（报错文案、是否允许空表）。

影响：`kiana-ai` 阶段 1 无法 `cargo build` 出真实模型表；`spec/ai-core.md` 的 catalog 测试无数据可用。

建议（较大，需要架构者 + ai-team）：
1. 新建 `crates/kiana-ai/build.rs`，读 `KIANA_MODEL_DATA_DIR`，缺目录时报 `cargo:warning` 还是 `panic!` 需明确
   （建议：`KIANA_MODEL_DATA_DIR` 未设置 → 生成空表 + `cargo:warning`；设置了但缺文件 → `panic!`，与 TS 的
   `check-model-data` 门禁等价）；
2. 在 `ARCHITECTURE.md` §4.5 补上：CI 必须先跑 TS 仓库的 `npm run hydrate:model-data`，或把 JSON 提交进 kiana；
3. 明确 42 个 provider 的顺序来源（`spec/ai-core.md` §2.3.1 的列表 vs `providers/all.rs` 的顺序）。

**结论（已决 / decided，ARCH §8 第 5、21 条）**：数据 **vendored**。42 个 provider JSON +
`.manifest.json` 已复制到 `crates/kiana-ai/data/`（1.1 MiB），逐个 sha256 与 manifest 对过；
来源与再生成步骤见 `crates/kiana-ai/data/README.md`。`build.rs` 的目录选择是
`KIANA_MODEL_DATA_DIR` → 仓库内 `crates/kiana-ai/data/` → `panic!`（**不**生成空表）；对每个 JSON 发
`rerun-if-changed`、按 manifest 校验 sha256、键顺序按 `spec/ai-core.md` §2.3.1。
`catalog.rs` 的 `//!` 头已同步为这一契约。

### G2.2 `kiana-codemode` 的 prelude / quickjs-wasi / feature 未接线

- `crates/kiana-codemode/src/runtime/prelude_source.rs` 要求 `include_str!("../../js/prelude.js")`，
  但 `crates/kiana-codemode/js/` **不存在**；`spec/agent-codemode.md` 附录 A 有 17,233 字符的 prelude 全文可用。
- `crates/kiana-codemode/Cargo.toml` **没有 `[features]`**；`cloudflare.rs` 需要 `feature cloudflare`，
  VM 后端需要 quickjs-wasi（via `wasmtime`）或 `rquickjs`，两者都未 pin 进该 crate（根 `Cargo.toml` 有
  `rquickjs 0.14.0`、`wasmtime 50.0.0`）。
- 没有 `build.rs`（若采用 wasm 方案，需要把 `quickjs-wasi.wasm` 编译/嵌入）。

规范侧：`spec/agent-codemode.md` §8.1 说 “`prelude.js` # include_str! 的解析后 prelude”，§8.1 的
Rust 设计要点建议 wasm 方案（推荐）或 rquickjs；feature 只在 §1.3 提了一句 “feature 或子模块（见 §8）”，
§8 没有给出 feature 名清单。`ARCHITECTURE.md` §5 的说明已指向本文件。

影响：codemode 沙箱（`kiana-builtin-extensions` 的 codemode 扩展、`kiana-durable` 的 codemode 步骤）无法实现；
`PRELUDE_HASH`/确定性指纹测试无法开始。

建议：
1. 架构者确定 VM 后端（wasm vs rquickjs），并写入 `spec/agent-codemode.md` §8 的决策记录；
2. 在 `kiana-codemode/Cargo.toml` 加 feature：`default = []`、`wasm`/`native`（或 `quickjs`/`wasmtime`）、
   `cloudflare`，并补对应 pin；
3. 阶段 1 从 `spec/agent-codemode.md` 附录 A 生成 `js/prelude.js` 并加 `include_str!` 测试
   （字节数 17,233、FNV-1a `8c6c9242`）。

**结论（已决 / decided，ARCH §8 第 22 条）**：VM 后端选 **wasm**（wasmtime 只跑一个 core module，
嵌入与 TS 同一份 `quickjs-wasi` 3.6.2 的 `quickjs.wasm`）；`rquickjs` 只作为不实现的备选。
feature 已固定：`default = []`、`wasm = ["dep:wasmtime"]`、`quickjs = ["dep:rquickjs"]`、
`cloudflare = []`（占位，§8 第 3 条）。`js/prelude.js` 已从 `spec/agent-codemode.md` 附录 A
逐字导出（17,233 字节，FNV-1a `8c6c9242` 已核对）。

### G2.3 打包资源没有任何 crate 落点

以下资源在 TS 仓库里随 npm 包分发，Rust 侧需要 `include_str!`/`include_bytes!` 或运行时路径：

| TS 资源 | 规范提到 | Rust 现状 |
|---|---|---|
| `modes/interactive/theme/{dark,light}.json`（156 行） | `kiana-theme/src/assets.rs`（`include_str!` 字节一致） | `assets.rs` 只有 `//!` 头，无 JSON 资源 |
| `core/export-html/{template.html,template.css,template.js,vendor/{marked,marked-cjk-friendly,highlight}.min.js}` | `kiana-export/src/assets.rs` | `assets.rs` 只有 `//!` 头，无模板/vendor 资源 |
| `modes/interactive/assets/clankolas.png`（539,053 字节，sha256 `169acd0d…`） | `spec/ca-interactive-mode.md` §2.16（`loadImageBase64`） | 骨架无文件 |
| `packages/coding-agent/docs/`（42 个 `.md` + `docs.json` + `images/` 4 个 png） | `getDocsPath()`、`documentation.test.ts` | 骨架无文件，且未说明单二进制如何定位 |
| `packages/coding-agent/examples/`（`examples/**` 共 137 个文件，其中 102 个 `.ts`；`extensions/` 下 86 个 `.ts`、119 个文件，含 `doom.wasm`） | 扩展语料、`getExamplesPath()` | 骨架无文件 |
| `packages/coding-agent/schemas/`（4 个 `.json`） | `generate:schemas`、`config-schemas.test.ts` | 骨架无文件 |
| `packages/coding-agent/{README.md,CHANGELOG.md}` | `getReadmePath()`/`getChangelogPath()` | 骨架无文件 |
| `packages/coding-agent/package.json` 的 `files` 字段 | npm 发布清单 | 无对应 |

规范侧：`spec/ca-cli.md` §2.6 定义了 `getPackageDir()`/`getDocsPath()`/`getExamplesPath()` 的**语义**
（`KIANA_PACKAGE_DIR` 覆盖、`isBunBinary` → `dirname(process.execPath)`），但**没有**规定 Rust 单二进制下这些
资源放在哪里（`current_exe()` 同目录？`$XDG_DATA`？嵌入二进制？）。`ARCHITECTURE.md` §8 没有这一条。

影响：M4 里程碑要求 “`kiana --version`、`kiana -p`、interactive 全部可用” 以及 “黄金测试逐字节对齐”，
但错误消息里的 `docs/providers.md` 路径、主题加载、HTML 导出模板、`clankolas.png` 都依赖这些资源；
`documentation.test.ts`、`config-schemas.test.ts`、`theme-export.test.ts` 无法移植。

建议（需要架构者决策，建议单独写一节 ARCH §4.11 “打包资源与单二进制布局”）：
1. 选定资源定位策略（建议：`include_bytes!` 嵌入 + `KIANA_PACKAGE_DIR` 覆盖开发期）；
2. 明确 `getDocsPath()` 等返回 `PathBuf` 还是 `Option<PathBuf>`（嵌入后可能无真实路径）；
3. 明确 `docs.json` 是否随二进制分发（`documentation.test.ts` 依赖它做导航校验）；
4. 明确 `schemas/` 的生成期 vs 运行期用途（`generate:schemas` 是开发脚本，运行时是否读）。

### G2.4 `photon` WASM 资源

`packages/coding-agent/src/utils/photon.ts` 使用 `photon-node` 的 WASM；`packages/durable/src/images/photon-wasm.d.ts`
是类型垫片。`spec/ca-cli.md` §6.2 说 “`photon-node`（WASM）→ `image` + `jpeg-encoder`/`png` + 自写 Lanczos3”，
并标注为“最难”。骨架 `kiana-core/src/image_process.rs` 没有实现，`kiana-durable` 的 `images` feature 依赖 `image`+`kamadak-exif`
但无 Photon 等价物。

影响：图片 resize 后的 base64 字节若与 Photon 不一致，`image-process.test.ts`/`image-resize-*.test.ts`/
`tool-result-images.test.ts` 会失败。

建议：架构者确认是否接受“用 `image` crate 重写并放宽字节一致性”（只比对尺寸/格式），还是必须逐字节复刻 Photon；
后者需要在 `kiana-core` 引入 Photon 的 Rust 移植或 wasm 运行时。

**结论（已决 / decided，ARCH §8 第 20 条）**：用**原生 `photon-rs 0.3.3`**（与 TS 的
`@silvia-odwyer/photon` 0.3.3 同一份 Photon 源码），**不引入 wasm 运行时**、不手写 Lanczos3。
2026-10-26 实测（320×240、Lanczos3）：JPEG quality 75 与 photon-node 的 wasm **逐字节相同**；
PNG 像素数据只差 1 个通道的末位（2/307,200 字节），编码层与 photon-rs 逐字节相同。
验收按“尺寸/格式/base64 限额逐条一致 + PNG 允许末位舍入”执行；落点
`kiana-durable::images::*`（feature `images`）与 `kiana-core::image_process`（feature `images`）。

---

## G3 未分配单元

### G3.1 `examples/` 未被任何 spec 纳入验收

只有 `spec/ca-extensions.md` 把 `examples/extensions/**` 当作扩展语料提到（且只列了部分文件名）。
未被任何 spec/ARCH 提到的文件：

- `examples/sdk/01-minimal.ts` … `13-session-runtime.ts`（14 个里 13 个未提，只有 `06-extensions.ts` 在
  `spec/ca-extensions.md` 出现）；
- `examples/extensions/doom-overlay/doom/{build.sh,doomgeneric_kiana.c,build/doom.js,build/doom.wasm}`；
- `examples/extensions/dynamic-resources/{dynamic.json,dynamic.md}`；
- `examples/extensions/subagent/{agents/*.md,prompts/*.md}`；
- `examples/extensions/{custom-provider-anthropic,custom-provider-gitlab-duo,gondolin,sandbox,with-deps,plan-mode}/**` 的内部文件。

影响：示例是扩展 API 的“活文档”，也是 `spec/ca-extensions.md` §8 的 “examples/extensions 语料”测试的输入；
未纳入会导致 SDK 面（M2）与扩展语料（M3）验证不完整。`doom.wasm` 还关系到 `wasmtime` feature 的验收。

建议：在 `spec/ca-extensions.md` 或新建 `spec/` 小节列出 “examples 语料清单 + 每个示例对应的 Rust 验收”
（至少 `cargo test -p kiana-extensions` 的 corpus 测试要覆盖全部 86 个 `.ts`）。

### G3.2 `docs/` 有 6 个文件完全未被引用

`docs/{containerization,quickstart,security,termux,windows,nvidia-rtx-llama-cpp}.md` 在 28 份 spec 与
`ARCHITECTURE.md` 中 0 次提及；`docs/images/*.png`（4 个）也未被提及。其余 36 个 `.md` 与 `docs.json`
至少在 `documentation.test.ts` 或 `getDocsPath()` 相关段落出现。

影响：`documentation.test.ts` 的导航完整性测试（“导航缺失页、孤立页”）需要全部 docs 的清单；
若不移植这 6 个文件，测试无法逐字对齐。另外 `windows.md`/`termux.md` 含平台行为说明，可能影响
`spec/ca-cli.md` 的 Windows 分支（`windows-self-update.ts`）与 Termux 路径处理。

建议：在 G2.3 的资源决策里一并覆盖；`docs` 的移植可以是“原样复制 + 校验和测试”，不需要重新编写。

### G3.3 7 个测试/辅助文件未在规范中出现

逐文件按 basename 在 28 份 spec 与 `ARCHITECTURE.md` 中 grep，0 命中：

- `test/suite/regressions/{3317-network-connection-lost-retry,6019-explicit-provider-retry-message,6904-dns-transport-retry,7301-stalled-availability-refresh}.test.ts`；
- `test/{plan-mode-utils,test-harness}.test.ts`、`test/streaming-render-debug.ts`、`test/test-theme-colors.ts`；
- `test/fixtures/fake-external-editor.mjs`（`external-editor.test.ts` 的夹具；测试本身有提及，夹具没有）。

影响：`spec/ca-interactive-mode.md` §7 的测试清单自称覆盖 regressions，但这 4 个 regression 文件 0 次出现；
按 ARCH §7.0 规则 4，验收门槛是“spec 的验收清单逐条勾掉”，缺条目会导致该规则无法执行。

建议：各 spec owner 在自己 §7 的测试清单补上文件名；夹具在对应 crate 的 `tests/fixtures/` 里 1:1 复制。
这些是“补齐清单”的小工作，但需要 owner 确认归哪个 crate（`3317`/`6019`/`6904`/`7301` 都是 provider 重试，
可能归 `kiana-ai` 的 golden 测试，而不是 `kiana-interactive`）。

### G3.4 `scripts/`、CI、发行工具链无归属

TS 仓库的以下部分**没有任何 Rust 侧规范**：

- 根 `scripts/`（64 个条目）：`build-binaries.sh`、`build-coding-agent-bundle.mjs`、`check-pinned-deps.mjs`、
  `check-runtime-deps.mjs`、`check-entry-graphs.mjs`、`check-model-catalog-compat.mjs`、
  `check-lockfile-commit.mjs`、`hydrate`/`generate:model-catalog`/`update:model-catalog-pin`/`diff:model-catalog`、
  `coding-agent-smoke.mjs`、`browser-smoke-entry.ts`、`pack-packages` 等；
- `packages/coding-agent/scripts/{generate-schemas.ts,generate-references.ts,migrate-sessions.sh}`；
- `.github/workflows/`（14 个）：`build-binaries.yml`、`env-daemons.yml`、`env.yml`、`nix.yml`、
  `publish-model-catalog.yml`、`preview-packages.yml`、`ci.yml` 等；
- `nix/{package.nix,model-catalog.json}`、`.husky/pre-commit`；
- `packages/coding-agent/install-lock/**`（`spec/ca-resources.md` §2.17 描述了它，但 Rust 侧是否需要保留未定）。

规范侧：`spec/ca-resources.md` §2.17 描述 `install-lock`，`spec/ca-cli.md` §2.21 描述 `windows-self-update`/`zod`，
但**没有**一份文档说明“哪些 CI/脚本要移植、哪些随 npm 一起退役”。`ARCHITECTURE.md` 没有
“构建与发布”章节。

影响：M4 的“发布”判定（5 个二进制 + npm 包？）不清楚；`check-runtime-deps`/`check-pinned-deps` 对应的 Rust
版本（`cargo deny`？`cargo tree` 脚本？）没有计划；`generate:schemas` 等价物的输出位置（G2.3）也依赖它。

建议：新增 `ARCHITECTURE.md` §9 “构建、CI 与发布”（或独立 `spec/build-release.md`），至少覆盖：
1. 5 个二进制（`kiana`/`kiana-rpc`/`kiana-env`/`kiana-ai`/`kiana-evals`）的产物矩阵与目标三元组；
2. `KIANA_ENV_VERSION`/`KIANA_MODEL_DATA_DIR` 的 CI 注入点（ARCH §4.5 已写，需落到 workflow）；
3. npm 包是否继续发布（若发布，`files` 里的 `docs`/`examples`/`schemas` 从哪来）；
4. 依赖 pin 检查（替代 `check-pinned-deps.mjs`）；
5. `scripts/` 里哪些 retire、哪些移植。

### G3.5 `migrate-sessions.sh` 与 `install-lock`

- `packages/coding-agent/scripts/migrate-sessions.sh`（2.2 KB）是独立的一次性迁移脚本，规范 0 次提及；
  若 Rust 版不再需要，应在 ARCH 里明确“不移植”。
- `install-lock/**` 被 `spec/ca-resources.md` §2.17 详细描述（1,507 行 lockfile），但它是 **npm 安装器**的产物；
  Rust 二进制不读它。规范未说明 Rust 侧是否保留（若保留，谁负责同步版本）。

建议：在 G3.4 的发布章节里逐条决策：“保留/退役”。

---

## G4 规范互相矛盾（不能靠 §3.8 自动仲裁）

### G4.1 `kiana-config` 的 crate 落点

- `spec/ca-config-models.md` §8.1（原文）：“本单元位于 `kiana/crates/kiana-coding-agent/src/config/`（crate `kiana-coding-agent`），
  该 crate 同时承载 ca-cli/ca-session/ca-resources/ca-tools-*”。
- `ARCHITECTURE.md` §1/§3.3/§3.7：`kiana-config` 是独立 crate（L5），且 §3.7 明确拒绝“单 crate `kiana-coding-agent`”。

已按 §3.8 规则 1 判定为 ARCH 优先，并在本阶段**直接修正**了 `spec/ca-config-models.md` §8.1 的表述与整棵树
（前缀/缩进改为 `crates/kiana-config/src/`，见 §A）。**剩余**：该 spec 的 §2.x 正文里仍有以
`kiana-coding-agent` 为上下文的模块路径引用（例如“该 crate 同时承载…”式的历史描述），需要 ca-config-models
owner 全文再扫一遍；同样地 `spec/ca-tools-compaction-export.md` §8.1 的树虽已加注，但正文 §2.x 的
“本单元 crate”措辞可能仍按旧单 crate 写法。

### G4.2 `kiana-session` vs `kiana-session-core` 的依赖方向

- `spec/ca-config-models.md` §8.1 边界段写 `kiana-session`（`SessionEntry`/`UsageEntry`），但 `kiana-config` 是 L5、
  `kiana-session` 是 L11，`kiana-config -> kiana-session` 会违反 ARCH §2 的依赖方向（已在本阶段改为 `kiana-session-core`）。
- `spec/ca-resources.md` §8.1 说 `kiana-resources` “被 `kiana-coding-agent` 与 `kiana-session` 同时使用”，但 `kiana-resources`
  是 L10、`kiana-session` 是 L11，方向正确；然而同节 §8.4 又写 “`LoadExtensionsResult` 来自 `kiana-session`，
  本 crate 只 re-export”——`kiana-resources`（L10）不能依赖 `kiana-session`（L11）。
- `spec/ca-extensions.md` §8.1 写依赖 `kiana-session`，但 `kiana-extensions` 是 L9，`kiana-session` 是 L11，反向。

已在本阶段修正 `spec/ca-extensions.md` §8.1 与 `spec/ca-config-models.md` 的边界段；`spec/ca-resources.md` §8.4
的 `LoadExtensionsResult` 一句**未改**（需要 owner 决定：`LoadExtensionsResult` 应在 `kiana-ext-api`（L7），
由 `kiana-extensions`（L9）构造，`kiana-resources`（L10）向下引用 `kiana-ext-api`）。这与 `ARCHITECTURE.md` §3.4/§3.5
的“`types.ts` 唯一权威在 `kiana-ext-api`”一致，但 `spec/ca-extensions.md` §8.2 的类型草图仍把
`LoadExtensionsResult` 放在 `kiana-extensions` 的 `types/` 下。**需要 ca-extensions owner 确认最终落点**。

### G4.3 `env-evals-durable-testing.md` §8.1 的布局与骨架不符

已在本阶段修正（`kiana-env-client` 的 `path/{posix,win32}` 与 `decode.rs` 不存在、`kiana-evals-fixtures` crate 不存在、
`kiana-env` 的 `Cargo.toml` 注释仍写“build.rs 读 ../package.json”），但遗留决策：

- `node:path` 的 posix/win32 两套语义**必须**实现（§6.2/§8.3 第 1 条），落点未定：`kiana-env-client/src/path/`
  与 `kiana-durable::env::node`（骨架有 `node.rs`）二者选一，或放 `kiana-core`。`kiana-core` 的 `paths.rs` 是
  `utils/paths.ts`（coding-agent 的路径工具），**不是** `node:path` 的完整移植，不能直接复用。
- `decode.rs`（`StreamDecoder`/`range_decoder`/`decode_into`/`starts_with_bom`）在骨架的 `kiana-durable/src/env/decode.rs`
  已存在（文件是空的 `//!` 头），但 `kiana-env-client` 没有 re-export 文件；`encoding_rs` 只在 `kiana-env` 的依赖里，
  `kiana-durable` 与 `kiana-env-client` 的 `Cargo.toml` 都**没有** `encoding_rs`——而 §2.3 说 client 复用 Durable 的解码器。

建议：架构者在 `ARCHITECTURE.md` §4.9 增加一行 “`node:path` posix/win32 移植的唯一实现位置”，并确认
`kiana-durable` 是否要加 `encoding_rs`（`StreamDecoder` 是 Durable 的 `env/decode.ts`）。

**结论（已决 / decided，ARCH §8 第 23 条）**：`node:path` 的唯一实现位置是
`kiana-durable::env::path::{posix,win32}`（已建三个文件，`PathFlavor` 选方言），
`kiana-env-client` 用 `pub use kiana_durable::env::path::posix as node_path;` 复用，**不得**有第二份实现；
`kiana-core::paths` 不参与。`kiana-durable` 已加 `encoding_rs`（`StreamDecoder` 留在
`kiana-durable::env::decode`），`kiana-env-client` 直接复用、不再加一份。ARCH §4.9 已登记这一行。

### G4.4 `kiana-ext-api` 与 `kiana-extensions` 的 `types/` 归属

`spec/ca-extensions.md` §8.2 的树把 `types/{mod,ui,context,tools,events,api,...}.rs` 全放在 `kiana-extensions`，
但 `ARCHITECTURE.md` §3.4/§3.5 与骨架（`crates/kiana-ext-api/` 8 个文件）把 `core/extensions/types.ts` 的公开契约
放在 `kiana-ext-api`（L7）。已在本阶段给 `spec/ca-extensions.md` §8.1 加注说明，但 §8.2 的树与类型草图未逐行改
（工作量大且需要 owner 决定哪些类型属于“公开契约”、哪些属于“runner 内部”）。

建议：ca-extensions owner 与 arch 一起列出 `types.ts` 的 2,308 行里哪些进 `kiana-ext-api`、哪些留在 `kiana-extensions`，
更新 §8.2 并删除重复定义（§7.0 规则 3 的“共享契约冻结”）。

### G4.5 `kiana-core` 的 `win32`/`termux`/`wsl` 平台分支缺规范

`utils/{windows-self-update,wsl,deprecation}.ts` 只有 `spec/ca-cli.md` §2.21 的简短描述（`windows-self-update.ts` 84 行、
`wsl.ts` 15 行），没有独立的验收清单；`docs/windows.md`/`docs/termux.md` 也未被引用（G3.2）。
`spec/ca-cli.md` §2.21 的 “安装方式检测/自更新” 是 `kiana update` 的实现核心，但没有定义 Rust 侧
`SelfUpdateCommand` 的执行器（`tokio::process::Command`？`std::process::Command`？）与 npm/brew/bun 的探测顺序。
执行器的另一处候选是 `spec/ca-resources.md` §8.2 的 `pkg_cli/self_update.rs`（`run_self_update`/`print_*`），
两份规范**都没有**说这两处的边界（谁负责探测、谁负责执行、谁打印）。

影响：`kiana update`（CLI 子命令，`spec/ca-cli.md` §2.9）与 `config.ts` 的 8 个导出（`detectInstallMethod` 等）
没有完整验收。

建议：`spec/ca-cli.md` 补一节 “自更新执行器”，明确它与 `spec/ca-resources.md` §8.2 的
`pkg_cli/self_update.rs` 的分工（建议：`kiana-core::config` 只做 `InstallMethod` 检测与命令构造，
`kiana-resources::pkg_cli` 执行 managed install 更新，`kiana-coding-agent` 的 CLI 只做编排），并把
`docs/windows.md`/`termux.md` 的语义并入。

---

## G5 版本常量与包装配（需决策）

### G5.1 `VERSION` / `KIANA_AI_VERSION` = `"1.4.0"` vs workspace `0.1.0`

- `spec/ca-cli.md` §2.6 定义 `pub const VERSION: &str; // "1.4.0"`；`spec/ca-interactive-mode.md` §2 也说
  `VERSION = pkg.version || "0.0.0"`（当前 1.4.0）。
- `spec/ai-api-other.md` 定义 `KIANA_AI_VERSION = "1.4.0"`（`utils/version.ts`）。
- 根 `Cargo.toml` 的 `[workspace.package] version = "0.1.0"`。

规范没有说明 Rust 侧 `VERSION`/`KIANA_AI_VERSION` 是硬编码 `"1.4.0"`、读 `CARGO_PKG_VERSION`、还是由 CI 注入
（类似 `KIANA_ENV_VERSION`）。`ARCHITECTURE.md` §4.5 只定义了 `KIANA_ENV_VERSION`。

影响：`kiana --version`、User-Agent (`kiana/1.4.0 (...)`)、changelog 版本标题、`env_version_matches_npm_package`
测试都会依赖它；若 Rust 版发 `0.1.0`，与 TS 的黄金测试会不一致。

建议：架构者决定（建议与 `KIANA_ENV_VERSION` 同模式：`build.rs` 注入 `KIANA_CODING_AGENT_VERSION`/`KIANA_AI_VERSION`，
CI 取 `packages/{coding-agent,ai}/package.json`），并写进 ARCH §4.5 + §8。

**结论（已决 / decided，ARCH §8 第 24 条）**：**字面量**，`"1.4.0"`（= 两个 `package.json` 的
当前 `version`）。已落地：`kiana_ai::KIANA_AI_VERSION`（`crates/kiana-ai/src/utils/version.rs`，`lib.rs`
再导出）与 `kiana_core::config::{VERSION, PACKAGE_NAME, APP_NAME, APP_TITLE, CONFIG_DIR_NAME}`
（`crates/kiana-core/src/config.rs`）。**不读 `CARGO_PKG_VERSION`**（workspace 是 0.1.0）、
**不新增**构建期注入变量；CI 校验这两个常量分别等于对应 `package.json` 的 `version`。
`kiana-env` 的 `KIANA_ENV_VERSION` 保持既有构建期注入（daemon 独立二进制，已有 build.rs）。注入点未设置时
回退 **`packages/env/package.json#version` 的字面量 `1.4.0`**（`crates/kiana-env/build.rs` 的 `NPM_VERSION`），
不回退 `CARGO_PKG_VERSION`：参照 daemon 的 `build.rs` 读 `../package.json`，上报值永远是 npm 包版本，
crate 版本 `0.1.0` 会偏离它（`tests/version.rs::env_version_matches_npm_package` 对比 `--version` 输出
与 `hello.version`）。此决定同步到 `ARCHITECTURE.md` §4.5/§8 第 7 条与
`spec/env-evals-durable-testing.md` §2.2.1。

### G5.2 npm 包是否继续发布

`packages/coding-agent/package.json` 的 `files`（`dist`/`docs`/`examples`/`schemas`/`containerization.md`/`CHANGELOG.md`）、
`bin.kiana`/`exports`、`prepublishOnly`（`spec/ca-cli.md` §2.9/§8.1 的 `package-distribution.test.ts`）都假定 npm 发布。
Rust 版是纯二进制，是否继续发 npm 包（用 `napi`/`postinstall` 下载二进制？）、`docs`/`examples` 从哪来（G2.3）、
`package-distribution.test.ts` 如何对齐，都没有决策。

建议：在 G3.4 的发布章节里一并决定；若不再发 npm，应在 ARCH 里明确 `spec/ca-cli.md` 的
`package-distribution.test.ts` 属于“不移植”，并说明替代测试（`cargo` 产物的布局断言）。

**结论（已决 / decided，ARCH §8 第 25 条）**：M1 **不发 npm 包**，只发 5 个二进制的产物；
`package-distribution.test.ts` 标记为“不移植”，替代为 `cargo build --release` 的布局断言
（bin 名、`--version` 输出、`KIANA_PACKAGE_DIR` 覆盖）。docs/examples/schemas 的定位随
G2.3/G3.4 在阶段 8 的发布章节单独决策。

---

## §A 本阶段已直接修复（小缺口）

以下已在 `ARCHITECTURE.md` 或 `spec/*.md` 中直接改正，无需再决策：

1. **ARCH §1 crate 表**：16 个子系统的文件/行数改为实测值（`kiana-core` 47/5,360、`kiana-theme` 5/2,261、
   `kiana-resources` 9/6,151、`kiana-extensions` 7/2,745、`kiana-builtin-extensions` 25/7,585、`kiana-interactive` 49/19,342、
   `kiana-coding-agent` 23/4,584 等），并补了 252 文件 / 76,291 行的换算说明（`modes/index.ts` 重复计、`core/index.ts` 未列）。
2. **ARCH §3.7**：`kiana-interactive` 行数 19,326 → 19,342。
3. **ARCH §5**：`kiana-ai` 的 `openrouter_api.rs` 笔误 → `providers/openrouter-api.ts`（77 行）；
   `compat.rs` 合并 `legacy-api-aliases.ts`/`oauth.ts` 的说明；`kiana-ai`/`kiana-evals` 的 `bin`/`main.rs` 是薄入口；
   `providers/data-json.d.ts` 是类型垫片不生成 `.rs`（另见 ARCH §8 第 14 条）。
4. **ARCH §5 kiana-core `config.rs`**：补全 46 个导出清单；**kiana-theme** 明确 `highlightCode` 委托 `kiana-core::syntax_highlight`。
5. **ARCH §5 kiana-tui / kiana-codemode**：分别注明原生平台层无契约（G1.5）与 prelude/feature 未接线（G2.2），指向本文件。
6. **ARCH §7.1 阶段表**：各 crate 文件/行数同步为实测值；`kiana-json` 730 → 723 行；`kiana-evals` 注明入口。
7. **ARCH §8 第 13/14 条**：spec 里的依赖版本是“建议”（根 `Cargo.toml` 权威：reqwest 0.13.5 / notify 8.2.1 /
   edition 2024 / rust-version 1.97；x11rb/objc2-app-kit 已于 2026-10-26 补 pin，arboard 明确不引入
   → 见 §0 与 G1.5）；`.d.ts` 不生成 `.rs`。
8. **`spec/ca-cli.md`**：§0.1 明确 `src/utils/**` 的规格在本单元、`.rs` 实现在 `kiana-core`（37 个文件）；
   §8.1 的 crate 名 `kiana-cli` → `kiana-coding-agent`（bin 名仍是 `kiana`），`utils/` 注明只 `pub use kiana_core::…`。
9. **`spec/ca-resources.md`**：全部 `kiana-cli` → `kiana-coding-agent`；§0.2 边界表的 `durable-harness`/`tui-core`/
   `ca-session` 归属改为 `kiana-ext-api`+`kiana-extensions`/`kiana-theme`/`kiana-tools`/`kiana-export`；§8.2 的
   `source_info.rs`/`diagnostics.rs`/`timings.rs`/`manifest.rs` 标明是再导出、`slash_commands.rs` 不存在；
   §5.3 依赖表的单元名同步。
10. **`spec/ca-config-models.md`**：§8.1 的 crate 落点改为 `kiana-config`（旧版写 `kiana-coding-agent/src/config/`），
    整棵树的前缀/缩进改为 `crates/kiana-config/src/`；`kiana-ai-auth` → `kiana_ai::auth`；`kiana-session` → `kiana-session-core`；
    `virtual.rs` → 骨架名 `virtual_models.rs`；`cache/stats.rs`/`usage_totals.rs` 标注为在 `kiana-session-core`；
    `http_dispatcher.rs` 标注为在 `kiana-core`（§3.3）。
11. **`spec/ca-tools-compaction-export.md`**：§8.1 “单 crate `kiana-coding-agent`” 改为 `kiana-tools`+`kiana-compaction`+`kiana-export`。
12. **`spec/ca-interactive-mode.md`**：§8.1 依赖列表去掉不存在的 `kiana-tui-components`/`kiana-tui-text`，补 `kiana-ext-api` 等；
    资源树里的 `themes/*.json` 移到 `kiana-theme`（`clankolas.png` 留本 crate），§6.5 同步。
13. **`spec/ca-extensions.md`**：§8.1 依赖改为骨架实际依赖（`kiana-ext-api`、`kiana-session-core`，无 `kiana-session`），
    说明 `types.ts` 的公开契约在 `kiana-ext-api`、`durable-harness` 只是单元名，并作废 §0.3 的
    “`core/extensions/**` 归 durable-harness” 说法。
14. **`spec/env-evals-durable-testing.md`**：§2.2.1 的依赖 pin 以 workspace 为准（`notify 8.2.1`、`serde_json 1.0.154`）、
    工具链 `rust-version 1.97`/edition 2024、`build.rs` 与骨架 `crates/kiana-env/build.rs` 逐字对齐；
    §8.3 的 `kiana-env-client` 树去掉不存在的 `path/`/`decode.rs`、`kiana-evals-fixtures` 标注为“未采纳”、
    `kiana-env` 的 `Cargo.toml` 注释改为不读 `../package.json`。
15. **`crates/kiana-theme/{src/lib.rs,Cargo.toml}`**：头注释说明 TS 源与 `kiana-core::syntax_highlight` 委托；
    description 同步。
16. **`crates/kiana-env-client/Cargo.toml`**：description 与 TS `package.json` 逐字一致（`spec/env-evals-durable-testing.md` §2.2.1）。
17. **ARCH §8 第 15 条**：新增，指向本文件的 G1–G5 分类与“开工前需逐条决策”的要求；
    `spec/GAPS.md` 的交叉引用（ARCH §5、`ca-interactive-mode.md` §6.5）改为带编号的条目名。

18. **根 `Cargo.toml` 新增 7 个 pin**（`cargo info` 逐个校验、`cargo check` 实际解析）：
    `hmac = "=0.13.0"`、`zstd = "=0.14.3"`、`photon-rs = { "=0.3.3", default-features = false }`、
    `x11rb = "=0.14.0"`、`objc2 = "=0.6.5"`、`objc2-foundation = "=0.3.2"`、
    `objc2-app-kit = "=0.3.2"`；`tokio-tungstenite` 补 `rustls-tls-native-roots`；`windows-sys`
    补 8 个 feature（Console/DataExchange/LibraryLoader/Memory/Ole/UI_Input/UI_WindowsAndMessaging 等）。
19. **`kiana-ai`**：`src/http.rs` → `src/http/{mod.rs,reqwest_client.rs}`（契约冻结 + 实现 stub）；
    `utils/streaming_json.rs` 从 TODO 改为完整契约（`ToolCallArgumentsStream`/`Replace`/`Finish`/
    `StreamingJsonParser`/`partial()`）；`utils/version.rs` 提供 `KIANA_AI_VERSION = "1.4.0"` 并在
    `lib.rs` 再导出；新增 `data/`（42 个 provider JSON + `.manifest.json` + `README.md`）。
20. **`kiana-tui`**：新增 `src/native/{mod,module_path,linux_x11,darwin,win32}.rs`，
    `image.rs`/`keys.rs` 改为再导出；feature `x11`/`darwin`/`win32`/`native`；新增
    `tests/native_clipboard.rs`（6 个测试）。
21. **`kiana-agent-core`**：`src/proxy.rs` 从 TODO 改为完整契约（`ProxyAssistantMessageEvent` 13 变体、
    `ProxyStreamOptions`、`stream_proxy`/`process_proxy_event` 的签名与逐字错误文案）。
22. **`kiana-codemode`**：`js/prelude.js` vendored（17,233 字节，FNV-1a `8c6c9242`）；
    `Cargo.toml` 加 `wasmtime`/`rquickjs`（optional）与 4 个 feature；`cloudflare.rs` 占位。
23. **`kiana-durable`**：`env/path/{mod,posix,win32}.rs`（`node:path` 唯一实现）；
    `storage/indexeddb.rs`、`storage/sqlite/cloudflare.rs`、`images/cloudflare.rs` 的不支持占位
    （逐字错误文案）；`images/{mod,node,exif}.rs`（Photon 映射与 EXIF 手写解析的契约）。
24. **`kiana-chord`**：`sqlite/cloudflare.rs` 占位（not supported，逐字文案）。
25. **`kiana-core`**：`config.rs` 补齐 5 个版本/名称常量（`VERSION = "1.4.0"` 等）；
    `Cargo.toml` 加 `reqwest`/`base64`/`flate2`/`crc32fast` 与 feature `images`。
26. **`spec/PROGRESS.md`**：新建，含状态语义（`todo`/`wip`/`done`/`blocked`）与
    `| unit | crate | status | notes |` 表头，供各 owner/合并者追加。
27. **ARCH §4.5/§4.9/§5/附录 A**：`http.rs` 改 `http/`、新增 `native/**`、`env/path/**`、
    `images/*` 的行；§4.9 契约表补 `NativeClipboard`/`ToolCallArgumentsStream`/`stream_proxy`/
    `node:path` 四行；附录 A 更新 pin 数与新增 pin 的说明。
28. **`kiana-env` 的 `build.rs` 回退值**（阶段 2，`crates/kiana-env/build.rs`）：未注入 `KIANA_ENV_VERSION` 时回退
    `NPM_VERSION = "1.4.0"`（= `packages/env/package.json#version`）而不是 `CARGO_PKG_VERSION`，理由见
    G5.1 的结论段；`ARCHITECTURE.md` §4.5/§8 第 7 条与 `spec/env-evals-durable-testing.md` §2.2.1 已同步，
    `tests/version.rs::env_version_matches_npm_package` 对比 `--version` 输出与 `hello.version`。
29. **线协议 JSON 的键序**（已记录，不是缺陷）：本工作区按 §6.1/`ARCHITECTURE.md` §4.3 开启
    `serde_json` 的 `preserve_order`，回复按插入序（`hello`：protocol、version、os、…；`lstat`/`info`：
    name、kind、size、mtimeSec、mtimeNsec、dev、ino；错误：code、message、syscall、path）。参照 daemon 的
    `serde_json` 未开该 feature，字节上按字典序（arch、cwd、dev、…；code、message、path、syscall）。
    字段集与值完全相同，客户端解析 JSON、不遍历回复键序（`src/remote-env.ts` 唯一的 `Object.entries`
    遍历的是客户端自己构造的对象），`packages/env/test` 的全部用例通过；`tests/golden.rs` 因此按解析后的
    值对比而非原始字节（见该文件头注释）。

> 注：`~/.cargo/bin/cargo check --workspace` 已通过（0 warning）；本阶段没有改任何 `kiana/` 下的文件
> （`git status` 在 `kiana/` 为空）。
