# ai-auth 单元移植规格（TypeScript -> Rust）

本文件是 `@earendil-works/kiana-ai` 中 **ai-auth** 单元（`src/auth/**`、`src/oauth.ts`、`src/bun-oauth.ts`）的完整移植规格。
源仓库 `packages/ai` 为只读参考；Rust 实现位于 `/home/ctyun/code/kiana_rev/kiana`。

- 源包：`/home/ctyun/code/kiana_rev/kiana/packages/ai`，名称 `@earendil-works/kiana-ai`，版本 `1.4.0`。
- 本单元文件范围（共 20 个文件、3871 行）：
  - `src/auth/types.ts`（259）、`src/auth/context.ts`（45）、`src/auth/helpers.ts`（59）、
    `src/auth/credential-store.ts`（67）、`src/auth/resolve.ts`（217）
  - `src/auth/oauth/pkce.ts`（34）、`oauth/device-code.ts`（98）、`oauth/callback-server.ts`（183）、
    `oauth/load.ts`（80）、`oauth/anthropic.ts`（307）、`oauth/openai-codex.ts`（438）、
    `oauth/openai-chatgpt.ts`（309）、`oauth/github-copilot.ts`（507）、`oauth/openrouter.ts`（169）、
    `oauth/kimi-coding.ts`（296）、`oauth/meta.ts`（208）、`oauth/xai.ts`（239）、`oauth/radius.ts`（321）
  - `src/oauth.ts`（10）、`src/bun-oauth.ts`（25）
- **不在本单元范围但被本单元直接调用**：`src/utils/oauth-page.ts`、`src/utils/abort.ts`、
  `src/utils/models-error.ts`、`src/utils/provider-env.ts`、`src/utils/sleep.ts`、`src/utils/diagnostics.ts`、
  `src/providers/github-copilot.models.ts`、`src/providers/radius-config.ts`。这些文件的契约在第 2.20 节逐字记录，
  实现由 **ai-core** 单元负责（`ai-core.md` 已覆盖大部分；本文件只补充本单元用到的精确语义）。
- 本单元**不**负责 `src/models.ts` 的编排逻辑（`login`/`logout`/`getAuth`/`checkAuth`/`resolveRefreshCredential`）——
  那是 ai-core 的 §2.1.3；本文件第 5 节只记录本单元被其调用的精确契约。
- 本文件同时是 (a) Rust 移植的验收清单（逐条可测），(b) Rust 侧设计说明。

## 目录

1. 概览
2. 逐文件规格（导出符号 + 行为）
3. 用户可见表面（逐字文本）
4. 外部格式（磁盘 / 环境变量 / HTTP / 网页）
5. 跨单元接口
6. npm 依赖与 Rust crate 映射、语义差异桥接
7. 测试清单
8. Rust 模块布局与最难复刻的部分

---

## 1. 概览

### 1.1 目的

ai-auth 是 kiana-ai 的**凭证与登录层**：

1. **凭证模型**：`ProviderAuth { apiKey?, oauth? }`、`CredentialStore`（每 provider 一条带类型标签的凭证）、
   `AuthContext`（env / fileExists）。
2. **鉴权解析**：`resolveProviderAuth()` 决定一次请求用哪个 api key / header / baseUrl，以及 OAuth 的
   **双重检查锁刷新**（`refreshStoredOAuthCredential()`）。
3. **登录交互协议**：`AuthInteraction.prompt()/notify()` 与 `AuthPrompt`/`AuthEvent`（UI 中立，TUI 只消费）。
4. **九条 OAuth 流程**：Anthropic（浏览器/复制码）、OpenAI Codex legacy（浏览器/设备码）、
   Sign in with ChatGPT（动态客户端注册）、GitHub Copilot（设备码 + 模型策略启用）、OpenRouter（PKCE→永久 key）、
   Kimi Code（设备码）、Meta（设备码→兑换 API key）、xAI（设备码）、Radius（浏览器/设备码，对接自定义 gateway）。
5. **OAuth 流程的按需加载/静态内嵌**：`load*.ts` 的 9 个 loader + `registerBundledOAuthFlowLoaders` +
   `registerBunOAuthFlows()`。

### 1.2 在 monorepo 中的位置

- `packages/ai/src/index.ts` 直接 `export *` 了 `auth/context.ts`、`auth/credential-store.ts`、`auth/helpers.ts`、
  `auth/types.ts`（因此这些是包根公共面），并 `export type {...} from "./compat/extension-oauth-types.ts"`。
- `src/models.ts` 从 `auth/resolve.ts` 引入 `resolveProviderAuth`、`refreshStoredOAuthCredential`、`ModelsError`，
  从 `auth/credential-store.ts` 引入 `InMemoryCredentialStore`，从 `auth/context.ts` 引入默认 context。
- 各 provider 工厂（`providers/anthropic.ts`、`openai.ts`、`openai-codex.ts`、`github-copilot.ts`、`openrouter.ts`、
  `kimi-coding.ts`、`meta.ts`、`xai.ts`、`radius.ts`）通过 `envApiKeyAuth()` / `lazyOAuth()` + `load*OAuth` 挂载 OAuth。
- `packages/coding-agent` 通过包根使用 `CredentialStore`/`AuthPrompt`/`AuthEvent`/`OAuthDeviceCodeInfo`，
  自带持久化实现 `AuthStorage`（`auth.json`）与 TUI `LoginDialogComponent`（不在本单元范围）。
- `src/oauth.ts` 是**仅类型**的兼容入口（`@earendil-works/kiana-ai/oauth`），供 coding-agent 扩展 OAuth 声明使用；
  **不得**从该入口泄漏任何内置 OAuth 实现（有测试断言）。
- `src/bun-oauth.ts` 是 `@earendil-works/kiana-ai/bun-oauth` 入口，供 Bun 单文件二进制静态内嵌全部 OAuth 流程。

### 1.3 入口点

| specifier | 目标 | 与本单元的关系 |
|---|---|---|
| `.` | `dist/index.js` | 再导出 `auth/{context,credential-store,helpers,types}.ts` 全部符号 |
| `./models` | `dist/models.js` | 不含 auth 实现（仅类型经 `models.ts` 间接引用）；额外 `export { ModelsError, type ModelsErrorCode } from "./auth/resolve.ts"`（见 1.5 勘误） |
| `./compat` | `dist/compat.js` | 旧全局 API（`stream`/`complete` 等）；它 `export * from "./env-api-keys.ts"` 并在缺 `apiKey` 时调 `getEnvApiKey(model.provider, options?.env)`（**不是** `envApiKeyAuth`），本身不含 auth 实现 |
| `./oauth` | `dist/oauth.js` | **仅** `src/oauth.ts` 的 7 个类型重导出 |
| `./bedrock-provider` | `dist/bedrock-provider.js` | 独立入口（Bedrock provider 工厂）；与 auth 无直接关系 |
| `./bun-oauth` | `dist/bun-oauth.js` | `registerBunOAuthFlows()` |
| `./providers/*`、`./api/*`、`./utils/*` | 各文件 | 三者为独立子路径导出；**不存在** `auth/utils/*` 子路径（原表述有误，`src/auth/` 下也没有 `utils/` 目录） |

`sideEffects` 不含 auth 任何文件：`src/auth/**` 必须是**无副作用**模块（除了 `bun-oauth.ts` 的显式调用）。
注意 `auth/oauth/load.ts` 与 `auth/context.ts` 使用**变量 specifier 的动态 import**，这正是为了让打包器
不把 Node 专有实现（`node:http`、`node:crypto`、`node:fs/promises`）拉进浏览器构建。

### 1.4 关键不变量（必须逐条在 Rust 中成立）

1. **存储凭证"拥有"provider**：只要有存储凭证，就绝不回退到环境变量；刷新失败也不回退。
2. 存储的 OAuth 凭证在 `< now + 5min` 即将过期时刷新；刷新在 `credentialStore.modify` 内完成，
   **并发/多进程只刷新一次**（锁内二次检查）。
3. **刷新一旦开始就忽略调用方 signal**，只用 15s 超时；这是为了保证已轮换的 refresh token 一定被持久化。
   调用方若要"取消即返回"，必须自己 race（`Models` 层负责）。
4. `modify` 是唯一写路径；`fn` 返回 `undefined` 表示"不修改"，`modify` 解析为 `next ?? current`。
5. `CredentialStore` 的读操作在开始前 `throwIfAborted()`；队列中的写操作被取消后**不得稍后执行**。
6. 每个 OAuth 流程的 `login()` 必须使用 `interaction.signal` 做网络请求与等待；`refresh()` 必须使用传入的 signal。
7. 所有 OAuth 错误信息在 `Models` 层被包装为 `ModelsError`，其 `message` 含 **cause 的 message**（见 2.5）；
   CLI 只打印 `error.message`。
8. 回调服务器（`callback-server.ts` 的通用实现）只接受 `GET` + 精确 path；state 不匹配 400、重复回调 409、非法路由 404、完成失败 502。
   **例外**：`openai-chatgpt.ts` 自带的那套服务器**不检查 HTTP 方法**（只比对 pathname），且回调解析失败时
   只回 400 页面、result promise 保持 pending（见 §2.12 与 §8.4 勘误）。

9. 设备码轮询遵循 RFC 8628：默认 5s、最小 1s、`slow_down` 时若服务端给出 `interval` 用之，否则 **+5s**。
10. 所有 `verification_uri` / `verification_uri_complete` 在交给 UI 之前必须验证为 http(s) URL
    （Copilot/OpenAI 接受 http+https，xAI 只接受 https，Kimi/Meta 接受 http+https），并**规范化**为 `URL.href`。
11. 任何持久化凭证的额外字段（`accountId`、`clientId`、`scopes`、`enterpriseUrl`、`availableModelIds`、`scope`、
    `gatewayConfig`）必须原样保存在 auth.json 的 oauth 条目里（`OAuthCredentials` 有索引签名）。
12. `OAuthCredentials.expires` 是 **毫秒时间戳**；OpenRouter 的永久凭证是 `Number.MAX_SAFE_INTEGER`（9007199254740991）。

### 1.5 作用域与符号计数

本单元范围内**导出名合计 67 个**，统计口径 = 各文件 `export` 语句导出的名字之和（`export type {...}`
的重导出按名字计入所在文件）。唯一重名只有 `OAuthCredentials` 一个（`auth/types.ts` 定义 + `oauth.ts` 重导出），
因此**去重后唯一名字为 66 个**（原写 64、并称 `ModelsError`/`ModelsErrorCode` 也重名，均属勘误：
`resolve.ts` 里这两个名字只有一条 `export { ModelsError, type ModelsErrorCode } from "../utils/models-error.ts"`，
`models.ts` 又原样重导出一次，但 `models.ts` 不在本单元文件范围内、不计入 67）：


`auth/**` 59 个（21+1+2+1+5+1+4+4+10+8+2）+ `bun-oauth.ts` 1 个 + `oauth.ts` 7 个 = 67；唯一 66。

- `auth/types.ts`：21（`ModelAuth`、`ApiKeyCredential`、`OAuthCredentials`、`OAuthCredential`、`Credential`、
  `CredentialInfo`、`AuthOperationOptions`、`CredentialStore`、`AuthContext`、`AuthResult`、`AuthCheck`、`AuthType`、
  `AuthPrompt`、`AuthInfoLink`、`AuthEvent`、`AuthInteraction`、`ProviderAuthInteraction`、`ApiKeyAuth`、
  `LoginOptions`、`OAuthAuth`、`ProviderAuth`，逐一见 2.1）
- `auth/context.ts`：1；`auth/helpers.ts`：2；`auth/credential-store.ts`：1；`auth/resolve.ts`：5（含 2 个重导出）
- `auth/oauth/pkce.ts`：1；`device-code.ts`：4；`callback-server.ts`：4；`load.ts`：10
- `auth/oauth/anthropic.ts`、`openai-codex.ts`、`openai-chatgpt.ts`、`github-copilot.ts`、`openrouter.ts`、
  `kimi-coding.ts`、`meta.ts`、`xai.ts` 各 1 个 `*OAuth` 常量（8 个）；`radius.ts`：2（`RadiusOAuthOptions`、
  `createRadiusOAuth`）
- `src/oauth.ts`：7 个类型重导出（`OAuthAuthInfo`、`OAuthCredentials`、`OAuthDeviceCodeInfo`、
  `OAuthLoginCallbacks`、`OAuthPrompt`、`OAuthSelectOption`、`OAuthSelectPrompt`，全部来自
  `compat/extension-oauth-types.ts`，属 ai-core 导出面）；`src/bun-oauth.ts`：1（`registerBunOAuthFlows`）

精确列表以第 2 节每文件小节为准；Rust 侧导出 `pub` 项应一一对应（见 8.1）。

---

## 2. 逐文件规格

约定：签名用 TypeScript 原文；"行为"按实现顺序描述；所有字符串字面量逐字给出（Rust 必须完全一致）。
"JS 语义"指必须按 JavaScript/WHATWG 行为实现，不能替换为 Rust 自然写法（见第 6.2 节）。

### 2.1 `src/auth/types.ts`（259 行）——凭证与交互类型

本文件**只有类型**，无运行时代码（Rust 侧为等价的 `struct`/`enum`/`trait`）。依赖
`../types.ts` 的 `ProviderEnv`、`ProviderHeaders`（类型别名，见 ai-core §2.1.2）。

```ts
export interface ModelAuth {
	apiKey?: string;
	headers?: ProviderHeaders;
	baseUrl?: string;
}

export interface ApiKeyCredential {
	type: "api_key";
	key?: string;
	env?: ProviderEnv;
}

export interface OAuthCredentials {
	refresh: string;
	access: string;
	expires: number;
	[key: string]: unknown;
}

export interface OAuthCredential extends OAuthCredentials {
	type: "oauth";
}

export type Credential = ApiKeyCredential | OAuthCredential;

export interface CredentialInfo {
	providerId: string;
	type: Credential["type"];   // "api_key" | "oauth"
}

export interface AuthOperationOptions {
	signal?: AbortSignal;
}
```

`ModelAuth` 语义（源码注释逐字）："Request auth for a single model request. If a value cannot be expressed as
`apiKey`, `headers`, or `baseUrl`, it is provider config, not auth."

`ApiKeyCredential.env`：provider 作用域的 environment/config 值（例如 Cloudflare account/gateway id）。
`key` 允许缺省（此时完全靠 env/ambient 解析）。

`OAuthCredentials` 有 **索引签名**：任何额外字段都必须保留（Rust 侧应建模为
`{ refresh: String, access: String, expires: f64/i64, extra: IndexMap<String, serde_json::Value> }`，
序列化时把 `extra` 平铺回对象；`type` 字段由 `OAuthCredential` 提供）。

`CredentialStore`（接口 + 逐字注释语义，**这是 Rust trait 的验收契约**）：

```ts
export interface CredentialStore {
	read(providerId: string, options?: AuthOperationOptions): Promise<Credential | undefined>;
	list(options?: AuthOperationOptions): Promise<readonly CredentialInfo[]>;
	modify(
		providerId: string,
		fn: (current: Credential | undefined) => Promise<Credential | undefined>,
		options?: AuthOperationOptions,
	): Promise<Credential | undefined>;
	delete(providerId: string, options?: AuthOperationOptions): Promise<void>;
}
```

- 文件头注释要点：App-owned storage，按 `Provider.id` 一条凭证；`modify` 是唯一写路径，因此每次变更都是
  串行化的 read-modify-write；`Models.getAuth()` 在 `modify` 内做 OAuth 刷新，避免并发请求重复刷新已轮换的 token。
  App 在 login 后通过 `modify(provider.id, async () => credential)` 持久化。login/logout 编排归 app 所有。
- 错误语义：`read` 对缺失条目解析为 `undefined`；方法仅在存储失败时 reject；`Models` 把这类 rejection 包成
  code `"auth"` 的 `ModelsError`。允许"尽力而为"的实现（内存视图 + 内部记录持久化错误，如 coding-agent 的
  `AuthStorage`）。
- `read` 注释：可能读到已过期凭证；用于显示/状态；请求用 auth 来自 `Models.getAuth()`。
- `list` 注释：**不得**解析密钥，**不得**执行配置的 API-key 命令（`!command`）。
- `modify` 注释：`fn` 能看到当前凭证（刷新、刷新中登录都依赖）；返回新凭证或 `undefined`（保持不变）；
  按 provider id 互斥，跨进程亦如此（file lock）；解析为写后凭证；`fn` 的 rejection 原样传播。
- `delete` 注释：实现必须与 `modify` 串行化。

```ts
export interface AuthContext {
	env(name: string): Promise<string | undefined>;
	/** Check whether a file exists. Supports a leading `~`. Always false in browsers. */
	fileExists(path: string): Promise<boolean>;
}

export interface AuthResult {
	auth: ModelAuth;
	/** Provider-scoped environment/config values resolved from credentials and ambient context. */
	env?: ProviderEnv;
	/** Human-readable label for status UI: "ANTHROPIC_API_KEY", "OAuth", "~/.aws/credentials". */
	source?: string;
}

export interface AuthCheck {
	source?: string;
	type: "api_key" | "oauth";
}

export type AuthType = "api_key" | "oauth";
```

`AuthPrompt`（登录期间的提示；`signal` 用于外部事件胜出时取消该提示，例如 `manual_code` 与回调服务器竞速，
回调胜出时 abort 提示）：

```ts
export type AuthPrompt = { signal?: AbortSignal } & (
	| { type: "text"; message: string; placeholder?: string }
	| { type: "secret"; message: string; placeholder?: string }
	| { type: "select"; message: string; options: readonly { id: string; label: string; description?: string }[] }
	| { type: "manual_code"; message: string; placeholder?: string }
);

export interface AuthInfoLink {
	url: string;
	label?: string;
}

export type AuthEvent =
	| { type: "info"; message: string; links?: readonly AuthInfoLink[] }
	| { type: "auth_url"; url: string; instructions?: string }
	| {
			type: "device_code";
			userCode: string;
			verificationUri: string;
			/** Offer best-effort browser opening on user confirmation. Defaults to true; clients may ignore it. */
			openBrowserOnConfirm?: boolean;
			/** Offer copying of userCode on user confirmation, replacing the clipboard. Defaults to true. */
			copyCodeOnConfirm?: boolean;
			intervalSeconds?: number;
			expiresInSeconds?: number;
	  }
	| { type: "progress"; message: string };
```

`AuthInteraction` / `ProviderAuthInteraction`：

```ts
export interface AuthInteraction {
	signal?: AbortSignal;
	prompt(prompt: AuthPrompt): Promise<string>;   // select 返回 option id
	notify(event: AuthEvent): void;                // 同步、无返回
}
export type ProviderAuthInteraction = AuthInteraction & { signal: AbortSignal };
```

注释语义：`prompt()` 返回输入/选中的字符串；取消/中止时 reject；`signal` 中止整个登录流程；
单次提示的取消用 `AuthPrompt.signal`。`notify` 是同步的，实现不得让异常影响流程（Rust 侧建议
`&dyn AuthInteraction` 的 `notify(&self, event: &AuthEvent)`，异常忽略或记为诊断）。

`ApiKeyAuth`：

```ts
export interface ApiKeyAuth {
	name: string;                                   // 显示名，如 "Anthropic API key"
	login?(interaction: ProviderAuthInteraction): Promise<ApiKeyCredential>;
	check?(input: { ctx: AuthContext; credential?: ApiKeyCredential; signal: AbortSignal }): Promise<AuthCheck | undefined>;
	resolve(input: { ctx: AuthContext; credential?: ApiKeyCredential; signal: AbortSignal }): Promise<AuthResult | undefined>;
}
```

- `login` 缺省 = 仅有 ambient 来源（不可交互配置）。
- `check` 必须是**无副作用**的可用性检查；缺省时 `Models` 通过 `resolve` 判断。
- `resolve` 按字段合并（`credential.key ?? env("...")`、`credential.env?.NAME ?? env("...")`），
  `undefined` 表示未配置；解析是 provider 作用域的，模型级端点准备在 auth 解析之后。

`LoginOptions`：

```ts
export interface LoginOptions {
	getDeviceId?: () => string;   // 稳定安装 ID（如 OpenAI 的 agent host ID），首次使用时创建，之后恒定
	agentName?: string;           // app 自报名称（OpenAI agent name hint、Codex originator），默认 kiana 自己的名字
}
```

`OAuthAuth`：

```ts
export interface OAuthAuth {
	name: string;                  // 如 "Anthropic (Claude Pro/Max)"
	isSubscription?: boolean;      // 是否由 provider 订阅支撑
	loginLabel?: string;          // OAuth 登录选项的标签，如 "Sign in with SuperGrok or X Premium"
	login(interaction: ProviderAuthInteraction, options?: LoginOptions): Promise<OAuthCredential>;
	refresh(credential: OAuthCredential, signal: AbortSignal): Promise<OAuthCredential>;
	toAuth(credential: OAuthCredential): Promise<ModelAuth>;
}
```

注释要点：`refresh`/`toAuth` 拆分让 `Models` 拥有加锁刷新模式；`refresh` 是网络调用，失败即抛
（invalid_grant 等），`Models` 在 store 锁内运行它；`toAuth` 必须**无副作用**，覆盖 per-credential baseUrl
（GitHub Copilot）；声明为 async 以支持 lazy wrapper 首次加载实现。

`ProviderAuth`：`{ apiKey?: ApiKeyAuth; oauth?: OAuthAuth }`；注释要求至少一个存在（即便 ambient 或无 key
的本地服务也要提供 `apiKey`，其 `resolve()` 报告 provider 是否已配置）。

### 2.2 `src/auth/context.ts`（45 行）

```ts
export function defaultProviderAuthContext(): AuthContext
```

实现细节：

- `getProcessEnv()`：读 `(globalThis as {process?: {env?}}).process?.env`；浏览器返回 `undefined`。
- `env(name)`：取 `process.env[name]`；**仅当** `typeof value === "string" && value.trim().length > 0` 时返回原值
  （**不做 trim**，返回含首尾空白的原字符串），否则 `undefined`。空串、纯空白、非字符串均视为未设置。
- `fileExists(path)`：
  1. `await importNodeModule("node:fs/promises")`（**变量 specifier** `const importNodeModule = (specifier: string) => import(specifier)`）；
  2. 若 `resolved.startsWith("~")`：`const os = await importNodeModule("node:os")`；`resolved = os.homedir() + resolved.slice(1)`
     （字符串拼接，**不是** path.join；因此 `~/x` → `<home>/x`，`~x` → `<home>x`）；
  3. `await fs.access(resolved)` → `true`；任何异常 → `false`（含模块加载失败、权限、不存在）。
- 每次都重新动态 import（Node 有模块缓存，但 Rust 侧可直接用 `tokio::fs::metadata`/`std::fs::metadata`，
  需保留 `~` 展开与"任何错误即 false"的语义）。
- 浏览器/非 Node 环境：`process` 不存在 → env 恒 `undefined`；`fileExists` 因 import 失败恒 `false`。

Rust 侧：`pub struct DefaultProviderAuthContext`，`env` 读 `std::env::var_os` 后按上述规则过滤；
`file_exists` 用 `std::path` 处理 `~`（拼接 `dirs::home_dir()`），`tokio::fs::metadata(path).await.is_ok()`。
注意 `~` 判定用 `starts_with('~')` 而不是 `~` 后接 `/`。

### 2.3 `src/auth/helpers.ts`（59 行）

```ts
export function envApiKeyAuth(name: string, envVars: readonly string[]): ApiKeyAuth
export function lazyOAuth(input: { name: string; isSubscription?: boolean; loginLabel?: string; load: () => Promise<OAuthAuth> }): OAuthAuth
```

**`envApiKeyAuth(name, envVars)`** 返回的对象：

- `name`
- `login(interaction)`：
  1. `interaction.signal.throwIfAborted();`
  2. `const key = await interaction.prompt({ type: "secret", message: `Enter ${name}` });`
     （无 placeholder；`name` 原样插入，例如 `"Enter Anthropic API key"`）
  3. `interaction.signal.throwIfAborted();`
  4. 返回 `{ type: "api_key", key }`（即使用户输入空串也照样返回）。
- `resolve({ ctx, credential, signal })`：
  1. `signal.throwIfAborted();`
  2. 若 `credential?.key` **真值**（非空串）→ `{ auth: { apiKey: credential.key }, env: credential.env, source: "stored credential" }`
     （注意即使 `credential.env` 为 `undefined`，也会显式带 `env: undefined`，Rust 侧等价于 `None`）。
  3. 依次对 `envVars` 调 `ctx.env(envVar)`，每次之后 `signal.throwIfAborted()`；第一个真值 → `{ auth: { apiKey: value }, source: envVar }`。
  4. 全部未命中 → `undefined`。
- 注意：`envApiKeyAuth` **不**看 `credential.env` 中的值来构造 apiKey（那是 provider 自定义 `resolve` 的职责）。

与本单元九个流程相关的 provider 工厂中，使用 `envApiKeyAuth` 的（ai-core 单元实现，name 逐字）：
`github-copilot`（`["COPILOT_GITHUB_TOKEN"]`，name `"GitHub Copilot token"`）、`openai`（`["OPENAI_API_KEY"]`，
name `"OpenAI API key"`）、`openrouter`（`["OPENROUTER_API_KEY"]`，name `"OpenRouter API key"`）、
`kimi-coding`（`["KIMI_API_KEY"]`，name `"Kimi API key"`）、`meta`（`["META_API_KEY"]`，name `"Meta Model API key"`）、
`xai`（`["XAI_API_KEY"]`，name `"xAI API key"`）、`radius`（`["RADIUS_API_KEY"]`，name `"Radius API key"`）。

例外（**不要**把它们算进 `envApiKeyAuth`，否则登录提示与解析优先级都会错）：

- `anthropic` **不用** `envApiKeyAuth`，而是在 `providers/anthropic.ts` 里自写 `anthropicApiKeyAuth()`：
  name `"Anthropic API key"`；`login` 用 `secret` prompt `"Enter Anthropic API key"`（与 `envApiKeyAuth` 逐字相同）；
  `resolve` 顺序为 ① `credential?.key` 真值 → `{auth:{apiKey}, env: credential.env, source:"stored credential"}`；
  ② `ANTHROPIC_AUTH_TOKEN` → **`{ auth: { headers: { Authorization: \`Bearer ${value}\` } }, source: "ANTHROPIC_AUTH_TOKEN" }`**（不是 apiKey！）；
  ③ `ANTHROPIC_OAUTH_TOKEN` → `{auth:{apiKey}, source:"ANTHROPIC_OAUTH_TOKEN"}`；④ `ANTHROPIC_API_KEY` → `{auth:{apiKey}, source:"ANTHROPIC_API_KEY"}`；
  ⑤ workload identity federation：依次要求 `ANTHROPIC_FEDERATION_RULE_ID`、`ANTHROPIC_ORGANIZATION_ID`、
  `ANTHROPIC_IDENTITY_TOKEN_FILE` **全部存在**（任一缺失即 `undefined`），再可选收集 `ANTHROPIC_SERVICE_ACCOUNT_ID`、
  `ANTHROPIC_WORKSPACE_ID`，返回 `{ auth: {}, env: <collected>, source: "workload identity federation" }`。
  每一步之间都有 `signal.throwIfAborted()`。**注意 `getEnvApiKey("anthropic")` 会剔除 `ANTHROPIC_AUTH_TOKEN`**（见 §7.13）。
- `openai-codex` 的 `auth` **只有 `oauth`，没有 `apiKey`**：该 provider 上的显式 apiKey override 会被 §2.5 步骤 3 忽略，
  `checkProviderAuth` 在无 apiKey handler 时也只认 oauth（见 §5.3）。
- 顺带说明：整个 `packages/ai` 里共有 **36** 个 provider 文件调用 `envApiKeyAuth`（本单元只关心上面 7 个）。

**`lazyOAuth(input)`**：返回 `OAuthAuth`，其中：

- `name` / `isSubscription` / `loginLabel` 直接取自 input；
- 内部 `let promise; const loaded = () => (promise ??= input.load());` —— **memoize 首次 load 的 Promise**
  （即使 load 失败也不重试，失败的 Promise 会被复用）；
- `login: async (interaction, options) => (await loaded()).login(interaction, options)`
- `refresh: async (credential, signal) => (await loaded()).refresh(credential, signal)`
- `toAuth: async (credential) => (await loaded()).toAuth(credential)`
- 三个方法各自在调用时才触发加载；并发首次调用共享同一 Promise。

Rust 侧：`OnceCell`/`tokio::sync::OnceCell<Arc<dyn OAuthAuth>>` 或 `Mutex<Option<Arc<...>>>`，
**必须**保持"失败不复位"（用 `OnceCell::get_or_try_init` 语义会重试，故应改为 `get_or_init` + 结果缓存）。
`login/refresh/toAuth` 在 Rust 中为 `async fn`，动态分发用 `#[async_trait]` 或返回 `Pin<Box<dyn Future>>`。

### 2.4 `src/auth/credential-store.ts`（67 行）

```ts
export class InMemoryCredentialStore implements CredentialStore {
	private credentials = new Map<string, Credential>();
	private chains = new Map<string, Promise<unknown>>();
}
```

**`private enqueue<T>(providerId, task: () => Promise<T>, options?): Promise<T>`** —— 逐字算法：

```ts
const signal = operationSignal(options?.signal);
const previous = this.chains.get(providerId) ?? Promise.resolve();
const queued = (async () => {
	await previous.catch(() => {});
	signal.throwIfAborted();
	return task();
})();
const tail = queued.catch(() => {});
this.chains.set(providerId, tail);
void tail.then(() => {
	if (this.chains.get(providerId) === tail) this.chains.delete(providerId);
});
return raceWithAbortSignal(queued, signal);
```

要点（顺序不可变）：

1. `operationSignal(undefined)` 返回一个**永不中止**的新 signal（`new AbortController().signal`）。
2. 等待前一个链尾（吞掉其错误），然后 `throwIfAborted()`，**然后才运行 task**。
3. 链尾存的是 `queued.catch(() => {})`，因此一个失败任务不会阻塞后续任务。
4. 只有当链尾仍是自己时才删除 map 条目（防止误删后来者）。
5. 返回 `raceWithAbortSignal(queued, signal)`：signal 已中止 → 立即 reject `abortReason(signal)`，
   并 `void queued.catch(()=>{})` 保证后续 rejection 被处理；否则竞速，abort 时 reject（任务继续跑但结果被丢弃）。
   因此"排队的写操作被取消后**不会**在稍后执行"（`models-runtime.test.ts` 的
   "cancels queued credential mutations without running them later" 断言 `secondRan === false`）。

**`read(providerId, options?)`**：`options?.signal?.throwIfAborted(); return this.credentials.get(providerId);`
（返回同一对象引用，不 clone；Rust 侧可返回 clone，但要注意 `Models` 层可能会修改 `credential.env` 合并
override —— 见 2.5 使用 `{ ...stored, env: {...} }` 而非原地改，故 clone 安全）。

**`list(options?)`**：`options?.signal?.throwIfAborted(); return [...this.credentials].map(([providerId, credential]) => ({ providerId, type: credential.type }));`
（Map 迭代顺序 = 首次插入顺序；更新已有 key **不改变**顺序，删除后重插会排到末尾。）

**`modify(providerId, fn, options?)`**：

```ts
return this.enqueue(providerId, async () => {
	const current = this.credentials.get(providerId);
	const next = await fn(current);
	options?.signal?.throwIfAborted();
	if (next !== undefined) this.credentials.set(providerId, next);
	return next ?? current;
}, options);
```

注意：`fn` 之后、写入之前再检查一次 abort；若被中止，`fn` 的结果被丢弃且**不写入**（异常向上冒泡，
enqueue 返回的 promise 会 reject，但队列里 `queued` 已 reject → 被 `tail` 吞掉）。

**`delete(providerId, options?)`**：`enqueue(providerId, async () => { this.credentials.delete(providerId); }, options)`
（`Map.delete` 幂等）。

Rust 侧设计：`InMemoryCredentialStore { inner: tokio::sync::Mutex<StoreState> }`，其中
`StoreState { credentials: IndexMap<String, Credential>, chains: HashMap<String, JoinHandle<()>...> }`。
纯 Rust 下无法直接照搬 promise chain，需要：
- 每个 provider 一个 `tokio::sync::Mutex<()>` 作为串行门（或 `Mutex<VecDeque>` 队列）；
- `enqueue` 语义 = 获取串行门（等待期可被 signal 中止）→ 检查 abort → 运行 task → 检查 abort → 写入；
- **关键**：abort 只丢弃"等待者"，不打断正在运行的任务；被取消的排队任务不得执行（在拿到门之后立刻检查 abort）。
- `chains` 的清理含义只是内存回收，Rust 侧用 `Weak` 或按需清理 `HashMap` 即可（对外不可观测）。

### 2.5 `src/auth/resolve.ts`（217 行）

```ts
export { ModelsError, type ModelsErrorCode } from "../utils/models-error.ts";

export interface AuthResolutionOverrides {
	apiKey?: string;
	env?: ProviderEnv;
	/** Require this much remaining OAuth-token validity; defaults to five minutes. */
	minOAuthValidityMs?: number;
	signal?: AbortSignal;
}

export function resolveProviderAuth(
	provider: { id: string; auth: ProviderAuth },
	credentials: CredentialStore,
	authContext: AuthContext,
	overrides?: AuthResolutionOverrides,
): Promise<AuthResult | undefined>

export async function refreshStoredOAuthCredential(
	credentials: CredentialStore,
	providerId: string,
	oauth: OAuthAuth,
	needsRefresh: (credential: OAuthCredential) => boolean,
	signal: AbortSignal,
): Promise<OAuthCredential | undefined>
```

常量（**非导出**，但数值是契约）：`DEFAULT_OAUTH_MINIMUM_VALIDITY_MS = 5 * 60 * 1000`（300000）、
`DEFAULT_OAUTH_REFRESH_TIMEOUT_MS = 15_000`。

**`resolveProviderAuth(provider, credentials, authContext, overrides?)`**：

```ts
const signal = operationSignal(overrides?.signal);
return raceWithAbortSignal(resolveProviderAuthWithSignal(...), signal);
```

即：发起内部异步解析后用 `raceWithAbortSignal` 对调用方 signal 竞速（内部不因 abort 而停止，
但调用方立即拿到 `AbortError`，且内部 rejection 被吞掉）。

**`resolveProviderAuthWithSignal`** 的精确优先级（顺序即语义）：

1. `signal.throwIfAborted();`
2. `const requestAuthContext = overrides?.env ? overlayEnvAuthContext(authContext, overrides.env) : authContext;`
   `overlayEnvAuthContext` 返回 `{ env: async (name) => env[name] || (await base.env(name)), fileExists: (path) => base.fileExists(path) }`
   —— **JS 真值**：override 中空串/`undefined` 会回落到 base；注意 `||` 而非 `??`。
3. **`overrides.apiKey !== undefined && provider.auth.apiKey`** → `resolveApiKey(requestAuthContext, provider.auth.apiKey,
   provider.id, { type: "api_key", key: overrides.apiKey, env: overrides.env }, signal)`。
   注意条件是 `!== undefined`，空串也算"提供"；且**仅当 provider 有 apiKey auth 时**才走这条（纯 OAuth provider
   的显式 apiKey override 被忽略）。
4. `const stored = await readCredential(credentials, provider.id, signal);`
5. 若 `stored`：
   - `stored.type === "oauth" && provider.auth.oauth` → `resolveStoredOAuth(credentials, provider.id, provider.auth.oauth, stored, signal, overrides?.minOAuthValidityMs)`
   - `stored.type === "api_key" && provider.auth.apiKey` → 若 `overrides?.env`：`credential = { ...stored, env: { ...stored.env, ...overrides.env } }`，
     否则原样；然后 `resolveApiKey(requestAuthContext, provider.auth.apiKey, provider.id, credential, signal)`
   - 否则（类型无匹配 handler）→ **`undefined`**（绝不回退 env）
6. 无 stored → `provider.auth.apiKey ? resolveApiKey(requestAuthContext, provider.auth.apiKey, provider.id, undefined, signal) : undefined`
   （ambient：env vars、AWS profiles、ADC 文件）。

**`refreshStoredOAuthCredential(credentials, providerId, oauth, needsRefresh, signal)`** —— 逐字：

```ts
let post: Credential | undefined;
const lockWait = new AbortController();
const cancelLockWait = () => lockWait.abort(signal.reason);
signal.addEventListener("abort", cancelLockWait, { once: true });
if (signal.aborted) cancelLockWait();
try {
	post = await credentials.modify(
		providerId,
		async (current) => {
			signal.removeEventListener("abort", cancelLockWait);
			signal.throwIfAborted();
			if (current?.type !== "oauth") return undefined; // logged out meanwhile
			if (!needsRefresh(current)) return undefined;    // another process/request refreshed
			try {
				return await oauth.refresh(current, AbortSignal.timeout(DEFAULT_OAUTH_REFRESH_TIMEOUT_MS));
			} catch (error) {
				throw new ModelsError("oauth", `OAuth refresh failed for ${providerId}`, { cause: error });
			}
		},
		{ signal: lockWait.signal },
	);
} catch (error) {
	if (error instanceof ModelsError) throw error;
	signal.throwIfAborted();
	throw new ModelsError("auth", `Credential store modify failed for ${providerId}`, { cause: error });
} finally {
	signal.removeEventListener("abort", cancelLockWait);
}
return post?.type === "oauth" ? post : undefined;
```

语义要点：

- `lockWait` 是**只用于等待锁**的派生 signal：调用方 abort 会以 `signal.reason` 中止它；
  因此排队等待可被取消。但**一旦进入 `fn`**，先 `signal.removeEventListener("abort", cancelLockWait)`
  —— 之后调用方的 abort **不再**影响刷新，刷新与其持久化只受 15s 超时约束（`AbortSignal.timeout(15_000)`，
  reason 为 `TimeoutError` DOMException）。
- `fn` 内的顺序：移除监听 → `throwIfAborted`（其实已移除监听，此处仍可能因 signal 早已 abort 而抛）→
  类型检查 → `needsRefresh` 二次检查（**锁内**，因此并发只刷新一次）→ `oauth.refresh(current, timeoutSignal)`。
- 刷新失败 → `ModelsError("oauth", "OAuth refresh failed for <providerId>", { cause })`。
- 外层 catch：`ModelsError` 原样抛；否则 `signal.throwIfAborted()`（若调用方已 abort，抛 AbortError 而非包装错误），
  再抛 `ModelsError("auth", "Credential store modify failed for <providerId>", { cause })`。
- 返回：仅当 `post` 是 oauth 时为该凭证，否则 `undefined`（意味着"期间被 logout / 被改成别的类型"）。

**`resolveStoredOAuth(credentials, providerId, oauth, stored, signal, minOAuthValidityMs?)`**（内部）：

```ts
const minimumValidityMs = Math.max(DEFAULT_OAUTH_MINIMUM_VALIDITY_MS, minOAuthValidityMs ?? 0);
const expiresSoon = (credential) => Date.now() + minimumValidityMs >= credential.expires;
let credential = stored;
if (expiresSoon(credential)) {
	const post = await refreshStoredOAuthCredential(credentials, providerId, oauth, expiresSoon, signal);
	if (!post) return undefined;
	credential = post;
	if (minOAuthValidityMs !== undefined && expiresSoon(credential)) {
		throw new ModelsError("oauth", `OAuth refresh returned a token that expires too soon for ${providerId}`);
	}
}
try {
	return { auth: await oauth.toAuth(credential), source: "OAuth" };
} catch (error) {
	throw new ModelsError("oauth", `OAuth auth derivation failed for ${providerId}`, { cause: error });
}
```

- `expiresSoon` 用 `>=`（恰好等于 now+5min 视为即将过期）。
- `minOAuthValidityMs` 只**提高**下限（`Math.max`），且仅当显式给出时才在刷新后复查（provider 契约外的
  强约束，如 `print-bearer-token` 的 `--min-expiry`）。
- 成功结果 `source` 恒为字面量 `"OAuth"`。

**`resolveApiKey(authContext, apiKey, providerId, credential, signal)`**（内部）：
`try { return await apiKey.resolve({ ctx: authContext, credential, signal }); } catch (error) { throw new ModelsError("auth", `API key auth failed for provider ${providerId}`, { cause: error }); }`

**`readCredential(credentials, providerId, signal)`**（内部）：
`try { return await credentials.read(providerId, { signal }); } catch { throw new ModelsError("auth", `Credential store read failed for ${providerId}`, { cause: error }); }`

**`ModelsError` 的消息拼接**（`utils/models-error.ts`，逐字）：

```ts
export type ModelsErrorCode = "model_source" | "model_validation" | "provider" | "stream" | "auth" | "oauth";
export class ModelsError extends Error {
	readonly code: ModelsErrorCode;
	constructor(code, message, options?: { cause?: unknown }) {
		super(withCauseDetail(message, options?.cause), options);
		this.name = "ModelsError";
		this.code = code;
	}
}
function withCauseDetail(message: string, cause: unknown): string {
	if (cause === undefined || cause === null) return message;
	const detail = formatThrownValue(cause).trim();
	if (!detail || message.includes(detail)) return message;
	return `${message}: ${detail}`;
}
```

`formatThrownValue(value)`：`value instanceof Error` → `value.message || value.name`；string → 原样；
否则 `String(value)`。因此向上暴露的错误文案形如：
`OAuth refresh failed for anthropic: OpenAI Codex token refresh failed (401): {...}`。
**`message.includes(detail)` 去重**必须保留（避免 "X failed: X failed" 这类重复）。

Rust 侧设计要点：
- `ModelsError` 是 `struct { code: ModelsErrorCode, message: String, cause: Option<Box<dyn Error>> }`，
  `Display` 输出已拼接消息；`std::error::Error::source()` 返回 cause。
- `resolve_provider_auth` 需 `#[async_trait]`；`overrides` 用 `AuthResolutionOverrides { api_key: Option<String>, env: Option<IndexMap<String,String>>, min_oauth_validity_ms: Option<i64>, signal: Option<AbortSignal> }`。
- 注意 Rust 无法表达"内部继续跑、外部立即取消"的 `raceWithAbortSignal`：可用 `tokio::select!`
  + `tokio::spawn` 持有内部 future（abort 时让 task 继续但丢弃 JoinHandle），或用
  `AbortSignal` 的 `Notify` 竞速并在 drop 时保留 task。详见 6.2 第 18 条。

### 2.6 `src/auth/oauth/pkce.ts`（34 行）

```ts
function base64urlEncode(bytes: Uint8Array): string   // 内部
export async function generatePKCE(): Promise<{ verifier: string; challenge: string }>
```

- `base64urlEncode`：按字节逐个 `String.fromCharCode(byte)` 拼成 binary 字符串（Latin-1 视角），
  `btoa(binary)` 后 `.replace(/\+/g,"-").replace(/\//g,"_").replace(/=/g,"")`（**去掉所有** `=` 填充）。
- `generatePKCE()`：
  1. `const verifierBytes = new Uint8Array(32); crypto.getRandomValues(verifierBytes);`
  2. `verifier = base64urlEncode(verifierBytes)` → 32 字节 → **43 个字符**（`43 = ceil(32*8/6)`，无填充）。
  3. `challenge = base64urlEncode(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))))`
     → SHA-256(verifier 的 UTF-8 字节) → 32 字节 → 43 字符。
- 结果满足 RFC 7636 S256；`challenge` 可用 `SHA256(verifier)` 的 base64url 独立验证（测试里就是这么做的）。
- 随机源必须是密码学安全的（Node `crypto.getRandomValues`）。Rust 用 `rand::rngs::OsRng` 或 `getrandom`。

### 2.7 `src/auth/oauth/device-code.ts`（98 行）——RFC 8628 轮询

常量（**非导出**，逐字）：

```ts
const CANCEL_MESSAGE = "Login cancelled";
const TIMEOUT_MESSAGE = "Device flow timed out";
const SLOW_DOWN_TIMEOUT_MESSAGE =
	"Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.";
const MINIMUM_INTERVAL_MS = 1000;
const DEFAULT_POLL_INTERVAL_SECONDS = 5;
const SLOW_DOWN_INTERVAL_INCREMENT_MS = 5000;
```

类型：

```ts
type OAuthDeviceCodeIncompletePollResult =
	| { status: "pending" }
	| { status: "slow_down"; intervalSeconds?: number }
	| { status: "failed"; message: string };

export type OAuthDeviceCodePollResult<T> = OAuthDeviceCodeIncompletePollResult | { status: "complete"; value: T };

export type OAuthDeviceCodePollOptions<T> = {
	intervalSeconds?: number;
	expiresInSeconds?: number;
	waitBeforeFirstPoll?: boolean;
	poll: () => Promise<OAuthDeviceCodePollResult<T>>;
	signal: AbortSignal;
};
```

**`export function abortableSleep(ms: number, signal: AbortSignal, cancelMessage: string): Promise<void>`**：

```ts
return new Promise((resolve, reject) => {
	if (signal.aborted) { reject(new Error(cancelMessage)); return; }
	const onAbort = () => { clearTimeout(timeout); reject(new Error(cancelMessage)); };
	const timeout = setTimeout(() => { signal.removeEventListener("abort", onAbort); resolve(); }, ms);
	signal.addEventListener("abort", onAbort, { once: true });
});
```

要点：中止时抛 `new Error(cancelMessage)`（**普通 Error**，name 为 `"Error"`，不是 AbortError）；
定时器触发时移除监听（避免泄漏）；`ms` 为 0 时下一 tick 即 resolve。

**`export async function pollOAuthDeviceCodeFlow<T>(options): Promise<T>`** —— 逐字算法：

```ts
const deadline = typeof options.expiresInSeconds === "number"
	? Date.now() + options.expiresInSeconds * 1000
	: Number.POSITIVE_INFINITY;
let intervalMs = Math.max(MINIMUM_INTERVAL_MS,
	Math.floor((options.intervalSeconds ?? DEFAULT_POLL_INTERVAL_SECONDS) * 1000));

let slowDownResponses = 0;
if (options.waitBeforeFirstPoll) {
	const remainingMs = deadline - Date.now();
	if (remainingMs > 0) {
		await abortableSleep(Math.min(intervalMs, remainingMs), options.signal, CANCEL_MESSAGE);
	}
}

while (Date.now() < deadline) {
	if (options.signal.aborted) throw new Error(CANCEL_MESSAGE);
	const result = await options.poll();
	if (result.status === "complete") return result.value;
	if (result.status === "failed") throw new Error(result.message);
	if (result.status === "slow_down") {
		slowDownResponses += 1;
		intervalMs = typeof result.intervalSeconds === "number"
			&& Number.isFinite(result.intervalSeconds)
			&& result.intervalSeconds > 0
				? Math.max(MINIMUM_INTERVAL_MS, Math.floor(result.intervalSeconds * 1000))
				: Math.max(MINIMUM_INTERVAL_MS, intervalMs + SLOW_DOWN_INTERVAL_INCREMENT_MS);
	}
	const remainingMs = deadline - Date.now();
	if (remainingMs <= 0) break;
	await abortableSleep(Math.min(intervalMs, remainingMs), options.signal, CANCEL_MESSAGE);
}

throw new Error(slowDownResponses > 0 ? SLOW_DOWN_TIMEOUT_MESSAGE : TIMEOUT_MESSAGE);
```

必须逐条对齐的边界：

1. 无 `expiresInSeconds` → deadline 为 `+∞`（永不因超时结束，只可能被 signal 或 poll 结果终止）。
2. `intervalSeconds` 为 0/负数/NaN → `Math.floor` 后再 `Math.max(1000, ...)`；例如 0 → 1000ms；
   `undefined` → 5000ms。**默认值来自 `??`**，因此 `null` 也会走默认（TS 类型不允许 null）。
3. `waitBeforeFirstPoll`：仅在 `deadline - now > 0` 时睡 `min(intervalMs, remaining)`；否则不睡。
4. 循环条件在**每次迭代开始**检查 `Date.now() < deadline`；因此 poll 一次后才可能因超时退出。
5. poll 抛错 → 直接向上传播（不重试、不包装）。
6. `slow_down` 后若服务端给 `intervalSeconds`（有限且 >0）→ 用它（最小 1000ms），否则 `+5000ms`（最小 1000ms）。
   **计数 `slowDownResponses` 用于最终选择超时文案**（只要出现过 ≥1 次 slow_down，超时文案就是长版本）。
7. 睡眠前重算 `remainingMs`，若 ≤0 则 `break`（不睡）。
8. 睡眠用 `min(intervalMs, remainingMs)`，因此最后一次睡眠可能短于 interval。
9. 超时文案：`slowDownResponses > 0` → `SLOW_DOWN_TIMEOUT_MESSAGE`，否则 `TIMEOUT_MESSAGE`。

测试时间线（`oauth-device-code.test.ts`，fake timers，起点 `2026-03-09T00:00:00Z`）：
- `intervalSeconds: 2`，无 `waitBeforeFirstPoll` → poll 于 t=0，然后 t=+2000 第二次（complete）。
- 同参数 + `waitBeforeFirstPoll` → 首次 poll 于 t=+2000。
- `intervalSeconds: 2` + one slow_down（无 interval）→ 第二次 poll 于 +7000（= 2000 + 5000）。
- slow_down 带 `intervalSeconds: 30` → 第二次 poll 于 +30000。
- 中止于 in-flight 等待 → reject `"Login cancelled"`。

Rust 侧：用 `tokio::time::sleep` + `tokio::select!` 或 `abortable::Abortable`；dealine 用
`Option<Instant>`（None = 无限）。`Date.now()` 语义是**墙钟**毫秒（`SystemTime::now().duration_since(UNIX_EPOCH)`，
f64 精度足够；测试用 fake clock 需可注入）。

### 2.8 `src/auth/oauth/callback-server.ts`（183 行）

```ts
export interface OAuthCallbackServerOptions<T> {
	providerName: string;      // 浏览器页面上的 provider 名，如 "OpenAI"
	host: string;              // 监听地址
	port: number;              // 0 = 任意空闲端口
	path: string;
	redirectHost?: string;     // 与 host 不同的 redirectUri 主机，如 "localhost"
	state?: string;            // 期望的 state；provider 不发 state 时省略
	complete: (code: string) => Promise<T>;   // 在返回网页之前完成登录（能显示交换失败）
	signal?: AbortSignal;
	timeoutMs?: number;
}

export interface OAuthCallbackServer<T> {
	redirectUri: string;
	wait(): Promise<T | undefined>;  // complete 的结果，或 cancel() 后 undefined
	cancel(): void;                  // 停止等待浏览器，除非回调已在完成中
	close(): void;
}

export async function startOAuthCallbackServer<T>(options): Promise<OAuthCallbackServer<T>>
export async function waitForCallbackOrManualInput<T>(
	interaction: ProviderAuthInteraction,
	callback: OAuthCallbackServer<T> | undefined,
	prompt: { message: string; placeholder: string },
): Promise<{ type: "callback"; value: T } | { type: "manual"; input: string }>
```

`sendPage(response, status, html)`：`response.writeHead(status, { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" }); response.end(html);`
（Node 实际发出的响应头集合还含 `connection: keep-alive`、`date`、`keep-alive: timeout=5`、`transfer-encoding: chunked`，
Rust 侧若用 hyper/axum 无法逐字一致 —— 见 6.2 第 20 条。）

**`startOAuthCallbackServer`** 精确流程：

1. `if (signal?.aborted) throw new Error("Login cancelled");`（同步抛出，发生在任何 socket 创建之前；
   `openrouter-oauth.test.ts` 断言此时不 emit 任何事件）。
2. 建 `waitPromise`（resolve/reject 由 `finish` 驱动），并 `waitPromise.catch(() => undefined)` 防止未观察的 rejection
   触发 unhandledRejection。
3. 状态：`claimed = false`、`settled = false`、`timer`。
   `finish(result)`：`if (settled) return; settled = true; clearTimeout(timer); signal?.removeEventListener("abort", onAbort);`
   然后 reject/resolve。`onAbort = () => finish({ error: new Error("Login cancelled") })`。
4. `createServer(handler)`，handler 内是 async IIFE（`void (async () => {...})()`），逐个请求判断：
   1. `const url = new URL(request.url ?? "/", "http://localhost");`
   2. `if (request.method !== "GET" || url.pathname !== options.path)` → **404** + `oauthErrorHtml("Callback route not found.")`（POST 也 404）。
   3. `if (options.state !== undefined && url.searchParams.get("state") !== options.state)` → **400** + `oauthErrorHtml("State mismatch.")`。
   4. `if (claimed || settled)` → **409** + `oauthErrorHtml("This sign-in has already been handled.")`。
   5. `const error = url.searchParams.get("error"); if (error)`：
      `const description = url.searchParams.get("error_description") ?? error;`
      → **400** + `oauthErrorHtml(`${providerName} authorization failed.`, description)`，
      然后 `finish({ error: new Error(`${providerName} authorization failed: ${description}`) })`。
   6. `const code = url.searchParams.get("code"); if (!code)` → **400** + `oauthErrorHtml("Missing authorization code.")`（**不**结束 wait）。
   7. `claimed = true;` 然后 `try { const value = await options.complete(code); → 200 + oauthSuccessHtml(`Signed in to ${providerName}. You may now close this page.`); finish({ value }); } catch (error) { const failure = error instanceof Error ? error : new Error(String(error)); → 502 + oauthErrorHtml(`${providerName} sign-in failed.`, failure.message); finish({ error: failure }); }`
5. `await new Promise((resolve, reject) => { server.once("error", reject); server.listen(options.port, options.host, () => { server.off("error", reject); resolve(); }); })`
   —— 绑定失败（如 `EADDRINUSE`）**reject 原始 Node 错误**（带 `code: "EADDRINUSE"`），调用方据此决定回退。
6. `const address = server.address(); if (!address || typeof address === "string") { server.close(); throw new Error("OAuth callback server did not bind to TCP"); }`
7. `server.on("error", (error) => finish({ error }));`（绑定成功后的运行时错误转为 wait 的 rejection）。
8. `signal?.addEventListener("abort", onAbort, { once: true });`
9. `if (options.timeoutMs !== undefined) timer = setTimeout(() => finish({ error: new Error(`${providerName} sign-in timed out`) }), options.timeoutMs);`
10. `const redirectHost = options.redirectHost ?? options.host;`
    `redirectUri = `http://${redirectHost.includes(":") ? `[${redirectHost}]` : redirectHost}:${address.port}${options.path}``
    —— **含 `:` 的主机（IPv6）加方括号**；端口取**实际**绑定端口。
11. 返回 `{ redirectUri, wait: () => waitPromise, cancel: () => { if (!claimed) finish({ value: undefined }); }, close: () => { finish({ error: new Error("OAuth callback server closed") }); server.close(); } }`。

状态机要点：

- `cancel()` 只在 `!claimed` 时生效：已 claimed 的回调继续完成（`callback-server.test.ts` 的
  "A claimed callback keeps completing even when the caller switches to manual input"）。
- `cancel()` 之后迟到的回调 → 409（因为 `settled = true`）。
- `close()` 会在 wait 尚未 settle 时以 `"OAuth callback server closed"` reject（各流程的 `finally` 里调用，
  通常此时已 settle，因此不可见）。
- 超时与 abort 消息：`${providerName} sign-in timed out`、`Login cancelled`。
- `complete()` 返回 `undefined`（例如 `async (code) => code` 但 code 为空不会发生）不会触发 `cancel` 语义，
  仍视为完成并 resolve `undefined` —— 这正是 `waitForCallbackOrManualInput` 判断"callback 未产生值"的方式。

**`waitForCallbackOrManualInput(interaction, callback, prompt)`** —— 逐字：

```ts
const manualAbort = new AbortController();
let manualError: Error | undefined;
const manual = interaction
	.prompt({ type: "manual_code", ...prompt, signal: manualAbort.signal })
	.then((input) => { callback?.cancel(); return input; })
	.catch((error: unknown) => { manualError = error instanceof Error ? error : new Error(String(error)); callback?.cancel(); return undefined; });
try {
	const value = await callback?.wait();
	if (manualError) throw manualError;
	if (value !== undefined) return { type: "callback", value };
	const input = await manual;
	if (manualError) throw manualError;
	return { type: "manual", input: input ?? "" };
} finally {
	manualAbort.abort();
}
```

要点：

- **同时**启动手动 prompt 与回调等待；两者竞速。
- 手动输入成功 → `callback.cancel()`（停止等待浏览器，若未 claimed）→ 返回 `{type:"manual", input}`。
- 手动 prompt 失败 → 记录 `manualError`、`callback.cancel()`，返回 undefined 使 promise 完成；
  随后若回调未产生值，`throw manualError`（因此"prompt 被取消"时登录以该错误结束）。
- 回调先到 → `await callback.wait()` 返回值；`manualError` 若已存在**优先抛出**（边界：prompt 先 reject 且
  callback 也恰好返回）。
- 回调 resolve `undefined`（被 cancel）→ 再 `await manual`（此时 manual 已被 cancel 触发完成/失败）。
- `finally` 里 `manualAbort.abort()`：不管谁胜出，都中止手动 prompt 的 signal（`AuthPrompt.signal`），
  TUI 侧据此收起输入框。
- `prompt` 对象展开在 `{ type: "manual_code", ...prompt, signal }` —— 即 `message`/`placeholder` 原样透传，
  **`signal` 覆盖为 manualAbort.signal**（调用方无法传入自己的 signal）。

测试（`oauth-callback-server.test.ts`，`describe.sequential`）断言：

- `redirectUri` 匹配 `/^http:\/\/127\.0\.0\.1:\d+\/callback$/`；`redirectHost: "localhost"` → `http://localhost:<port>/callback`。
- 404（错误 path、POST）、400（state 不匹配 → body 含 `State mismatch.`）、409（重复）、
  400（缺 code）、200 且 body 含 `Authentication successful`、`Signed in to Example.`、
  `fill="#F09082"`、`fill="#4D9ABF"`、`fill="#F1BE58"`。
- `complete` 抛错 → 502 且 body 含 `Example sign-in failed.` + 原始 message；`wait()` reject 同一 message。
- provider error → 400 且 body 含 `User denied access`；`wait()` reject `Example authorization failed: User denied access`。
- `cancel()` → wait resolve `undefined`；之后回调 409。
- abort → reject `Login cancelled`；`timeoutMs: 10` → reject `Example sign-in timed out`；
  预中止的 signal → `startOAuthCallbackServer` 直接 reject `Login cancelled`。
- 端口被占用 → `startOAuthCallbackServer` reject 带 `code: "EADDRINUSE"`（**不**自动换端口）。
- `waitForCallbackOrManualInput`：回调胜出 → `{type:"callback", value}` 且 `prompt.signal` 已 abort；
  粘贴胜出 → `{type:"manual", input}` 且回调被取消；无 callback 时仅手动；prompt 失败向上传播。

### 2.9 `src/auth/oauth/load.ts`（80 行）

```ts
const importOAuthModule = (specifier: string): Promise<unknown> => {
	const runtimeSpecifier = import.meta.url.endsWith(".js") ? specifier.replace(/\.ts$/, ".js") : specifier;
	return import(runtimeSpecifier);
};

type OAuthFlowLoaders = {
	anthropic: () => OAuthAuth | Promise<OAuthAuth>;
	openaiCodex: () => OAuthAuth | Promise<OAuthAuth>;
	openaiChatGPT: () => OAuthAuth | Promise<OAuthAuth>;
	githubCopilot: () => OAuthAuth | Promise<OAuthAuth>;
	openrouter: () => OAuthAuth | Promise<OAuthAuth>;
	kimiCoding: () => OAuthAuth | Promise<OAuthAuth>;
	meta: () => OAuthAuth | Promise<OAuthAuth>;
	xai: () => OAuthAuth | Promise<OAuthAuth>;
	radius: (options: { name: string; gateway: string }) => OAuthAuth | Promise<OAuthAuth>;
};

let bundledLoaders: OAuthFlowLoaders | undefined;

export function registerBundledOAuthFlowLoaders(loaders: OAuthFlowLoaders): void  // bundledLoaders = loaders
```

九个 loader（每个都是 `export const`）：

| loader | 无 bundled 时动态 import 的模块 | 取其导出 |
|---|---|---|
| `loadAnthropicOAuth()` | `./anthropic.ts` | `.anthropicOAuth` |
| `loadOpenAICodexOAuth()` | `./openai-codex.ts` | `.openaiCodexOAuth` |
| `loadOpenAIChatGPTOAuth()` | `./openai-chatgpt.ts` | `.openaiChatGPTOAuth` |
| `loadGitHubCopilotOAuth()` | `./github-copilot.ts` | `.githubCopilotOAuth` |
| `loadOpenRouterOAuth()` | `./openrouter.ts` | `.openRouterOAuth` |
| `loadKimiCodingOAuth()` | `./kimi-coding.ts` | `.kimiCodingOAuth` |
| `loadMetaOAuth()` | `./meta.ts` | `.metaOAuth` |
| `loadXaiOAuth()` | `./xai.ts` | `.xaiOAuth` |
| `loadRadiusOAuth({name, gateway})` | `./radius.ts` | `.createRadiusOAuth(options)` |

要点：

- `bundledLoaders` 存在时**优先**用它（Bun 单文件二进制场景），且由 loader 自己 `return bundledLoaders.X()`。
- `.ts`/`.js` 改写：`import.meta.url` 以 `.js` 结尾（构建产物）时把 specifier 的 `.ts` 换成 `.js`；
  否则保留 `.ts`（源码直跑，Bun/tsx）。
- 每次调用都会重新 `import()`（Node ESM 缓存保证模块只求值一次），**不**做结果缓存 —— 缓存由 `lazyOAuth` 负责。
- `registerBundledOAuthFlowLoaders` 是**可复写**的全局（后注册覆盖先注册）。

Rust 侧：不需要动态 import；`load_*` 直接返回 `Arc::new(AnthropicOAuth)` 或从 `OnceCell` 取；
`register_bundled_oauth_flow_loaders` 可用 `OnceCell<Box<dyn Fn..>>` 模拟（但 Rust 中该机制无实际用途，
建议保留 API 形状：`set_bundled_oauth_flow_loaders(loaders)`，`None` 时回落到内置实现）。
若为了与 TS 行为一致（"失败不缓存"）而需要，注意 `lazyOAuth` 的 memoize 语义在 Rust 侧同样适用。

### 2.10 `src/auth/oauth/anthropic.ts`（307 行）

常量（逐字）：

```ts
const decode = (s: string) => atob(s);
const CLIENT_ID = decode("OWQxYzI1MGEtZTYxYi00NGQ5LTg4ZWQtNTk0NGQxOTYyZjVl");   // = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
const AUTHORIZE_URL = "https://claude.ai/oauth/authorize";
const TOKEN_URL = "https://platform.claude.com/v1/oauth/token";
const CALLBACK_HOST = getProviderEnvValue("KIANA_OAUTH_CALLBACK_HOST") || "127.0.0.1";   // 模块加载时求值一次
const CALLBACK_PORT = 53692;
const CALLBACK_PATH = "/callback";
const REDIRECT_URI = `http://localhost:${CALLBACK_PORT}${CALLBACK_PATH}`;
const COPY_CODE_REDIRECT_URI = "https://platform.claude.com/oauth/code/callback";
const ANTHROPIC_BROWSER_LOGIN_METHOD = "browser";
const ANTHROPIC_COPY_CODE_LOGIN_METHOD = "copy_code";
const SCOPES = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
```

（`CALLBACK_HOST` 在**模块初始化**时读取环境变量，之后不再变；单测用 `vi.stubEnv` 必须在 import 前设置。
Rust 侧建议在第一次使用时读取以规避该陷阱，但若要逐字一致则仍应在构造 `AnthropicOAuth` 时读取。）

**`parseAuthorizationInput(input: string): { code?: string; state?: string }`**（内部）：

```ts
const value = input.trim();
if (!value) return {};
try { const url = new URL(value); return { code: url.searchParams.get("code") ?? undefined, state: url.searchParams.get("state") ?? undefined }; } catch {}
if (value.includes("#")) { const [code, state] = value.split("#", 2); return { code, state }; }
if (value.includes("code=")) { const params = new URLSearchParams(value); return { code: params.get("code") ?? undefined, state: params.get("state") ?? undefined }; }
return { code: value };
```

- 顺序很重要：先按**绝对 URL** 解析（`new URL` 只接受绝对 URL，相对串会抛）；再按 `#` 切分（`code#state`，
  `split("#", 2)` 只在**第一个** `#` 处切，剩余 `#` 归入 state）；再按 `code=...` 形式（`URLSearchParams` 解析
  `a=1&b=2` 风格串，注意 `+` 被解码为空格）；否则整个值当 code。
- `state` 为 `""`（空串）时：`url.searchParams.get` 返回 `""`，经 `?? undefined` 保留为 `""`（不是 undefined）；
  调用方用 truthiness 判断（`parsed.state && parsed.state !== verifier`），空串视为"无 state"。

**`formatErrorDetails(error: unknown): string`**（内部）：Error 时拼 `${error.name}: ${error.message}`，
然后依次追加 `code=<code>`（若真值）、`errno=<String(errno)>`（若 `typeof !== "undefined"`）、
`cause=${formatErrorDetails(error.cause)}`（若 `typeof error.cause !== "undefined"`）、`stack=<stack>`（若有），
以 `"; "` 连接；非 Error 用 `String(error)`。

**`postJson(url, body: Record<string, string|number>, signal): Promise<string>`**（内部）：

```ts
const response = await fetch(url, {
	method: "POST",
	headers: { "Content-Type": "application/json", Accept: "application/json" },
	body: JSON.stringify(body),
	signal: AbortSignal.any([signal, AbortSignal.timeout(30_000)]),
});
const responseBody = await response.text();
if (!response.ok) throw new Error(`HTTP request failed. status=${response.status}; url=${url}; body=${responseBody}`);
return responseBody;
```

（无 `response.text()` 的 catch：读取 body 失败会直接冒泡。）

**`exchangeAuthorizationCode(code, state, verifier, redirectUri, signal)`**（内部）：POST JSON
`{grant_type:"authorization_code", client_id, code, state, redirect_uri, code_verifier}`（**键顺序如此**，
JSON.stringify 保序）；异常包装逐字：

- 请求失败：`` `Token exchange request failed. url=${TOKEN_URL}; redirect_uri=${redirectUri}; response_type=authorization_code; details=${formatErrorDetails(error)}` ``
- JSON 解析失败：`` `Token exchange returned invalid JSON. url=${TOKEN_URL}; body=${responseBody}; details=${formatErrorDetails(error)}` ``

返回 `{ type:"oauth", refresh: tokenData.refresh_token, access: tokenData.access_token, expires: Date.now() + tokenData.expires_in * 1000 - 5 * 60 * 1000 }`
（**减 5 分钟**；没有字段校验 —— 缺字段会得到 `undefined`，序列化后字段缺失）。

**`refreshAnthropicToken(refreshToken, signal)`**（内部）：POST JSON `{grant_type:"refresh_token", client_id, refresh_token}`
（**不含 `scope`**，测试断言 `expect(body.scope).toBeUndefined()`）；错误文案：
`` `Anthropic token refresh request failed. url=${TOKEN_URL}; details=${formatErrorDetails(error)}` ``、
`` `Anthropic token refresh returned invalid JSON. url=${TOKEN_URL}; body=${responseBody}; details=${formatErrorDetails(error)}` ``。
返回同上（过期 −5min）。

**`loginAnthropic(interaction)`**（browser 模式）：

1. `generatePKCE()`。
2. `startCallbackServer(port)` 包装 `startOAuthCallbackServer({ providerName: "Anthropic", host: CALLBACK_HOST, port, path: CALLBACK_PATH, redirectHost: "localhost", state: verifier, complete: async (code) => code, signal: interaction.signal })`
   —— **`state` 用 verifier**（Anthropic 把 PKCE verifier 同时当 state）；`complete` 原样返回 code（交换延后）。
3. `const callback = await startCallbackServer(53692).catch(() => startCallbackServer(0)).catch(() => undefined);`
   —— 端口被占 → 回退到任意空闲端口；再失败 → 无回调服务器（只能粘贴）。
   **重要边界（原文遗漏）**：`startOAuthCallbackServer` 在 `signal.aborted` 时会**同步**抛 `"Login cancelled"`，
   而这里两层 `.catch()` 会把它**一并吞掉**得到 `undefined`。因此"调用前就已 abort"的情形下，Anthropic 浏览器流程
   **不会**立即 reject，而是继续 `redirectUri = REDIRECT_URI` → notify `auth_url` → 进入 `waitForCallbackOrManualInput`
   （此时 `interaction.prompt` 拿到的 signal 已 abort，TUI 通常立刻 reject，但那取决于宿主实现）。
   Rust 侧必须照抄这个"吞掉预取消错误"的行为，否则取消时序与事件序列会与 TS 不同。
4. `const redirectUri = callback?.redirectUri ?? REDIRECT_URI;`（回退时用固定 `http://localhost:53692/callback`，
   即便那个端口不可用 —— 测试 "keeps the localhost redirect_uri for manual callback login"）。
5. 构造授权 URL：`new URLSearchParams({ code: "true", client_id, response_type: "code", redirect_uri: redirectUri, scope: SCOPES, code_challenge: challenge, code_challenge_method: "S256", state: verifier })`
   → `notify({ type: "auth_url", url: `${AUTHORIZE_URL}?${authParams.toString()}`, instructions: "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here." })`。
6. `waitForCallbackOrManualInput(interaction, callback, { message: "Complete login in your browser, or paste the authorization code / redirect URL here:", placeholder: redirectUri })`。
7. callback → `code = result.value`，`state = verifier`；manual → `parseAuthorizationInput`，若
   `parsed.state && parsed.state !== verifier` → `throw new Error("OAuth state mismatch")`；`code = parsed.code`；
   `state = parsed.state ?? verifier`。
8. `if (!code) throw new Error("Missing authorization code");`
9. `notify({ type: "progress", message: "Exchanging authorization code for tokens..." });`
10. `return await exchangeAuthorizationCode(code, state, verifier, redirectUri, interaction.signal);`
11. `finally { callback?.close(); }`

**`loginAnthropicCopyCode(interaction)`**：

1. `generatePKCE()`；授权参数同上但 `redirect_uri = COPY_CODE_REDIRECT_URI`。
2. `notify({ type:"auth_url", url, instructions: "Complete login in your browser, then copy the code Anthropic shows and paste it here." })`。
3. `prompt({ type:"manual_code", message: "Paste the code Anthropic shows after you sign in:", placeholder: "code#state", signal: interaction.signal })`
   —— **注意 signal 是整个登录的 signal**（不是派生）。
4. `parseAuthorizationInput`；state 校验同上（`OAuth state mismatch`）；缺 code → `Missing authorization code`。
5. progress + `exchangeAuthorizationCode(parsed.code, parsed.state ?? verifier, verifier, COPY_CODE_REDIRECT_URI, interaction.signal)`。

**`anthropicOAuth: OAuthAuth`**：

```ts
{
	name: "Anthropic (Claude Pro/Max)",
	isSubscription: true,
	async login(interaction) {
		const method = await interaction.prompt({
			type: "select",
			message: "Select Anthropic login method:",
			options: [
				{ id: "browser", label: "Browser login (default)" },
				{ id: "copy_code", label: "Copy code login (headless)" },
			],
		});
		if (method === "copy_code") return loginAnthropicCopyCode(interaction);
		if (method !== "browser") throw new Error(`Unknown Anthropic login method: ${method}`);
		return loginAnthropic(interaction);
	},
	refresh: (credential, signal) => refreshAnthropicToken(credential.refresh, signal),
	async toAuth(credential) { return { apiKey: credential.access }; },
}
```

`loginLabel` **未设置**（undefined）。select 的 prompt **没有** `signal` 字段。

测试要点（`anthropic-oauth.test.ts`）：浏览器流程的 `redirect_uri` 来自 `callback.redirectUri`；
粘贴模式 exchange 的 `redirect_uri` 为 `http://localhost:53692/callback`（即使回调服务器在跑，也用它自己的
redirectUri）；copy_code 的 redirect_uri 为 `https://platform.claude.com/oauth/code/callback`；
端口 53692 被占时回退，且 `redirectUri.hostname === "localhost"`、port ≠ 53692；页面 200 含 `Signed in to Anthropic.`；
`select` 被取消（prompt 抛 `Login cancelled`）时 login reject 该错误。

### 2.11 `src/auth/oauth/openai-codex.ts`（438 行）

模块顶部（**不是** top-level import，防止浏览器/Vite 构建失败）：

```ts
let _randomBytes: typeof import("node:crypto").randomBytes | null = null;
if (typeof process !== "undefined" && (process.versions?.node || process.versions?.bun)) {
	import("node:crypto").then((m) => { _randomBytes = m.randomBytes; });
}
```

常量（逐字）：

```ts
const CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTH_BASE_URL = "https://auth.openai.com";
const AUTHORIZE_URL = `${AUTH_BASE_URL}/oauth/authorize`;
const TOKEN_URL = `${AUTH_BASE_URL}/oauth/token`;
const REDIRECT_URI = "http://localhost:1455/auth/callback";
const DEVICE_USER_CODE_URL = `${AUTH_BASE_URL}/api/accounts/deviceauth/usercode`;
const DEVICE_TOKEN_URL = `${AUTH_BASE_URL}/api/accounts/deviceauth/token`;
const DEVICE_VERIFICATION_URI = `${AUTH_BASE_URL}/codex/device`;
const DEVICE_REDIRECT_URI = `${AUTH_BASE_URL}/deviceauth/callback`;
const DEVICE_CODE_TIMEOUT_SECONDS = 15 * 60;
const OPENAI_CODEX_BROWSER_LOGIN_METHOD = "browser";
const OPENAI_CODEX_DEVICE_CODE_LOGIN_METHOD = "device_code";
const SCOPE = "openid profile email offline_access";
const JWT_CLAIM_PATH = "https://api.openai.com/auth";
```

类型：`OAuthToken = { access: string; refresh: string; expires: number }`、`TokenOperation = "exchange" | "refresh"`、
`DeviceAuthInfo = { deviceAuthId; userCode; intervalSeconds: number }`、
`DeviceTokenSuccess = { authorizationCode; codeVerifier }`、
`JwtPayload = { [JWT_CLAIM_PATH]?: { chatgpt_account_id?: string }; [key: string]: unknown }`。

- `getCallbackHost()`：`getProviderEnvValue("KIANA_OAUTH_CALLBACK_HOST") || "127.0.0.1"`（**每次调用**读取）。
- `createState()`：`if (!_randomBytes) throw new Error("OpenAI Codex OAuth is only available in Node.js environments");`
  `return _randomBytes(16).toString("hex")` → 32 位小写十六进制。
- `parseAuthorizationInput`：与 2.10 的同名函数**完全相同**（含 `#` 与 `code=` 分支）。
- `decodeJwt(token)`：`const parts = token.split("."); if (parts.length !== 3) return null;`
  `const decoded = atob(parts[1] ?? ""); return JSON.parse(decoded)`；任何异常 → `null`。
  注意用 **`atob`**（不接受 base64url 的 `-`/`_`）：含 `-`/`_` 的 payload 会抛 `InvalidCharacterError` → `null`。
- `fetchWithLoginCancellation(input, init)`：`try { return await fetch(...) } catch (e) { if (init.signal?.aborted) throw new Error("Login cancelled"); throw e; }`
  （把 fetch 的中止错误统一成 `Login cancelled`）。
- `readTokenResponse(response, operation)`：
  - `!response.ok` → `` `OpenAI Codex token ${operation} failed (${response.status}): ${text || response.statusText}` ``，
    其中 `text = await response.text().catch(() => "")`。
  - `const rawJson = await response.json();` —— **没有 try/catch**：响应体不是合法 JSON 时抛**原始 `SyntaxError`**
    （`Unexpected token ... in JSON at position ...`），不会被包装成 `OpenAI Codex token ...` 文案。
  - 若 `!json?.access_token || !json.refresh_token || typeof json.expires_in !== "number"`
    → `` `OpenAI Codex token ${operation} response missing fields: ${JSON.stringify(json)}` ``。
  - 返回 `{ access, refresh, expires: Date.now() + expires_in*1000 }`（**不**减余量；这是唯一不减的流程，
    因为上游 token 生命周期短且下游还有 5min 刷新阈值）。
- `exchangeAuthorizationCode(code, verifier, redirectUri, signal)`：POST `application/x-www-form-urlencoded`
  `{grant_type:"authorization_code", client_id, code, code_verifier, redirect_uri}`（键顺序如此），
  经 `fetchWithLoginCancellation` → `readTokenResponse(response, "exchange")`。
- `refreshAccessToken(refreshToken, signal)`：POST form `{grant_type:"refresh_token", refresh_token, client_id}`；
  fetch 抛错 → `` `OpenAI Codex token refresh error: ${error.message || String(error)}` ``；然后 `readTokenResponse(response, "refresh")`。
  刷新**不写 stderr**（测试断言 `console.error` 未被调用）。
- `startOpenAICodexDeviceAuth(signal)`：POST JSON `{client_id}` 到 `DEVICE_USER_CODE_URL`；
  - `!ok` 且 `status === 404` → `"OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL."`
  - `!ok` 其他 → `` `OpenAI Codex device code request failed with status ${status}${body ? `: ${body}` : ""}` ``
  - 解析：`const intervalSeconds = typeof json?.interval === "string" ? Number(json.interval.trim()) : json?.interval;`
    校验 `!json?.device_auth_id || !json.user_code || typeof intervalSeconds !== "number" || !Number.isFinite(intervalSeconds) || intervalSeconds < 0`
    → `` `Invalid OpenAI Codex device code response: ${JSON.stringify(json)}` ``。
  - 返回 `{ deviceAuthId, userCode, intervalSeconds }`（**允许 0**）。
- `pollOpenAICodexDeviceAuth(device, signal)`：`pollOAuthDeviceCodeFlow<DeviceTokenSuccess>({ intervalSeconds, expiresInSeconds: DEVICE_CODE_TIMEOUT_SECONDS, signal, poll })`
  （**无 `waitBeforeFirstPoll`** → 立即首轮）。poll 每次 POST JSON `{device_auth_id, user_code}`：
  - `response.ok`：解析 `{ authorization_code?, code_verifier? }`；缺任一 → `{status:"failed", message: `Invalid OpenAI Codex device auth token response: ${JSON.stringify(json)}`}`；
    否则 `{status:"complete", value:{ authorizationCode, codeVerifier }}`。
  - `status === 403 || status === 404` → `{status:"pending"}`。
  - 其他：读 text（失败 → `""`），尝试 `JSON.parse` 取 `json?.error`（对象取 `.code`，字符串原样）；
    `deviceauth_authorization_pending` → pending；`slow_down` → slow_down（无 interval）；
    否则 `{status:"failed", message: `OpenAI Codex device auth failed with status ${status}${body ? `: ${body}` : ""}`}`。
- `createAuthorizationFlow(originator = "kiana")`：`generatePKCE()` + `createState()`；`new URL(AUTHORIZE_URL)` 后依次
  `searchParams.set`：`response_type=code`、`client_id`、`redirect_uri`（REDIRECT_URI）、`scope`、`code_challenge`、
  `code_challenge_method=S256`、`state`、`id_token_add_organizations=true`、`codex_cli_simplified_flow=true`、
  `originator`；返回 `{ verifier, state, url: url.toString() }`。
- `getAccountId(accessToken)`：`decodeJwt` → `payload?.["https://api.openai.com/auth"]?.chatgpt_account_id`；
  非空字符串才返回，否则 null。
- `credentialsFromToken(token)`：`accountId = getAccountId(token.access)`；缺失 →
  `throw new Error("Failed to extract accountId from token")`；返回
  `{ type:"oauth", access, refresh, expires, accountId }`。
- `exchangeAuthorizationCodeForCredentials(code, verifier, redirectUri, signal)`：`credentialsFromToken(await exchangeAuthorizationCode(...))`。
- `loginOpenAICodexDeviceCode(interaction)`：设备授权 → notify
  `{type:"device_code", userCode, verificationUri: DEVICE_VERIFICATION_URI, intervalSeconds, expiresInSeconds: DEVICE_CODE_TIMEOUT_SECONDS}`
  → poll → `exchangeAuthorizationCodeForCredentials(code.authorizationCode, code.codeVerifier, DEVICE_REDIRECT_URI, signal)`。
- `loginOpenAICodex(interaction, options?)`：`createAuthorizationFlow(options?.agentName)`（**undefined 时用默认 "kiana"**，
  因为默认参数只在 `undefined` 时生效）；`startOAuthCallbackServer({providerName:"OpenAI", host:getCallbackHost(), port:1455,
  path:"/auth/callback", state, complete: async (code) => code, signal}).catch(() => undefined)`（端口被占则无服务器；
  **同样会吞掉预取消的 `"Login cancelled"`**——与 §2.10 步骤 3 相同，预取消时仍会 notify `auth_url` 并进入粘贴提示）；
  notify `{type:"auth_url", url, instructions: "A browser window should open. Complete login to finish."}`；
  `waitForCallbackOrManualInput(..., { message: "Complete login in your browser, or paste the authorization code / redirect URL here:", placeholder: REDIRECT_URI })`；
  manual 时 `parseAuthorizationInput`，`if (parsed.state && parsed.state !== state) throw new Error("State mismatch")`
  （**文案是 "State mismatch"，不是 "OAuth state mismatch"**）；缺 code → `Missing authorization code`；
  `exchangeAuthorizationCodeForCredentials(code, verifier, REDIRECT_URI, signal)`；`finally { callback?.close(); }`。
  注意 browser 流程**不** notify progress。
- `refreshOpenAICodexToken(refreshToken, signal)`：`credentialsFromToken(await refreshAccessToken(...))`
  —— 因此刷新**要求**新 access token 含 accountId claim，否则 `Failed to extract accountId from token`。

`openaiCodexOAuth`：

```ts
{
	name: "OpenAI (ChatGPT Plus/Pro)",
	isSubscription: true,
	async login(interaction, options) {
		const method = await interaction.prompt({
			type: "select",
			message: "Select OpenAI Codex login method:",
			options: [
				{ id: "browser", label: "Browser login (default)" },
				{ id: "device_code", label: "Device code login (headless)" },
			],
		});
		if (method === "device_code") return loginOpenAICodexDeviceCode(interaction);
		if (method !== "browser") throw new Error(`Unknown OpenAI Codex login method: ${method}`);
		return loginOpenAICodex(interaction, options);
	},
	refresh: (credential, signal) => refreshOpenAICodexToken(credential.refresh, signal),
	async toAuth(credential) { return { apiKey: credential.access }; },
}
```

无 `loginLabel`。

测试要点（`openai-codex-oauth.test.ts`）：设备流程首轮 poll 在 **t=0**（无 waitBeforeFirstPoll），
第二次在 +5000；notify 的 `expiresInSeconds` 恒为 900；exchange 的 `redirect_uri` 为
`https://auth.openai.com/deviceauth/callback`；browser 流程粘贴回调的 `redirect_uri` 为 `http://localhost:1455/auth/callback`；
`originator` 来自 `options.agentName`；403/404 视为 pending；token 刷新失败文案含原始 body 且不写 stderr；
端口 1455 被占时回退粘贴；15 分钟超时；取消 → `Login cancelled`。

### 2.12 `src/auth/oauth/openai-chatgpt.ts`（309 行）——Sign in with ChatGPT

常量（逐字）：

```ts
const DYNAMIC_CLIENT_ID = "dynamic_agent_client";     // 每次登录注册新 client；OpenAI 在回调里返回签发的 client_id
const AGENT_NAME_HINT = "Kiana";
const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const AUTHORIZE_URL = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE = "https://api.openai.com/v1";
const CALLBACK_HOST = getProviderEnvValue("KIANA_OAUTH_CALLBACK_HOST") || "127.0.0.1";   // 模块初始化时求值
const CALLBACK_PORT = 1455;
const CALLBACK_PATH = "/auth/callback";
const REDIRECT_URI = `http://127.0.0.1:${CALLBACK_PORT}${CALLBACK_PATH}`;
const DIRECT_TOKEN_SCOPE = "chatgpt.tokens.use.direct";
const SCOPE = `openid profile email offline_access resource.invoke ${DIRECT_TOKEN_SCOPE}`;
const EXPIRY_MARGIN_MS = 3 * 60 * 1000;
```

本文件**自带**一套回调服务器（不复用 `callback-server.ts`），差异必须保留：

- 成功/失败页头是 `{ "Content-Type": "text/html; charset=utf-8" }`（**大写 C，且没有 cache-control**）。
- 状态码：路由错 404、provider error 400、回调解析错误 400、成功 200、handler 内未捕获异常 500。
- **不检查 HTTP 方法**（原文未写）：handler 只做 `const url = new URL(request.url || "", REDIRECT_URI);` 与
  `if (url.pathname !== CALLBACK_PATH)`，POST/PUT 只要 path 对也会被当成回调处理（通用服务器是 `method !== "GET" || ...` → 404）。
- **解析失败时 result promise 保持 pending**（原文未写）：`authorizationResultFromCallback` 抛错的 catch 只
  `sendHtml(response, 400, oauthErrorHtml(message))` 然后 `return`，**不** reject `result`、也不 resolve；
  因此浏览器看到错误页，而 `Promise.race([callback.result, manualCode])` 仍卡在回调分支上（只能靠 manual prompt 或
  abort 收场）。`error` 查询参数分支则会 `rejectResult`（`ChatGPT authorization failed: ${error}`）。
- 每个请求的整个 handler 体包在一个同步 `try/catch` 里，**任何**同步异常 → 500 +
  `oauthErrorHtml("Internal error while processing the callback.")`。
- `sendHtml` 无 `cache-control`，也**不** `response.end` 之前设置 Content-Length（chunked）。
- 页面文案：`oauthErrorHtml("Callback route not found.")`、`oauthErrorHtml("ChatGPT was not connected.", `Error: ${error}`)`、
  `oauthErrorHtml(message)`（message 来自解析错误）、`oauthSuccessHtml("ChatGPT authentication completed. You can close this window.")`、
  `oauthErrorHtml("Internal error while processing the callback.")`。
- 监听失败（`EADDRINUSE` 等）通过 `server.once("error", reject)` 传播**原始 Node 错误**；
  监听成功后改为 `server.on("error", rejectResult)`（运行时错误 → result promise reject）。

函数：

- `randomValue(): string` = `randomBytes(32).toString("base64url")`（**Node base64url**，43 字符，无填充）。
- `authorizationResultFromCallback(url, expectedState): {code, clientId}`：
  1. `code` 缺失 → `throw new Error("Missing authorization code")`
  2. `state` 缺失 → `"Missing OAuth state"`
  3. `state !== expectedState` → `"OAuth state mismatch"`
  4. `clientId = url.searchParams.get("client_id")?.trim()`；空 → `"OpenAI OAuth registration callback did not contain an issued client ID"`
- `authorizationResultFromManualInput(input, expectedState)`：
  1. `new URL(input.trim())` 失败 → `"Paste the full callback URL from the browser"`
  2. `expected = new URL(REDIRECT_URI)`；`url.origin !== expected.origin || url.pathname !== expected.pathname`
     → `` `The pasted callback URL must start with ${REDIRECT_URI}` ``
  3. `error` 参数存在 → `` `ChatGPT authorization failed: ${error}` ``
  4. 否则 `authorizationResultFromCallback(url, expectedState)`
- `requestToken(body: URLSearchParams, signal): Promise<TokenResponse>`：POST `TOKEN_URL`，
  headers `{ accept: "application/json", "content-type": "application/x-www-form-urlencoded" }`；
  `!ok` → `` `OpenAI OAuth token request failed (${response.status}): ${responseBody || response.statusText}` ``；
  响应 JSON 不是（非数组的）对象 → `"OpenAI OAuth token response must be an object"`。
- `requireTokenString(value, field)`：非字符串或 `trim().length === 0` → `` `OpenAI OAuth token response has invalid ${field}` ``（field ∈ access_token | refresh_token | scope）。
- `credentialFromTokenResponse(token, clientId)`：
  - access/refresh/scope 经 `requireTokenString`；
  - `typeof expires_in !== "number" || !Number.isFinite || <= 0` → `"OpenAI OAuth token response has invalid expires_in"`；
  - `scopes = scope.trim().split(/\s+/).filter(Boolean)`；不含 `chatgpt.tokens.use.direct`
    → `` `OpenAI OAuth grant did not include ${DIRECT_TOKEN_SCOPE}` ``；
  - 返回 `{ type:"oauth", access, refresh, expires: Date.now() + expires_in*1000 - EXPIRY_MARGIN_MS, clientId, scopes }`
    （**减 3 分钟**，字段顺序 access/refresh/expires/clientId/scopes）。
- `exchangeAuthorizationCode(code, verifier, clientId, signal)`：form body **键顺序**
  `grant_type=authorization_code, client_id=<签发的 clientId>, code, code_verifier, redirect_uri=REDIRECT_URI, resource=RESOURCE`；
  然后要求 `token.id_token` 为非空字符串（否则 `"OpenAI OAuth token response did not contain an ID token"`；
  该 id_token **不**被使用，只做契约检查）；最后 `credentialFromTokenResponse(token, clientId)`。
- `refreshAccessToken(credential, signal)`：`credential.clientId` 必须为非空字符串，否则
  `"Stored OpenAI OAuth credential does not contain an issued client ID; reconnect ChatGPT"`；
  form body **键顺序** `grant_type=refresh_token, client_id, refresh_token, resource`（**无 scope**）；
  `credentialFromTokenResponse(token, clientId)`（新 scopes 覆盖旧的，clientId 保持）。
- `agentHostId(deviceId)`: `!deviceId || !UUID_PATTERN.test(deviceId)` →
  `"Sign in with ChatGPT requires a device ID (UUID) for this installation"`；返回 `` `urn:uuid:${deviceId.toLowerCase()}` ``。
- `loginOpenAIChatGPT(interaction, options?)`：
  1. `hostId = agentHostId(options?.getDeviceId?.())`（**在创建服务器之前**，测试断言此时尚未发授权请求）；
  2. `generatePKCE()`；`state = randomValue()`；`nonce = randomValue()`；
  3. `startCallbackServer(state).catch(e => { if (!(e instanceof Error && "code" in e && e.code === "EADDRINUSE")) throw e; throw new Error(`Port ${CALLBACK_PORT} is in use, probably by an unfinished login in another kiana session or by the Codex CLI. Cancel that login and try again.`); })`
     —— **不**回退到别的端口（否则回调会打到别的进程）；
  4. 授权 URL 参数 **键顺序**：`client_id=dynamic_agent_client`、`agent_name_hint=<options.agentName ?? "Kiana">`、
     `ext_agent_host_id=urn:uuid:<lowercased uuid>`、`response_type=code`、`redirect_uri`、`resource`、`scope`、`state`、
     `code_challenge`、`code_challenge_method=S256`、`nonce`；
  5. `notify({type:"auth_url", url, instructions: "Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here."})`；
  6. `manualAbort = new AbortController()`；`manualCode = interaction.prompt({ type:"manual_code",
     message: "Complete login in your browser, or paste the final redirect URL here:", placeholder: REDIRECT_URI,
     signal: AbortSignal.any([manualAbort.signal, interaction.signal]) }).then(input => authorizationResultFromManualInput(input, state))`
     —— **prompt 与回调并行**，但用 `Promise.race` 而非 `waitForCallbackOrManualInput`；
  7. `const result = await Promise.race([callback.result, manualCode]);`
     `notify({type:"progress", message: "Exchanging authorization code for tokens..."});`
     `return await exchangeAuthorizationCode(result.code, verifier, result.clientId, interaction.signal);`
  8. catch：`if (interaction.signal.aborted) throw new Error("Login cancelled"); throw error;`
  9. finally：`manualAbort.abort(); callback.server.close(); callback.server.closeAllConnections();`
     （`closeAllConnections()` 必须保留：浏览器会预开空闲连接，若不强制关闭，下一次登录的回调可能被本服务器的
     残留连接接走并以 `OAuth state mismatch` 拒绝。）
- `openaiChatGPTOAuth`：`{ name: "OpenAI (ChatGPT subscription)", isSubscription: true, loginLabel: "Sign in with ChatGPT",
  login: loginOpenAIChatGPT, refresh: refreshAccessToken, async toAuth(c) { return { apiKey: c.access }; } }`。

测试要点（`openai-chatgpt-oauth.test.ts`）：授权 URL 的 `client_id` 恒为 `dynamic_agent_client`、
`agent_name_hint` 默认 `Kiana`、`ext_agent_host_id` 为 `urn:uuid:<uuid>`、`scope` 为完整 SCOPE、`redirect_uri` 固定；
兑换时 `client_id` 用回调里签发的 ID；凭证含 `clientId` 与 `scopes` 数组；无 deviceId/非 UUID → reject 且**未**启动授权；
未授予 direct scope → reject；refresh 缺 `refresh_token` → `token response has invalid refresh_token`；
refresh 用凭证的 clientId、无 scope 参数、expires 约 now+(3600−180)s。

### 2.13 `src/auth/oauth/github-copilot.ts`（507 行）

常量（逐字）：

```ts
const decode = (s: string) => atob(s);
const CLIENT_ID = decode("SXYxLmI1MDdhMDhjODdlY2ZlOTg=");   // = "Iv1.b507a08c87ecfe98"
const COPILOT_HEADERS = {
	"User-Agent": "GitHubCopilotChat/0.35.0",
	"Editor-Version": "vscode/1.107.0",
	"Editor-Plugin-Version": "copilot-chat/0.35.0",
	"Copilot-Integration-Id": "vscode-chat",
} as const;
const COPILOT_API_VERSION = "2026-06-01";
```

类型：`DeviceCodeResponse {device_code, user_code, verification_uri, interval?, expires_in}`、
`DeviceTokenSuccessResponse {access_token, token_type?, scope?}`、
`DeviceTokenErrorResponse {error, error_description?, interval?}`。

内部函数（全部必须逐字对齐）：

- `normalizeDomain(input): string | null`：`trim()`；空 → null；`trimmed.includes("://") ? new URL(trimmed) : new URL(`https://${trimmed}`)`；
  返回 `url.hostname`（**WHATWG 规范化**：小写、punycode、去掉端口与路径）；解析失败 → null。
- `getUrls(domain)`：`{ deviceCodeUrl: `https://${domain}/login/device/code`, accessTokenUrl: `https://${domain}/login/oauth/access_token`, copilotTokenUrl: `https://api.${domain}/copilot_internal/v2/token` }`。
- `getBaseUrlFromToken(token)`：`token.match(/proxy-ep=([^;]+)/)`；无 → null；`proxyHost.replace(/^proxy\./, "api.")`；
  返回 `https://${apiHost}`（**不**做 URL 规范化）。
- `getGitHubCopilotBaseUrl(token?, enterpriseDomain?)`：有 token 且解析成功 → 用之；否则有 enterpriseDomain →
  `https://copilot-api.${enterpriseDomain}`；否则 `"https://api.individual.githubcopilot.com"`。
- `parseGitHubCopilotModelCatalog(raw, allowPolicyFallback)`：
  1. `data = asRecord(raw)?.data`；非数组 → `throw new Error("Invalid Copilot models response")`；
  2. `accountModels`：对每个元素，`id` 必须是字符串（否则丢弃）；`capabilities.supports.tool_calls === false` → 丢弃；
     保留 `{ id, pickerEnabled: item.model_picker_enabled === true, policyState: asRecord(item.policy)?.state }`；
  3. `pickerModelIds = accountModels.filter(m => m.pickerEnabled && m.policyState !== "disabled").map(m => m.id)`；
  4. `usePolicyFallback = allowPolicyFallback && pickerModelIds.length === 0`；
  5. `availableModelIds = pickerModelIds.length > 0 || !allowPolicyFallback ? pickerModelIds : accountModels.filter(m => m.policyState === "enabled").map(m => m.id)`；
  6. `policyModelIds = accountModels.filter(m => m.policyState === "unconfigured" && Object.hasOwn(GITHUB_COPILOT_MODELS, m.id) && (m.pickerEnabled || usePolicyFallback)).map(m => m.id)`；
  7. 返回 `{ availableModelIds, policyModelIds }`。
  （`GITHUB_COPILOT_MODELS` 的键集合决定"已知模型"；Rust 侧需等价的内置模型 id 集合。）
- `fetchWithRateLimitRetry(url, init, signal, retryPolicy)`：
  - `retryBudgetSignal = maxRetries > 0 && maxElapsedMs > 0 ? AbortSignal.timeout(maxElapsedMs) : undefined`；
  - `requestSignal = retryBudgetSignal ? AbortSignal.any([signal, retryBudgetSignal]) : signal`；
  - `retryDeadline = retryBudgetSignal ? Date.now() + maxElapsedMs : undefined`；
  - 循环 `for (retry = 0; ; retry++)`：`fetch(url, {...init, signal: AbortSignal.any([requestSignal, AbortSignal.timeout(5000)])})`
    （**每个请求 5s 超时**）；`if (response.status !== 429 || retry === maxRetries) return response;`
  - `retryAfter = response.headers.get("retry-after")`；`delayMs = 500 * 2 ** retry`；
    若有 retry-after：`seconds = Number.parseFloat(retryAfter)`；`delayMs = Number.isNaN(seconds) ? Date.parse(retryAfter) - Date.now() : seconds * 1000`；
    `if (!Number.isFinite(delayMs)) return response;`；
  - `delayMs = Math.max(0, delayMs)`；`if (retryDeadline !== undefined && delayMs >= retryDeadline - Date.now()) return response;`
    （**延迟超出剩余预算就放弃重试并返回 429 响应**）；
  - `await response.body?.cancel();`（释放连接）→ `await sleep(delayMs, requestSignal);`
- `fetchGitHubCopilotModels(copilotToken, enterpriseDomain, signal, retryPolicy)`：`baseUrl = getGitHubCopilotBaseUrl(copilotToken, enterpriseDomain)`；
  `allowPolicyFallback = baseUrl === "https://api.individual.githubcopilot.com"`；
  `fetchWithRateLimitRetry(`${baseUrl}/models`, { headers: { Accept: "application/json", Authorization: `Bearer ${copilotToken}`, ...COPILOT_HEADERS, "X-GitHub-Api-Version": COPILOT_API_VERSION } }, ...)`；
  `!ok` → `` `${status} ${statusText}: ${await response.text()}` ``；否则 `parseGitHubCopilotModelCatalog(await response.json(), allowPolicyFallback)`。
- `fetchJson(url, init)`：`!ok` → `` `${status} ${statusText}: ${text}` ``；否则 `response.json()`。
- `startDeviceFlow(domain, signal)`：`fetchJson(deviceCodeUrl, { method:"POST", headers:{Accept:"application/json",
  "Content-Type":"application/x-www-form-urlencoded","User-Agent":"GitHubCopilotChat/0.35.0"},
  body: new URLSearchParams({client_id: CLIENT_ID, scope: "read:user"}), signal })`；
  - 非对象 → `"Invalid device code response"`；
  - 字段校验：`device_code`/`user_code`/`verification_uri` 必须 string、`interval` 若定义必须 number、`expires_in` 必须 number，
    否则 `"Invalid device code response fields"`；
  - `new URL(verificationUri)` 失败 → `"Untrusted verification_uri in device code response"`；
    `protocol !== "https:" && protocol !== "http:"` → 同一文案；
  - 返回 `{ device_code, user_code, verification_uri: parsedUri.href, interval, expires_in }`（**已规范化**）。
- `pollForGitHubAccessToken(domain, device, signal)`：`pollOAuthDeviceCodeFlow<string>({ intervalSeconds: device.interval,
  expiresInSeconds: device.expires_in, waitBeforeFirstPoll: true, signal, poll })`；
  poll：`fetchJson(accessTokenUrl, { method:"POST", headers:{Accept, "Content-Type": "application/x-www-form-urlencoded",
  "User-Agent": "GitHubCopilotChat/0.35.0"}, body: new URLSearchParams({client_id, device_code, grant_type: "urn:ietf:params:oauth:grant-type:device_code"}), signal })`；
  - 有 `access_token` string → complete；
  - 有 `error` string：`authorization_pending` → pending；`slow_down` → `{status:"slow_down", intervalSeconds: typeof interval === "number" ? interval : undefined}`；
    否则 `{status:"failed", message: `Device flow failed: ${error}${description ? `: ${description}` : ""}`}`；
  - 其他 → `{status:"failed", message: "Invalid device token response"}`。
- `refreshGitHubCopilotAccessToken(refreshToken, enterpriseDomain, signal)`：`domain = enterpriseDomain || "github.com"`；
  `fetchJson(copilotTokenUrl, { headers: { Accept: "application/json", Authorization: `Bearer ${refreshToken}`, ...COPILOT_HEADERS }, signal })`
  （**GET**）；非对象 → `"Invalid Copilot token response"`；`token` 必须 string、`expires_at` 必须 number，
  否则 `"Invalid Copilot token response fields"`；返回 `{ type:"oauth", refresh: refreshToken, access: token,
  expires: expiresAt * 1000 - 5 * 60 * 1000, enterpriseUrl: enterpriseDomain }`（`enterpriseUrl` 可能是 `undefined`，
  而 `{}` 中该键仍存在但值为 undefined → JSON 序列化时被丢弃）。
- `refreshGitHubCopilotToken(refreshToken, enterpriseDomain, signal)`：先 access，再
  `fetchGitHubCopilotModels(credentials.access, enterpriseDomain, signal, { maxRetries: 0, maxElapsedMs: 0 })`
  （**刷新不重试**，测试断言 throttling 时只请求一次），返回 `{ ...credentials, availableModelIds }`。
- `enableGitHubCopilotModel(token, modelId, enterpriseDomain, signal): Promise<boolean>`：
  `url = `${baseUrl}/models/${modelId}/policy``；`fetchWithRateLimitRetry(url, { method:"POST", headers: {
  "Content-Type":"application/json", Authorization: `Bearer ${token}`, ...COPILOT_HEADERS, "openai-intent": "chat-policy",
  "x-interaction-type": "chat-policy" }, body: JSON.stringify({ state: "enabled" }) }, signal, { maxRetries: 2, maxElapsedMs: 5000 })`；
  fetch 抛错时 `if (signal.aborted) throw; return false;`；`status === 429`（重试预算耗尽后仍是 429）→
  `` `${status} ${statusText}: ${await response.text()}` ``；否则返回 `response.ok`。
- `enableGitHubCopilotModels(token, modelIds, enterpriseDomain, signal)`：顺序遍历；成功则收集 id；
  抛错时 `if (signal.aborted) throw; break;`（**放弃剩余模型但保留已启用结果**）。
- `loginGitHubCopilot(interaction)`：
  1. `prompt({ type:"text", message: "GitHub Enterprise URL/domain (blank for github.com)", placeholder: "company.ghe.com" })`
     （**无 signal 字段**）；
  2. `if (interaction.signal.aborted) throw new Error("Login cancelled");`
  3. `enterpriseDomain = normalizeDomain(input)`；`trimmed && !enterpriseDomain` → `"Invalid GitHub Enterprise URL/domain"`；
     `domain = enterpriseDomain || "github.com"`；
  4. `startDeviceFlow(domain, signal)` → `notify({ type:"device_code", userCode, verificationUri, intervalSeconds: device.interval,
     expiresInSeconds: device.expires_in })`（**不传** openBrowserOnConfirm/copyCodeOnConfirm）；
  5. `pollForGitHubAccessToken` → `refreshGitHubCopilotAccessToken(githubAccessToken, enterpriseDomain ?? undefined, signal)`；
  6. `fetchGitHubCopilotModels(credentials.access, enterpriseDomain ?? undefined, signal, { maxRetries: 2, maxElapsedMs: 5000 })`；
  7. 若 `models.policyModelIds.length > 0`：`notify({type:"progress", message: "Enabling models..."})` 然后
     `enableGitHubCopilotModels(credentials.access, models.policyModelIds, enterpriseDomain ?? undefined, signal)`；
  8. 返回 `{ ...credentials, availableModelIds: [...new Set([...models.availableModelIds, ...enabledModelIds])] }`
     （**去重并保持首次出现顺序**）。
- `copilotEnterpriseDomain(credential)`：`credential.enterpriseUrl` 必须是字符串且非空；`normalizeDomain(...) ?? undefined`。
- `githubCopilotOAuth`：`{ name: "GitHub Copilot", isSubscription: true, login: loginGitHubCopilot,
  refresh: (credential, signal) => refreshGitHubCopilotToken(credential.refresh, copilotEnterpriseDomain(credential), signal),
  async toAuth(c) { return { apiKey: c.access, baseUrl: getGitHubCopilotBaseUrl(c.access, copilotEnterpriseDomain(c)) }; } }`
  —— 无 `loginLabel`。

`GITHUB_COPILOT_MODELS` 的角色：仅用于 `Object.hasOwn(GITHUB_COPILOT_MODELS, model.id)` 判断"是否为 kiana 内置模型"
（策略启用只针对已知模型；"remote-only-model" 被忽略）。Rust 侧需要等价的内置 id 集合（由 ai-core 的
`providers/github-copilot.models.ts` 提供）。

测试要点（`github-copilot-oauth.test.ts`，743 行，最全）：
- picker 目录过滤、空 picker 时 policy fallback（仅 Individual 端点）、非 Individual 不 fallback；
- 刷新时不重试目录限流（请求次数 == 1）；
- 设备码事件字段（interval/expiresIn）；
- 策略启用只针对"已知 + 支持工具 + unconfigured + pickerEnabled 或 policyFallback"；
- 429 + `Retry-After: 1` 后重试，时序为 t+1000 的第二次请求（fake timers 精确到 999/1 ms）；
- 传输失败后继续下一个模型；重试延迟超出预算（`Retry-After: 5` vs 5s 预算）→ 停止批处理但**仍持久化凭证**
  （`store.read("github-copilot")` 等于返回凭证）；
- 非 http(s) `verification_uri`（如 `$(id>/tmp/pwned)`）→ reject 且**不**触发 onDeviceCode；
- `verification_uri` 里的 ESC 字节被规范化为 `%1B`；
- 轮询时间线：+5000、+10000、+17000（服务端 slow_down interval=7）；
- 超时（expires_in=25）后文案为长版本的 `Device flow timed out after one or more slow_down responses...`。

### 2.14 `src/auth/oauth/openrouter.ts`（169 行）

常量：

```ts
const AUTHORIZE_URL = "https://openrouter.ai/auth";
const TOKEN_URL = "https://openrouter.ai/api/v1/auth/keys";
const LOGIN_TIMEOUT_MS = 5 * 60 * 1000;
const TOKEN_EXCHANGE_TIMEOUT_MS = 30_000;
```

- `getCallbackHost()`：`getProviderEnvValue("KIANA_OAUTH_CALLBACK_HOST") || "127.0.0.1"`（每次调用）。
- `parseAuthorizationInput(input): string | undefined`：trim；空 → undefined；`new URL(value)` 成功 →
  `searchParams.get("code") ?? undefined`；含 `code=` → `new URLSearchParams(value).get("code") ?? undefined`；
  否则整串（**trim 后**）作为 code。
- `errorDetail(body: JsonObject)`：依次 `error_description`(string) → `message`(string) → `error`(string) →
  `error`(非数组对象) 的 `.message`(string) → undefined。
- `exchangeAuthorizationCode(code, verifier, signal)`：
  1. `if (signal.aborted) throw new Error("Login cancelled");`
  2. 自建 `controller`；`signal` abort → `controller.abort(signal.reason)`；`setTimeout(() => controller.abort(new Error("OpenRouter OAuth token exchange timed out")), 30_000)`；
  3. fetch `TOKEN_URL`，headers `{ accept: "application/json", "content-type": "application/json" }`，
     body JSON **键顺序** `{code, code_verifier, code_challenge_method: "S256"}`（JSON.parse 后按此顺序，
     测试断言 `toMatchObject`）；
  4. 解析 body：`response.json()` 成功且为非数组对象 → 用它；**若解析失败且 `response.ok`** →
     `throw new Error("OpenRouter OAuth returned invalid JSON")`；非 ok 且解析失败 → body 保持 `{}`；
  5. catch：`if (signal.aborted) throw new Error("Login cancelled")`；`if (controller.signal.aborted) throw new Error("OpenRouter OAuth token exchange timed out")`；否则原样抛；
  6. finally：`clearTimeout`；移除 abort 监听；
  7. `!response.ok` → `` `OpenRouter OAuth key exchange failed (HTTP ${status})${detail ? `: ${detail}` : ""}` ``；
  8. `typeof body.key !== "string" || body.key.length === 0` → `'OpenRouter OAuth response carries no "key"'`；
  9. 返回 `{ type:"oauth", access: body.key, refresh: "", expires: Number.MAX_SAFE_INTEGER }`。
- `loginOpenRouter(interaction)`：
  1. `generatePKCE()`；
  2. `startOAuthCallbackServer({ providerName: "OpenRouter", host: getCallbackHost(), port: 0,
     path: `/oauth/callback/${crypto.randomUUID()}`, complete: (code) => exchangeAuthorizationCode(code, verifier, interaction.signal),
     signal: interaction.signal, timeoutMs: LOGIN_TIMEOUT_MS })`
     —— **随机路径**（OpenRouter 不发 state，随机路径防误请求）；**在服务器内完成交换**（页面可显示失败）；
  3. `authorizeUrl = new URL(AUTHORIZE_URL); authorizeUrl.search = new URLSearchParams({ callback_url: callback.redirectUri,
     code_challenge: challenge, code_challenge_method: "S256" }).toString();`
     （`search` 赋值 → `?` 后为完整 query，**键顺序如此**）；
  4. `notify({ type:"progress", message: `Listening for OpenRouter OAuth callback on ${callback.redirectUri}` })`；
  5. `notify({ type:"auth_url", url: authorizeUrl.toString(), instructions: "Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here." })`；
  6. `waitForCallbackOrManualInput(interaction, callback, { message: "Complete sign-in in your browser, or paste the authorization code / redirect URL here:", placeholder: callback.redirectUri })`；
     callback → 直接返回其 value（已交换的凭证）；manual → `parseAuthorizationInput`；无 code →
     `"Missing authorization code"`；`notify({type:"progress", message: "Exchanging authorization code for an API key..."})`；
     `return await exchangeAuthorizationCode(code, verifier, interaction.signal)`；
  7. `finally { callback.close(); }`。
- `openRouterOAuth`：`{ name: "OpenRouter OAuth", loginLabel: "Sign in with OpenRouter", login: loginOpenRouter,
  async refresh(credential, _signal) { return credential; }, async toAuth(c) { return { apiKey: c.access }; } }`
  —— `isSubscription` **未设置**（测试断言 `openRouterOAuth.isSubscription !== true`；refresh 返回**同一对象**）。

测试要点：一次性回调 + JSON 交换（`code_challenge` 与 verifier 的 SHA-256 匹配）；交换失败同时体现在页面（502）
与 login rejection（`OpenRouter OAuth key exchange failed (HTTP 403): invalid code`）；同一回调只允许一次交换
（第二次 409）；成功但无 key → `OpenRouter OAuth response carries no "key"`；粘贴完整回调 URL 或裸 code 均可
（裸 code 会 trim）；prompt 取消 → `Login cancelled` 且**不**发请求；空白输入 → `Missing authorization code`；
取消时关闭回调（后续 fetch 拒绝）；预取消 → 不创建服务器、不 emit 事件；`KIANA_OAUTH_CALLBACK_HOST=localhost` →
`callback_url` 主机为 `localhost`。

### 2.15 `src/auth/oauth/kimi-coding.ts`（296 行）

常量：

```ts
const CLIENT_ID = "17e5f671-d194-4dfb-9706-5516cb48c098";
const DEFAULT_OAUTH_HOST = "https://auth.kimi.com";
const DEVICE_CODE_TIMEOUT_SECONDS = 15 * 60;
const DEFAULT_POLL_INTERVAL_SECONDS = 5;
const REQUEST_TIMEOUT_MS = 30 * 1000;
const REFRESH_MAX_RETRIES = 3;
```

- `getOauthHost()`：`const override = getProviderEnvValue("KIMI_CODE_OAUTH_HOST") || getProviderEnvValue("KIMI_OAUTH_HOST");
  return (override || DEFAULT_OAUTH_HOST).replace(/\/+$/, "");`（**去掉全部尾随斜杠**；每次调用）。
- `requestSignal(signal)`：`AbortSignal.any([AbortSignal.timeout(30_000), signal])`（超时优先注册，两者任一触发）。
- `formUrlEncode(fields)`：`new URLSearchParams(fields).toString()`（`application/x-www-form-urlencoded` 编码，
  空格 → `+`，`:`,`/` → `%3A`,`%2F`）。
- `readJson(response)`：`response.json()` 成功且为对象 → 它；否则（抛错或非对象）→ `null`。
- `trustedHttpUrl(value)`：非字符串/空 → null；`new URL` 失败 → null；protocol 非 `https:`/`http:` → null；
  否则 `url.href`（规范化）。
- `startDeviceAuthorization(oauthHost, signal)`：POST `${oauthHost}/api/oauth/device_authorization`，
  headers `{ "Content-Type": "application/x-www-form-urlencoded", Accept: "application/json" }`，
  body `client_id=<CLIENT_ID>`；`!ok` → `` `Kimi Code device authorization failed with status ${status}${text ? `: ${text}` : ""}` ``；
  校验 `device_code`/`user_code`/`verification_uri`/`verification_uri_complete` 均为**非空**字符串且两个 URI 通过
  `trustedHttpUrl`，否则 `` `Invalid Kimi Code device authorization response: ${JSON.stringify(json)}` ``；
  `intervalSeconds = (number, finite, >0) ? interval : 5`；`expiresInSeconds = (number, finite, >0) ? expires_in : 900`。
- `parseTokenResponse(json, operation)`：`access_token`/`refresh_token` 必须非空字符串、`expires_in` 必须有限且 >0，
  否则 `` `Kimi Code token ${operation} response missing fields: ${JSON.stringify(json)}` ``；
  返回 `{ access, refresh, expires: Date.now() + expiresIn*1000 }`（**无余量**）。
- `pollForToken(oauthHost, device, signal)`：`pollOAuthDeviceCodeFlow<TokenResponse>({ intervalSeconds, expiresInSeconds,
  waitBeforeFirstPoll: true, signal, poll })`；poll：POST `${oauthHost}/api/oauth/token`（同 headers/编码），
  body `{client_id, device_code, grant_type: "urn:ietf:params:oauth:grant-type:device_code"}`：
  - `status >= 500` → `{status:"failed", message: `Kimi Code device token request failed with status ${status}${text ? `: ${text}` : ""}`}`；
  - `ok && typeof json.access_token === "string"` → 尝试 `parseTokenResponse`；失败 → `{status:"failed", message: error.message}`；
  - `error === "authorization_pending"` → pending；
  - `error === "slow_down"` → `{status:"slow_down", intervalSeconds: typeof interval === "number" && interval > 0 ? interval : undefined}`；
  - `error === "expired_token"` → `"Kimi Code device authorization expired. Please restart login."`；
  - `error === "access_denied"` → `"Kimi Code login was denied."`；
  - 其他 → `` `Kimi Code device token request failed (status ${status})${typeof error === "string" ? `: ${error}${description}` : ""}` ``，
    其中 `description = typeof error_description === "string" ? ": " + error_description : ""`。
- `isRetryableRefreshFailure(response)`：`status === 429 || status >= 500`。
- `refreshToken(oauthHost, refreshTokenValue, signal)`：`for (attempt = 0; attempt <= 3; attempt++)`：
  - `attempt > 0` → `await sleep(1000 * 2 ** (attempt - 1), signal)`（1s、2s、4s）；
  - `if (signal.aborted) throw new Error("Kimi Code token refresh aborted");`
  - POST `${oauthHost}/api/oauth/token`，body `{client_id, grant_type:"refresh_token", refresh_token}`；
    fetch 抛错 → 记录 `lastError` 并 `continue`（**重试**）；
  - `json = await readJson(response)`；`ok` → `parseTokenResponse(json, "refresh")`；
  - `status === 401 || 403 || json?.error === "invalid_grant"` → `` `Kimi Code token refresh unauthorized (status ${status})${description}` ``
    （**不重试**）；
  - `isRetryableRefreshFailure && attempt < 3` → `lastError = new Error(`Kimi Code token refresh failed with status ${status}`)`，continue；
  - 否则 → `` `Kimi Code token refresh failed with status ${status}${JSON.stringify(json)}` ``（`JSON.stringify(null)` = `"null"`，故总非空）。
  - 循环耗尽 → `throw lastError ?? new Error("Kimi Code token refresh failed")`。
- `loginKimiCoding(interaction)`：设备授权 → notify `{type:"device_code", userCode, verificationUri: device.verificationUriComplete,
  intervalSeconds, expiresInSeconds}`（**用 complete URI**）→ poll → `{ type:"oauth", access, refresh, expires }`。
- `kimiCodingOAuth`：`{ name: "Kimi Code (subscription)", isSubscription: true, loginLabel: "Sign in with Kimi Code",
  login: loginKimiCoding, refresh: async (credential, signal) => 用 getOauthHost() 重新解析并 refresh,
  async toAuth(c) { return { headers: { Authorization: `Bearer ${c.access}` } }; } }`
  —— `toAuth` 返回 **headers**（不是 apiKey）。

测试要点：登录时间线（+5000/+10000）、`expires` 不带余量、`expired_token`/`access_denied` 文案、
`KIMI_CODE_OAUTH_HOST`（尾斜杠被去掉）、refresh 返回 Bearer header、429 重试一次（t+1000）后成功、
`invalid_grant` 不重试并报 unauthorized。

### 2.16 `src/auth/oauth/meta.ts`（208 行）

常量：

```ts
const CLIENT_ID = "1031625952748946";
const AUTH_HOST = "https://auth.meta.com";
const DEVICE_AUTHORIZATION_URL = `${AUTH_HOST}/oidc/device/authorization/`;   // 注意尾斜杠
const DEVICE_TOKEN_URL = `${AUTH_HOST}/oidc/device/token/`;                  // 注意尾斜杠
const API_KEY_MINT_URL = "https://api.meta.ai/muse-code/key";
const API_KEY_LIFETIME_MS = 24 * 60 * 60 * 1000;
const REQUEST_TIMEOUT_MS = 30 * 1000;
```

文件头注释（语义必须保留）：Meta 把身份与 API 访问分开 —— 身份 token 不能用于推理，需用 Muse Code 的
key-mint 端点换成 Model API key（约一天有效）。**身份 token 存为 `refresh`，铸造的 key 存为 `access`**，
让标准 OAuth 调度器在 key 过期时重新铸造。身份 token 本身不可续期（`grant_type=refresh_token` 返回 404、
也不下发 refresh_token），因此 mint 返回 401/403 即会话失效，必须重新登录。

- `requestSignal(signal)`：`AbortSignal.any([AbortSignal.timeout(30_000), signal])`。
- `readJson(response)`：同 2.15。
- `errorDetail(json)`：按顺序 `error_description`、`detail`、`message`、`error`，第一个"字符串且 `trim()` 非空"的值 →
  返回 `` `: ${value.trim()}` ``；否则 `""`。
- `trustedHttpUrl(value)`：同 2.15（http/https）。
- `positiveNumber(value)`：`number && Number.isFinite && > 0 ? value : undefined`。
- `startDeviceAuthorization(signal)`：POST `DEVICE_AUTHORIZATION_URL`，headers 同 Kimi，
  body `new URLSearchParams({ client_id: CLIENT_ID }).toString()`；
  `json = await readJson(response)`（**先读 body 再判 ok**）；`!ok` → `` `Meta device authorization failed with status ${status}${errorDetail(json)}` ``；
  `verificationUri = trustedHttpUrl(json?.verification_uri_complete) ?? trustedHttpUrl(json?.verification_uri)`
  （**优先 complete**）；`device_code`/`user_code` 必须非空字符串且 `verificationUri` 非空，否则
  `` `Invalid Meta device authorization response: ${JSON.stringify(json)}` ``；
  返回 `{ deviceCode, userCode, verificationUri, intervalSeconds: positiveNumber(json?.interval), expiresInSeconds: positiveNumber(json?.expires_in) }`
  （都可为 `undefined` → 轮询用默认 5s / 无 deadline）。
- `pollForIdentityToken(device, signal)`：`pollOAuthDeviceCodeFlow<string>({ ..., waitBeforeFirstPoll: true, ... })`；
  poll：POST `DEVICE_TOKEN_URL`，body `{grant_type: "urn:ietf:params:oauth:grant-type:device_code", device_code, client_id}`；
  `ok && typeof json.access_token === "string" && json.access_token` → complete；
  switch `json?.error`：`authorization_pending` → pending；`slow_down` → `{status:"slow_down", intervalSeconds: positiveNumber(json?.interval)}`；
  `access_denied` → `"Meta login was denied."`；`expired_token` → `"Meta device authorization expired. Please restart login."`；
  default → `` `Meta device token request failed with status ${status}${errorDetail(json)}` ``。
- `mintApiKey(identityToken, signal)`：POST `API_KEY_MINT_URL`，headers `{ Accept: "application/json",
  Authorization: `Bearer ${identityToken}`, "Content-Type": "application/json", "x-api-version": "1.0.0" }`，
  body 字面量 `"{}"`，`signal: requestSignal(signal)`（即 `AbortSignal.any([timeout(30s), signal])` —— **mint 也有 30s 超时**，
  原文漏写；`refresh()` 直接复用 `mintApiKey`，因此刷新同样受 30s 约束）；
  - `status === 401 || 403` → `` `Meta session expired (status ${status}). Run \`/login meta\` to sign in again.${errorDetail(json)}` ``；
  - `!ok` → `` `Meta API key mint failed with status ${status}${errorDetail(json)}` ``；
  - `api_key` 必须非空字符串；否则取 `trustedHttpUrl(json?.action_url)`：
    `` `Meta did not issue an API key.${actionUrl ? ` Complete setup at ${actionUrl}` : ""}` ``；
  - 返回 `{ type:"oauth", refresh: identityToken, access: apiKey, expires: Date.now() + API_KEY_LIFETIME_MS }`。
- `loginMeta(interaction)`：try { 设备授权 → notify `{type:"device_code", userCode, verificationUri, intervalSeconds,
  expiresInSeconds}`（**不传** confirm 标志）→ poll 身份 token → `notify({type:"progress", message: "Enabling Meta Model API access..."})`
  → `mintApiKey` } catch { `if (interaction.signal.aborted) throw new Error("Login cancelled"); throw e;` }
  —— 注释：in-flight fetch 在 abort 时抛 DOMException，登录 UI 依赖这条消息。
- `metaOAuth`：`{ name: "Meta (Muse subscription)", isSubscription: true, loginLabel: "Sign in with Meta",
  login: loginMeta, refresh: (credential, signal) => mintApiKey(credential.refresh, signal),
  async toAuth(c) { return { apiKey: c.access }; } }`。

测试要点：设备流程 notify 用 `verification_uri_complete`；poll 时间线 +5000/+10000；`expires = now + 10s + 24h`
（fake timers 下精确）；refresh 用 `credential.refresh` 重新铸造；无 key 且 `action_url` →
`Meta did not issue an API key. Complete setup at https://dev.meta.ai/billing`；toAuth 用 access。

### 2.17 `src/auth/oauth/xai.ts`（239 行）

常量：

```ts
const XAI_CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828";
const XAI_SCOPE = "openid profile email offline_access grok-cli:access api:access";
const XAI_DEVICE_CODE_URL = "https://auth.x.ai/oauth2/device/code";
const XAI_TOKEN_URL = "https://auth.x.ai/oauth2/token";
const REFRESH_SKEW_MS = 5 * 60 * 1000;
const DEFAULT_TOKEN_LIFETIME_SECONDS = 3600;
```

- `requiredString(body, field)`：非字符串或空 → `` `Invalid xAI OAuth response field: ${field}` ``。
- `positiveNumber(body, field)`：`number && finite && > 0` 否则同文案。
- `validateVerificationUri(raw)`：`new URL` 失败或 `protocol !== "https:"` → `"Untrusted verification URI in xAI OAuth response"`；
  否则 `url.href`（**只允许 https**，与 Copilot/Kimi/Meta 不同）。
- `postForm(url, fields, signal)`：POST，headers `{ Accept: "application/json", "Content-Type": "application/x-www-form-urlencoded" }`，
  body `new URLSearchParams(fields)`；fetch 抛错时 `if (signal.aborted) throw new Error("Login cancelled"); throw e;`；
  解析 body：非数组对象 → 它；其他 → `{}`；解析失败时 `if (signal.aborted) throw new Error("Login cancelled");`
  否则 `` `xAI OAuth returned invalid JSON (HTTP ${status})` ``；返回 `{ ok, status, body }`。
- `requestFailure(action, response)`：`detail = [error, error_description].filter(Boolean).join(": ")`；
  `` `xAI OAuth ${action} failed (HTTP ${status})${detail ? `: ${detail}` : ""}` ``。
- `parseDeviceCode(body)`：
  - `intervalSeconds = (number && finite && > 0) ? interval : undefined`（**0 视为缺失** → 轮询用默认 5s）；
  - `verificationUriComplete = (非空字符串) ? validateVerificationUri(...) : undefined`；
  - 返回 `{ deviceCode: requiredString(body,"device_code"), userCode: requiredString(body,"user_code"),
    verificationUri: validateVerificationUri(requiredString(body,"verification_uri")), verificationUriComplete,
    intervalSeconds, expiresInSeconds: positiveNumber(body,"expires_in") }`
    （`expires_in` 缺失 → **抛错**，与 Kimi/Meta 不同）。
- `credentialsFromTokenResponse(body, previousRefreshToken?)`：
  - `access = requiredString(body,"access_token")`；
  - `refresh = (body.refresh_token === undefined && previousRefreshToken) ? previousRefreshToken : requiredString(body,"refresh_token")`
    （**未轮换时保留旧 refresh**）；
  - `expiresInSeconds = body.expires_in === undefined ? 3600 : positiveNumber(body,"expires_in")`
    （**刷新时缺失按 1 小时**）；
  - 返回 `{ type:"oauth", access, refresh, expires: Date.now() + expiresInSeconds*1000 - REFRESH_SKEW_MS }`（减 5 分钟）。
- `requestDeviceCode(signal)`：`postForm(XAI_DEVICE_CODE_URL, { client_id, scope, referrer: "kiana" }, signal)`；
  `!ok` → `requestFailure("device authorization", response)`；`parseDeviceCode(body)`。
- `pollForTokens(device, signal)`：`pollOAuthDeviceCodeFlow<OAuthCredential>({ intervalSeconds, expiresInSeconds,
  waitBeforeFirstPoll: true, signal, poll })`；poll：`postForm(XAI_TOKEN_URL, { grant_type: "urn:ietf:params:oauth:grant-type:device_code",
  client_id, device_code }, signal)`；`ok` → complete（`credentialsFromTokenResponse(body)`，**无 previousRefreshToken**）；
  `authorization_pending` → pending；`slow_down` → `{status:"slow_down", intervalSeconds: typeof interval === "number" ? interval : undefined}`；
  `access_denied` 或 `authorization_denied` → `"xAI device authorization was denied"`；
  `expired_token` → `"xAI device code expired"`；其他 → `{status:"failed", message: requestFailure("device token polling", response).message}`。
- `loginXai(interaction)`：设备码 → notify `{type:"device_code", userCode, verificationUri: device.verificationUriComplete ?? device.verificationUri,
  intervalSeconds, expiresInSeconds}` → poll。
- `refreshXaiToken(refreshToken, signal)`：`postForm(XAI_TOKEN_URL, { grant_type:"refresh_token", client_id, refresh_token }, signal)`；
  `!ok` → `requestFailure("token refresh", response)`；`credentialsFromTokenResponse(body, refreshToken)`。
- `xaiOAuth`：`{ name: "xAI (Grok/X subscription)", isSubscription: true, loginLabel: "Sign in with SuperGrok or X Premium",
  login: loginXai, refresh: (c, s) => refreshXaiToken(c.refresh, s), async toAuth(c) { return { apiKey: c.access }; } }`。

测试要点（`xai-oauth.test.ts`）：设备表单字段（client_id/scope/referrer）、轮询时间线（+5000，slow_down
带 interval=10 → 下一次 +10000，再下一次 +20000）、`interval: 0` 回退默认 5s、优先 `verification_uri_complete`、
非 https 的 complete/verification URI（含 `file:///etc/passwd`、`not a url`）→ `Untrusted verification URI`、
`access_denied`/`authorization_denied` → `xAI device authorization was denied`、首个轮询等待期间取消 →
`Login cancelled` 且只发过 1 个请求、refresh 保留未轮换 refresh、`expires_in` 缺失按 3600−300、缺字段 →
`Invalid xAI OAuth response field: access_token`、refresh 失败 → `xAI OAuth token refresh failed (HTTP 400): invalid_grant: refresh token revoked`。

### 2.18 `src/auth/oauth/radius.ts`（321 行）

常量（逐字）：

```ts
const CALLBACK_HOST = "127.0.0.1";          // 固定，不读 KIANA_OAUTH_CALLBACK_HOST
const CALLBACK_PORT = 1456;
const CALLBACK_PATH = "/oauth/callback";
const REDIRECT_URI = `http://${CALLBACK_HOST}:${CALLBACK_PORT}${CALLBACK_PATH}`;
const TOKEN_EXPIRY_SKEW_MS = 60_000;
const LOGIN_METHOD_BROWSER = "browser";
const LOGIN_METHOD_DEVICE_CODE = "device-code";     // 注意是 "device-code"（连字符）
const OAUTH_CLIENT_ID = "kiana-gateway";
const OAUTH_SCOPE = "gateway offline_access";
const OAUTH_DEVICE_CODE_GRANT_TYPE = "urn:ietf:params:oauth:grant-type:device_code";
```

- `loadRadiusOAuthDiscovery(gateway, signal)`：`fetch(new URL("/v1/oauth", gateway), { headers: { accept: "application/json" }, signal })`；
  `!ok` → `` `Could not load Radius OAuth config from ${gateway}: ${status} ${await response.text()}` ``；
  `typeof discovery.authorizationEndpoint !== "string"` → `` `Invalid Radius OAuth config from ${gateway}` ``；
  返回 `{ authorizationEndpoint }`（**只**取这个字段；测试用 `{issuer}` 断言报错）。
- `class OAuthResponseError extends Error`：字段 `status: number`、`oauthError?: string`；
  构造：`detail = oauthError ? (description ? `${oauthError}: ${description}` : oauthError) : (description || String(status))`；
  `super(`${message}: ${detail}`)`；`this.status = status; this.oauthError = oauthError;`（`name` 保持 `"Error"`，
  因为未赋值 —— Rust 侧错误类型需能被 `instanceof` 判定，等价于 `enum` 分支）。
- `readOAuthResponseError(response, message)`：`text = await response.text().catch(() => "")`；若 text 非空：
  尝试 `JSON.parse` 取 `error`/`error_description`（都需 string）；解析失败 → `description = text`；
  返回 `new OAuthResponseError(response.status, oauthError, description, message)`。
- `requestOAuthToken(gateway, body: URLSearchParams, signal)`：POST `new URL("/v1/oauth/token", gateway)`，
  headers `{ accept: "application/json", "content-type": "application/x-www-form-urlencoded" }`；
  fetch 抛错时 `if (signal.aborted) throw new Error("Login cancelled"); throw e;`；
  `!ok` → `throw await readOAuthResponseError(response, "Radius OAuth token request failed")`；
  返回 `{ type:"oauth", access: data.access_token, refresh: data.refresh_token,
  expires: Date.now() + data.expires_in*1000 - 60_000, scope: data.scope }`（`scope` 可能是 undefined）。
- `loginWithBrowser(gateway, authorizationEndpoint, interaction)`：
  1. `generatePKCE()`；`state = crypto.randomUUID()`；
  2. `authorizeUrl = new URL(authorizationEndpoint); authorizeUrl.search = new URLSearchParams({ response_type:"code",
     client_id: OAUTH_CLIENT_ID, redirect_uri: REDIRECT_URI, scope: OAUTH_SCOPE, code_challenge: challenge,
     code_challenge_method: "S256", handoff: "url", state }).toString();`
  3. `startOAuthCallbackServer({ providerName: "Radius", host: CALLBACK_HOST, port: 1456, path: CALLBACK_PATH, state,
     complete: (code) => requestOAuthToken(gateway, new URLSearchParams({ grant_type:"authorization_code", client_id,
     redirect_uri: REDIRECT_URI, code, code_verifier: verifier }), interaction.signal), signal: interaction.signal })`
     —— **端口固定 1456，不回落**（被占则抛 EADDRINUSE）；交换在服务器内完成（页面可显示失败）。
  4. `notify({type:"progress", message: `Listening for OAuth callback on ${REDIRECT_URI}`})`；
     `notify({type:"auth_url", url: authorizeUrl.toString(), instructions: "Continue in your browser."})`；
  5. `const credential = await callback.wait(); if (!credential) throw new Error("OAuth callback did not complete."); return credential;`
  6. `finally { callback.close(); }`。
- `requestDeviceAuthorization(gateway, signal)`：POST `new URL("/v1/oauth/device", gateway)`，
  headers 同 token，body `new URLSearchParams({ client_id: OAUTH_CLIENT_ID, scope: OAUTH_SCOPE })`；
  fetch 抛错 → `Login cancelled`（若 aborted）；`!ok` → `readOAuthResponseError(response, "Radius OAuth device authorization failed")`；
  `!data.device_code || !data.user_code || !data.verification_uri || !data.expires_in` →
  `"Radius OAuth device authorization response is missing required fields"`；返回原样字段（含可选 interval）。
- `loginWithDeviceCode(gateway, interaction)`：设备授权 → notify `{type:"device_code", userCode, verificationUri,
  intervalSeconds: device.interval, expiresInSeconds: device.expires_in}`（**无 waitBeforeFirstPoll** → 立即轮询）→
  `pollOAuthDeviceCodeFlow<OAuthCredential>({ intervalSeconds, expiresInSeconds, signal, poll })`；poll 调
  `requestOAuthToken(gateway, {grant_type: OAUTH_DEVICE_CODE_GRANT_TYPE, client_id, device_code}, signal)` 并映射：
  `authorization_pending` → pending；`slow_down` → slow_down（**无 intervalSeconds**）；`expired_token` →
  `"Device authorization expired."`；`access_denied` → `"Device authorization was denied."`；**default → 原样重抛**
  （非 `OAuthResponseError` 的错误也原样抛）。
- `export interface RadiusOAuthOptions { name: string; gateway: string }`
- `export function createRadiusOAuth(options): OAuthAuth`：`gateway = normalizeRadiusGatewayUrl(options.gateway)`
  （见 ai-core：无 scheme 补 `https://`，去尾斜杠；正则 `/^https?:\/\//iu`）。
  - `name: options.name`（**无** isSubscription / loginLabel）；
  - `login(interaction)`：`prompt({ type:"select", message: `Sign in to ${options.name}:`, options: [
    { id: "browser", label: "Sign in with browser (recommended)" },
    { id: "device-code", label: "Sign in with device code (when signing in from another device)" } ] })`；
    `device-code` → `loginWithDeviceCode`；`browser` → 先 `loadRadiusOAuthDiscovery(gateway, signal)` 再 `loginWithBrowser`；
    其他 → `` `Unknown ${options.name} sign-in method: ${loginMethod}` ``；
  - `refresh(credential, signal)`：`requestOAuthToken(gateway, {grant_type:"refresh_token", client_id, refresh_token}, signal)`
    （**不做 discovery**）；
  - `toAuth(c) { return { apiKey: c.access }; }`。

测试要点：设备登录 URL 序列恰为 `[/v1/oauth/device, /v1/oauth/token]`、表单字段、返回凭证含 `scope`、
`expires = now + 3600s − 60s`；refresh 只打 `/v1/oauth/token`；discovery 缺 `authorizationEndpoint` →
`Invalid Radius OAuth config from <gateway>`；browser 流程交换失败 → 502 页面含 `code expired`、login 错误含
`invalid_grant: code expired`；成功 → 200 且页面含 `Signed in to Radius.`。

### 2.19 `src/oauth.ts`（10 行）与 `src/bun-oauth.ts`（25 行）

`src/oauth.ts`：**仅类型**重导出（`@earendil-works/kiana-ai/oauth`）：

```ts
export type {
	OAuthAuthInfo, OAuthCredentials, OAuthDeviceCodeInfo, OAuthLoginCallbacks,
	OAuthPrompt, OAuthSelectOption, OAuthSelectPrompt,
} from "./compat/extension-oauth-types.ts";
```

（`OAuthCredentials` 最终来自 `auth/types.ts`；其余来自 `compat/extension-oauth-types.ts`，见 ai-core §2.1.15。）
**不变量**：该模块运行时**必须为空**（无任何值导出），测试断言其没有 `loginAnthropic`/`anthropicOAuth` 属性。

`src/bun-oauth.ts`：

```ts
import { anthropicOAuth } from "./auth/oauth/anthropic.ts";
import { githubCopilotOAuth } from "./auth/oauth/github-copilot.ts";
import { kimiCodingOAuth } from "./auth/oauth/kimi-coding.ts";
import { registerBundledOAuthFlowLoaders } from "./auth/oauth/load.ts";
import { metaOAuth } from "./auth/oauth/meta.ts";
import { openaiChatGPTOAuth } from "./auth/oauth/openai-chatgpt.ts";
import { openaiCodexOAuth } from "./auth/oauth/openai-codex.ts";
import { openRouterOAuth } from "./auth/oauth/openrouter.ts";
import { createRadiusOAuth } from "./auth/oauth/radius.ts";
import { xaiOAuth } from "./auth/oauth/xai.ts";

export function registerBunOAuthFlows(): void {
	registerBundledOAuthFlowLoaders({
		anthropic: () => anthropicOAuth,
		openaiCodex: () => openaiCodexOAuth,
		openaiChatGPT: () => openaiChatGPTOAuth,
		githubCopilot: () => githubCopilotOAuth,
		openrouter: () => openRouterOAuth,
		kimiCoding: () => kimiCodingOAuth,
		meta: () => metaOAuth,
		xai: () => xaiOAuth,
		radius: createRadiusOAuth,
	});
}
```

注意：该模块**顶层 import** 了全部 9 个 OAuth 实现（与 `load.ts` 的懒加载相反），因为它只用于把实现静态内嵌进
Bun 单文件二进制。调用一次即注册；重复调用等价。

### 2.20 本单元直接依赖、但不属于本单元的文件（精确契约）

| 文件 | 本单元用到的符号 | 精确语义 |
|---|---|---|
| `src/utils/oauth-page.ts` | `oauthSuccessHtml(message)`、`oauthErrorHtml(message, details?)` | 见 4.6（整页 HTML 逐字） |
| `src/utils/abort.ts` | `abortReason(signal)`、`operationSignal(signal?)`、`raceWithAbortSignal(op, signal)` | 见 2.5 与 4.7 |
| `src/utils/models-error.ts` | `ModelsError`、`ModelsErrorCode` | 见 2.5 末尾 |
| `src/utils/provider-env.ts` | `getProviderEnvValue(name, env?)` | `env?.[name] || process.env[name] || getBunSandboxEnvValue(name) || undefined`；Bun 沙箱回退读 `/proc/self/environ`（仅当 `process.versions.bun` 且 `process.env` 为空），解析 `\0` 分隔的 `NAME=VALUE`（`idx > 0` 才收录），结果缓存在模块级 `Map` |
| `src/utils/sleep.ts` | `sleep(ms, signal)` | `signal.throwIfAborted(); setTimeout(resolve, ms)`；abort → `reject(signal.reason)`（**不是**自定义 Error） |
| `src/utils/diagnostics.ts` | `formatThrownValue(value)` | Error → `message \|\| name`；string → 原样；其他 → `String(value)` |
| `src/providers/github-copilot.models.ts` | `GITHUB_COPILOT_MODELS` | 内置 Copilot 模型目录对象（键为模型 id）；本单元只用 `Object.hasOwn` |
| `src/providers/radius-config.ts` | `normalizeRadiusGatewayUrl(value)` | `/^https?:\/\//iu.test(value) ? value : \`https://${value}\``，然后 `.replace(/\/+$/u, "")` |

Rust 侧这些符号由 ai-core 单元实现；ai-auth 只依赖其函数签名与上述语义。**注意 `oauth-page.ts` 的 HTML 必须由
ai-core 提供且逐字一致**（本单元的回调服务器直接把它的输出写到响应里）。

---

## 3. 用户可见表面（逐字文本）

本单元不直接渲染终端；它通过 `AuthInteraction` 把文本交给宿主（coding-agent 的 `LoginDialogComponent`，
属 tui/coding-agent 单元）。以下文本**由本单元产生**，必须逐字一致（含标点、大小写、省略号、冒号后的空格）。

### 3.1 `AuthPrompt`（登录期间显示的提示）

| 流程 | prompt | 逐字字段 |
|---|---|---|
| `envApiKeyAuth(name, …).login` | secret | `message: "Enter " + name`（如 `Enter Anthropic API key`、`Enter OpenAI API key`、`Enter GitHub Copilot token`、`Enter Radius API key`）；无 placeholder |
| Anthropic | select | `message: "Select Anthropic login method:"`；options `[{id:"browser",label:"Browser login (default)"},{id:"copy_code",label:"Copy code login (headless)"}]` |
| Anthropic browser | manual_code | `message: "Complete login in your browser, or paste the authorization code / redirect URL here:"`；`placeholder: <redirectUri>` |
| Anthropic copy_code | manual_code | `message: "Paste the code Anthropic shows after you sign in:"`；`placeholder: "code#state"` |
| OpenAI Codex | select | `message: "Select OpenAI Codex login method:"`；options `[{id:"browser",label:"Browser login (default)"},{id:"device_code",label:"Device code login (headless)"}]` |
| OpenAI Codex browser | manual_code | `message: "Complete login in your browser, or paste the authorization code / redirect URL here:"`；`placeholder: "http://localhost:1455/auth/callback"` |
| OpenAI ChatGPT | manual_code | `message: "Complete login in your browser, or paste the final redirect URL here:"`；`placeholder: "http://127.0.0.1:1455/auth/callback"` |
| GitHub Copilot | text | `message: "GitHub Enterprise URL/domain (blank for github.com)"`；`placeholder: "company.ghe.com"` |
| OpenRouter | manual_code | `message: "Complete sign-in in your browser, or paste the authorization code / redirect URL here:"`；`placeholder: <callback.redirectUri>` |
| Radius | select | `message: \`Sign in to ${options.name}:\``；options `[{id:"browser",label:"Sign in with browser (recommended)"},{id:"device-code",label:"Sign in with device code (when signing in from another device)"}]` |
| Kimi / Meta / xAI | （无 prompt，纯设备码） | — |

### 3.2 `AuthEvent.auth_url`

| 流程 | `instructions`（逐字） |
|---|---|
| Anthropic browser | `Complete login in your browser. If the browser is on another machine, paste the final redirect URL here.` |
| Anthropic copy_code | `Complete login in your browser, then copy the code Anthropic shows and paste it here.` |
| OpenAI Codex browser | `A browser window should open. Complete login to finish.` |
| OpenAI ChatGPT | `Complete sign-in in your browser. If the callback does not complete, paste the final redirect URL here.` |
| OpenRouter | `Complete sign-in in your browser. If the browser is on another machine, paste the final redirect URL here.` |
| Radius browser | `Continue in your browser.` |

`url` 为完整授权 URL，参数顺序见 §4.3（逐字 query 顺序是契约）。

### 3.3 `AuthEvent.progress`

逐字：`Exchanging authorization code for tokens...`（Anthropic 两种、OpenAI ChatGPT）、
`Exchanging authorization code for an API key...`（OpenRouter）、
`Listening for OpenRouter OAuth callback on ${callback.redirectUri}`（OpenRouter）、
`Listening for OAuth callback on ${REDIRECT_URI}`（Radius）、
`Enabling models...`（GitHub Copilot）、`Enabling Meta Model API access...`（Meta）。

### 3.4 `AuthEvent.device_code`

| 流程 | userCode | verificationUri | intervalSeconds | expiresInSeconds | 额外 |
|---|---|---|---|---|---|
| GitHub Copilot | 服务端 `user_code` | 规范化后的 `verification_uri` | 服务端 `interval`（可能 undefined） | 服务端 `expires_in` | 无 confirm 标志 |
| OpenAI Codex device | `user_code` | `https://auth.openai.com/codex/device` | 服务端（字符串会 `Number(trim())`） | `900` | — |
| Kimi | `user_code` | **`verification_uri_complete`** | 服务端或 5 | 服务端或 900 | — |
| Meta | `user_code` | `verification_uri_complete` ?? `verification_uri` | 可选 | 可选 | — |
| xAI | `user_code` | `verification_uri_complete` ?? `verification_uri` | 可选（>0 才算） | 必填 | — |
| Radius device | `user_code` | 服务端 `verification_uri` | 可选 | 必填 | — |

`openBrowserOnConfirm` / `copyCodeOnConfirm` 本单元**从不设置**（宿主默认按 true 处理）。

### 3.5 `AuthEvent.info`

本单元**未使用**（只有 `auth_url`/`device_code`/`progress`）。宿主仍需支持（其他 provider 会用）。

### 3.6 错误文案（用户可见的错误消息）

本单元直接抛出的 `Error`（`message` 逐字）：

| 出处 | message |
|---|---|
| callback-server | `Login cancelled` |
| callback-server | `OAuth callback server did not bind to TCP` |
| callback-server | `OAuth callback server closed` |
| callback-server | `${providerName} sign-in timed out` |
| callback-server | `${providerName} authorization failed: ${description}` |
| callback-server | `${providerName} sign-in failed.`（页面 message；wait 抛原始 failure message） |
| device-code | `Login cancelled`、`Device flow timed out`、`Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again.` |
| anthropic | `OAuth state mismatch`、`Missing authorization code`、`Unknown Anthropic login method: ${method}`、`Token exchange request failed. url=…; redirect_uri=…; response_type=authorization_code; details=…`、`Token exchange returned invalid JSON. url=…; body=…; details=…`、`Anthropic token refresh request failed. url=…; details=…`、`Anthropic token refresh returned invalid JSON. url=…; body=…; details=…`、`HTTP request failed. status=…; url=…; body=…` |
| openai-codex | `OpenAI Codex OAuth is only available in Node.js environments`、`Login cancelled`、`State mismatch`、`Missing authorization code`、`OpenAI Codex token exchange failed (${status}): …`、`OpenAI Codex token refresh failed (${status}): …`、`OpenAI Codex token exchange response missing fields: …`、`OpenAI Codex token refresh response missing fields: …`、`OpenAI Codex token refresh error: …`、`OpenAI Codex device code login is not enabled for this server. Use browser login or verify the server URL.`、`OpenAI Codex device code request failed with status ${status}[: body]`、`Invalid OpenAI Codex device code response: …`、`Invalid OpenAI Codex device auth token response: …`、`OpenAI Codex device auth failed with status ${status}[: body]`、`Failed to extract accountId from token`、`Unknown OpenAI Codex login method: ${method}` |
| openai-chatgpt | `Missing authorization code`、`Missing OAuth state`、`OAuth state mismatch`、`OpenAI OAuth registration callback did not contain an issued client ID`、`Paste the full callback URL from the browser`、`The pasted callback URL must start with http://127.0.0.1:1455/auth/callback`、`ChatGPT authorization failed: ${error}`、`OpenAI OAuth token request failed (${status}): …`、`OpenAI OAuth token response must be an object`、`OpenAI OAuth token response has invalid access_token|refresh_token|scope`、`OpenAI OAuth token response has invalid expires_in`、`OpenAI OAuth grant did not include chatgpt.tokens.use.direct`、`OpenAI OAuth token response did not contain an ID token`、`Stored OpenAI OAuth credential does not contain an issued client ID; reconnect ChatGPT`、`Sign in with ChatGPT requires a device ID (UUID) for this installation`、`Port 1455 is in use, probably by an unfinished login in another kiana session or by the Codex CLI. Cancel that login and try again.`、`Login cancelled` |
| github-copilot | `Invalid GitHub Enterprise URL/domain`、`Login cancelled`、`Invalid device code response`、`Invalid device code response fields`、`Untrusted verification_uri in device code response`、`Device flow failed: ${error}[: ${description}]`、`Invalid device token response`、`Invalid Copilot token response`、`Invalid Copilot token response fields`、`Invalid Copilot models response`、`${status} ${statusText}: ${body}`（目录/policy 失败） |
| openrouter | `Login cancelled`、`OpenRouter OAuth returned invalid JSON`、`OpenRouter OAuth token exchange timed out`、`OpenRouter OAuth key exchange failed (HTTP ${status})[: ${detail}]`、`OpenRouter OAuth response carries no "key"`、`Missing authorization code` |
| kimi-coding | `Kimi Code device authorization failed with status ${status}[: ${text}]`、`Invalid Kimi Code device authorization response: …`、`Kimi Code token poll response missing fields: …`、`Kimi Code device token request failed with status ${status>500}[: ${text}]`、`Kimi Code device authorization expired. Please restart login.`、`Kimi Code login was denied.`、`Kimi Code device token request failed (status ${status})[: ${error}${description}]`、`Kimi Code token refresh aborted`、`Kimi Code token refresh unauthorized (status ${status})[: ${description}]`、`Kimi Code token refresh failed with status ${status}`、`Kimi Code token refresh failed with status ${status}: ${JSON.stringify(json)}`、`Kimi Code token refresh failed` |
| meta | `Meta device authorization failed with status ${status}[: detail]`、`Invalid Meta device authorization response: …`、`Meta login was denied.`、`Meta device authorization expired. Please restart login.`、`Meta device token request failed with status ${status}[: detail]`、``Meta session expired (status ${status}). Run `/login meta` to sign in again.[: detail]``、`Meta API key mint failed with status ${status}[: detail]`、`Meta did not issue an API key.[ Complete setup at ${actionUrl}]`、`Login cancelled` |
| xAI | `Invalid xAI OAuth response field: ${field}`、`Untrusted verification URI in xAI OAuth response`、`xAI OAuth returned invalid JSON (HTTP ${status})`、`xAI OAuth device authorization failed (HTTP ${status})[: ${detail}]`、`xAI OAuth device token polling failed (HTTP ${status})[: ${detail}]`、`xAI OAuth token refresh failed (HTTP ${status})[: ${detail}]`、`xAI device authorization was denied`、`xAI device code expired`、`Login cancelled` |
| radius | `Could not load Radius OAuth config from ${gateway}: ${status} ${body}`、`Invalid Radius OAuth config from ${gateway}`、`Radius OAuth token request failed: ${detail}`、`Radius OAuth device authorization failed: ${detail}`、`Radius OAuth device authorization response is missing required fields`、`Device authorization expired.`、`Device authorization was denied.`、`OAuth callback did not complete.`、`Login cancelled`、`Unknown ${name} sign-in method: ${method}` |
| envApiKeyAuth | `Enter ${name}`（prompt，非错误） |
| resolve | `OAuth refresh failed for ${providerId}`、`Credential store modify failed for ${providerId}`、`OAuth refresh returned a token that expires too soon for ${providerId}`、`OAuth auth derivation failed for ${providerId}`、`API key auth failed for provider ${providerId}`、`Credential store read failed for ${providerId}` |

### 3.7 浏览器页面（用户可见的 HTML）

见 §4.6 的逐字模板。页面标题恒为 `Authentication successful` 或 `Authentication failed`；
成功 message 逐字：

- 通用回调服务器：`Signed in to ${providerName}. You may now close this page.`
- OpenAI ChatGPT 专用服务器：`ChatGPT authentication completed. You can close this window.`

失败 message 逐字：`Callback route not found.`、`State mismatch.`、`This sign-in has already been handled.`、
`Missing authorization code.`、`${providerName} authorization failed.`（details = `error_description ?? error`）、
`${providerName} sign-in failed.`（details = 原始错误 message）、`ChatGPT was not connected.`（details = `Error: ${error}`）、
`Internal error while processing the callback.`、以及 ChatGPT 回调解析错误的 message 原文。

### 3.8 与 CLI 的边界

`kiana auth check|print-api-key|print-bearer-token` 的解析/输出属 coding-agent 单元；本单元提供其依赖的
`resolveProviderAuth`（含 `minOAuthValidityMs`）与 `ModelsError`。`print-bearer-token` 缺省要求 30 分钟剩余
有效期（`DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS = 30 * 60_000`），通过 `getAuth(provider, { minOAuthValidityMs })`
触发本单元 §2.5 的"刷新后强校验"分支。

---

## 4. 外部格式（磁盘 / 环境变量 / HTTP / 网页）

### 4.1 `auth.json`（磁盘格式，由宿主实现、本单元定义形状）

本单元定义 `Credential` 形状；持久化由 coding-agent 的 `AuthStorage`（`packages/coding-agent/src/core/auth-storage.ts`）
提供。Rust 侧若把两者合并实现，必须保持以下逐字行为：

- 路径：`join(getAgentDir(), "auth.json")`，其中 `getAgentDir()` =
  `process.env[`${APP_NAME.toUpperCase()}_CODING_AGENT_DIR`]`（非空时，经 `normalizePath` 展开 `~`）否则
  `join(os.homedir(), CONFIG_DIR_NAME, "agent")`；`CONFIG_DIR_NAME` 默认 `".kiana"`、`APP_NAME` 默认 `"kiana"`
  （均来自 `package.json` 的 `kianaConfig`）。默认即 `~/.kiana/agent/auth.json`；环境变量名 `KIANA_CODING_AGENT_DIR`。
- 目录创建：`mkdirSync(dir, { recursive: true, mode: 0o700 })`。
- 文件若不存在则写入 `"{}"`（创建时 `mode: 0o600`；已存在时不改 mode/ACL）。
- 写回：`JSON.stringify(data, null, 2)`（2 空格缩进，**无末尾换行**），`encoding: "utf-8"`, `mode: 0o600`。
  顶层键顺序 = 原文件顺序（`{ ...currentData, [provider]: next }` 保留未修改 provider 的插入顺序，
  被修改的 provider 保持其原位置；新 provider 追加到末尾）。
- 删除：`delete currentData[provider]` 后整体重写。
- 读取时剥离 UTF-8 BOM（`stripBom`）；`ENOENT` → `{}`。
- 文件锁：`proper-lockfile`，`realpath: false`；异步锁 `stale: 30_000`、`retries: 0`，重试指数退避
  （`baseDelayMs = min(10 * 2**retry, 1000)`，随机抖动 `round(baseDelayMs * (1 + Math.random()))`，
  上限 `remainingMs`，总 deadline 30s；Windows 上 `EPERM` 额外最多重试 5 次）；同步锁最多 10 次、每次 20ms 忙等。
  锁被破坏（onCompromised）→ 抛 `"Auth storage lock was compromised"` 或原始错误。
- 校验（`ReadOnlyAuthStorage.load()`，用于 `--no-refresh`）：解析失败 →
  `Failed to read auth.json: <message>`；顶层非对象 → `Invalid auth.json: expected an object`；
  条目非对象 → `Invalid auth.json credential for provider "<id>"`；`api_key` 要求 `key` 为 string|undefined、
  `env` 为 `Record<string,string>`|undefined；`oauth` 要求 `access`/`refresh` 为 string、`expires` 为有限 number；
  否则同一 `Invalid auth.json credential for provider "<id>"`。
- 只读存储的 `modify`/`delete` → `Read-only credential storage cannot modify auth.json`。
- `read()` 两个实现的语义**不同**（原文笼统写成"OAuth 凭证原样返回 `structuredClone`"，只对其中一个成立）：
  - `AuthStorage`（可变、文件后端，实际被 runtime 使用）：`read(provider)` = `await readLatestData(options)`
    → 取 `[provider]` → `options?.signal?.throwIfAborted()` → **`type !== "api_key"` 时直接返回 `credential`
    （同一对象引用，不 clone）**；`api_key` 且 `key === undefined` 也原样返回；否则
    `{ ...credential, key: resolveConfigValue(credential.key, credential.env) }`（`$VAR` / `!command` / `$$` / `$!` 解析，
    结果按进程级 Map 缓存在 `resolve-config-value.ts`；`env` 里同名变量优先于 `process.env`）。
  - `ReadOnlyAuthStorage`（`--no-refresh` 检查用）：`read()` 里 `type !== "api_key" || !key || isCommandConfigValue(key)`
    → **`structuredClone(credential)`**；否则同样 `resolveConfigValue`。
- `AuthStorage.readLatestData(options)` 的额外行为（对差分测试很重要）：
  - 先 `getFileRevision(authPath)`（`dev:ino:size:mtimeNs:ctimeNs` 字符串）与缓存 revision 比对，相同则直接返回缓存 `data`；
  - 否则查看是否已有 in-flight `reload`（`{controller, promise, readers}`），有则复用；没有则新建；
    `readers++`；`raceWithAbortSignal(reload.promise, options?.signal)`；**无 signal 时**吞掉错误回落 `readState.data`；
    `finally` 里 `readers--`，归零则清空 reload 并 `controller.abort()`（即"最后一个读者取消即中止重载"）；
  - 构造时若已有 `sharedAuthFileReadState` 且 `authPath` 相同，则**共享同一 readState**（模块级单例，跨实例可见）；
    否则同步 `reload()`（`try { withLock(...) } catch { 保留上一次有效快照 }`）。
- `readStoredCredential(providerId, authPath = join(getAgentDir(), "auth.json"))`（导出、同步、一次性）：
  `JSON.parse(stripBom(readFileSync(normalizePath(authPath), "utf-8")))[providerId]`；**任何异常 → `undefined`**；
  **不**解析 `!command`/`$VAR`，**不**校验形状，**不**建实例。
- `delete(provider)`：锁内 `JSON.parse(stripBom(content))` → `delete data[provider]` → `next = JSON.stringify(data, null, 2)`
  （即使条目本来不存在也照样整文件重写）→ `updateReadState(latestData)`（**不**更新 revision，故 revision 置为 `undefined`）。
- `modify(provider, fn)`：锁内解析 → `await fn(currentData[provider])`；`undefined` → 返回原值且**不写文件**；
  否则 `merged = { ...currentData, [provider]: next }`、`next = JSON.stringify(merged, null, 2)`；返回 `next`，
  并 `updateReadState`。文件内容解析失败时 `parseStorageData` 直接 `JSON.parse` 抛出（**坏文件不会被覆盖**，
  见测试 `does not overwrite malformed auth files`）。

`auth.json` 的 JSON 形状（本单元定义的 `Credential`）：

```json
{
  "anthropic": {
    "type": "oauth",
    "access": "sk-ant-oat01-...",
    "refresh": "sk-ant-ort01-...",
    "expires": 1767225600000
  },
  "openai": {
    "type": "oauth",
    "access": "...",
    "refresh": "...",
    "expires": 1767225600000,
    "clientId": "oaiapp_issued",
    "scopes": ["openid", "profile", "email", "offline_access", "resource.invoke", "chatgpt.tokens.use.direct"]
  },
  "openai-codex": { "type": "oauth", "access": "...", "refresh": "...", "expires": 1767225600000, "accountId": "acct_..." },
  "github-copilot": {
    "type": "oauth", "access": "tid=...;exp=...;proxy-ep=proxy.individual.githubcopilot.com;...",
    "refresh": "ghu_...", "expires": 1767225600000, "enterpriseUrl": "company.ghe.com",
    "availableModelIds": ["gpt-5.4", "..."]
  },
  "openrouter": { "type": "oauth", "access": "sk-or-v1-...", "refresh": "", "expires": 9007199254740991 },
  "kimi-coding": { "type": "oauth", "access": "...", "refresh": "...", "expires": 1767225600000 },
  "meta": { "type": "oauth", "access": "LLM|...", "refresh": "<identity token>", "expires": 1767225600000 },
  "xai": { "type": "oauth", "access": "...", "refresh": "...", "expires": 1767225600000 },
  "radius": { "type": "oauth", "access": "...", "refresh": "...", "expires": 1767225600000, "scope": "gateway offline_access" },
  "openai-env-key": { "type": "api_key", "key": "sk-...", "env": { "CLOUDFLARE_ACCOUNT_ID": "..." } },
  "empty-key-provider": { "type": "api_key" }
}
```

字段约束（Rust serde 模型）：

| 字段 | 类型 | 必填 | 备注 |
|---|---|---|---|
| `type` | `"api_key" \| "oauth"` | 是 | 判别式 |
| `key` | string | 否 | 仅 api_key；可为 `!command` 形式（宿主解析） |
| `env` | `Record<string,string>` | 否 | 仅 api_key；同时参与 env 覆盖 |
| `access` | string | oauth 必填 | 请求凭证（api key 或 bearer token） |
| `refresh` | string | oauth 必填 | 刷新令牌；OpenRouter 为 `""`；Meta 为身份 token |
| `expires` | number（毫秒） | oauth 必填 | 绝对时间戳 |
| `accountId` | string | openai-codex | 从 access token 的 JWT claim 提取 |
| `clientId` | string | openai(chatgpt) | 回调中签发的 client id |
| `scopes` | string[] | openai(chatgpt) | 由 `scope` 字段按空白切分 |
| `enterpriseUrl` | string | github-copilot | 用户输入的域名（规范化后） |
| `availableModelIds` | string[] | github-copilot | 登录/刷新时抓取的账号可用模型 |
| `scope` | string\|undefined | radius | 服务端返回的 scope |
| `gatewayConfig` | object | radius（旧版兼容） | 见 `radius-config.ts`，本单元不写 |

### 4.2 环境变量

| 变量 | 读取位置 | 语义 |
|---|---|---|
| `KIANA_OAUTH_CALLBACK_HOST` | anthropic（模块初始化）、openai-chatgpt（模块初始化）、openai-codex（每次调用）、openrouter（每次调用） | 回调服务器监听地址与 `redirectHost` 无关的 host；空值/空白/未设 → `"127.0.0.1"`。判定用 `getProviderEnvValue(...) \|\| "127.0.0.1"`（JS 真值：`"0"` 也算真值）。**不影响** `redirectUri` 主机：Anthropic 用 `redirectHost: "localhost"`，Codex/ChatGPT 用固定 `localhost`/`127.0.0.1`，OpenRouter 用监听 host（因此 `KIANA_OAUTH_CALLBACK_HOST=localhost` 会让 `callback_url` 主机变 `localhost`）。Radius 不读它。 |
| `KIMI_CODE_OAUTH_HOST` | kimi-coding（每次 `getOauthHost()`） | 覆盖 OAuth host；优先于 `KIMI_OAUTH_HOST`；均去除尾随 `/` |
| `KIMI_OAUTH_HOST` | 同上 | 次选 |
| `KIANA_CODING_AGENT_DIR` | coding-agent 的 `getAgentDir()`（宿主） | 覆盖 agent 目录（auth.json 所在） |
| 各 provider 的 `*_API_KEY` | `envApiKeyAuth` → `ctx.env()` | 见 ai-core 的 `env-api-keys.ts`；本单元只用 `ctx.env` |

注意 `getProviderEnvValue(name)` 的顺序：`env?.[name] || process.env[name] || BunSandbox(/proc/self/environ) || undefined`
—— 空串会继续往后找。

### 4.3 HTTP 端点（逐字 URL / 方法 / 头 / body）

#### 4.3.1 Anthropic

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 授权（浏览器/复制码） | `GET https://claude.ai/oauth/authorize?<params>` | 浏览器打开 | query 顺序：`code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=<urlencoded>&scope=<urlencoded>&code_challenge=<urlencoded>&code_challenge_method=S256&state=<verifier>` |
| 兑换 | `POST https://platform.claude.com/v1/oauth/token` | `Content-Type: application/json`、`Accept: application/json` | `{"grant_type":"authorization_code","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","code":"…","state":"…","redirect_uri":"…","code_verifier":"…"}` |
| 刷新 | 同上 | 同上 | `{"grant_type":"refresh_token","client_id":"9d1c250a-e61b-44d9-88ed-5944d1962f5e","refresh_token":"…"}`（**无 scope**） |

URLSearchParams 序列化规则（WHATWG；Rust 必须自实现或使用等价实现）：除 `A-Za-z0-9*-. _` 外全部百分号编码，
空格 → `+`，`~` → `%7E`，`!`→`%21`，`(`→`%28`，`)`→`%29`，`'`→`%27`，`:`→`%3A`，`/`→`%2F`。实测样例：

```
code=true&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e&response_type=code&redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback&scope=org%3Acreate_api_key+user%3Aprofile+user%3Ainference+user%3Asessions%3Aclaude_code+user%3Amcp_servers+user%3Afile_upload&code_challenge=abc-_%7E*&code_challenge_method=S256&state=xyz
```

注意 `*`、`-`、`_`、`.` 不编码；`~` 编码为 `%7E`（这一点与 `curl`/多数 Rust 库不同，易错）。

#### 4.3.2 OpenAI Codex (legacy)

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 授权 | `GET https://auth.openai.com/oauth/authorize?...` | — | query 顺序：`response_type=code&client_id=app_EMoamEEZ73f0CkXaXp7hrann&redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback&scope=openid+profile+email+offline_access&code_challenge=…&code_challenge_method=S256&state=<32 hex>&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=<agentName ?? "kiana">` |
| 兑换/刷新 | `POST https://auth.openai.com/oauth/token` | `Content-Type: application/x-www-form-urlencoded` | 兑换：`grant_type=authorization_code&client_id=…&code=…&code_verifier=…&redirect_uri=…`；刷新：`grant_type=refresh_token&refresh_token=…&client_id=…` |
| 设备码 | `POST https://auth.openai.com/api/accounts/deviceauth/usercode` | `Content-Type: application/json` | `{"client_id":"app_EMoamEEZ73f0CkXaXp7hrann"}` |
| 设备轮询 | `POST https://auth.openai.com/api/accounts/deviceauth/token` | `Content-Type: application/json` | `{"device_auth_id":"…","user_code":"…"}` |
| 设备验证页 | `https://auth.openai.com/codex/device` | — | — |
| 设备码回调 redirect_uri | `https://auth.openai.com/deviceauth/callback` | — | 仅用于兑换 body |

无 `Content-Type`（form）时 Node fetch 会发送 `application/x-www-form-urlencoded;charset=UTF-8`；此处显式设置为
`application/x-www-form-urlencoded`（**无 charset**），Rust 侧必须照抄，不要附加 charset。

#### 4.3.3 OpenAI Sign in with ChatGPT

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 授权 | `GET https://auth.openai.com/api/accounts/authorize?...` | — | query 顺序：`client_id=dynamic_agent_client&agent_name_hint=<Kiana\|custom>&ext_agent_host_id=urn%3Auuid%3A<uuid>&response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&resource=https%3A%2F%2Fapi.openai.com%2Fv1&scope=openid+profile+email+offline_access+resource.invoke+chatgpt.tokens.use.direct&state=<43 字符 base64url>&code_challenge=…&code_challenge_method=S256&nonce=<43 字符 base64url>` |
| 兑换 | `POST https://auth.openai.com/api/accounts/oauth/token` | `accept: application/json`（小写）、`content-type: application/x-www-form-urlencoded` | 键顺序 `grant_type=authorization_code&client_id=<issued>&code=…&code_verifier=…&redirect_uri=http%3A%2F%2F127.0.0.1%3A1455%2Fauth%2Fcallback&resource=https%3A%2F%2Fapi.openai.com%2Fv1` |
| 刷新 | 同上 | 同上 | 键顺序 `grant_type=refresh_token&client_id=<issued>&refresh_token=…&resource=https%3A%2F%2Fapi.openai.com%2Fv1`（无 scope） |
| 回调 | `http://127.0.0.1:1455/auth/callback?code=…&state=…&client_id=<issued>` | — | 浏览器/粘贴 |

#### 4.3.4 GitHub Copilot

域 `domain` = 用户企业域（如 `company.ghe.com`）或 `github.com`。

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 设备码 | `POST https://<domain>/login/device/code` | `Accept: application/json`、`Content-Type: application/x-www-form-urlencoded`、`User-Agent: GitHubCopilotChat/0.35.0` | `client_id=Iv1.b507a08c87ecfe98&scope=read%3Auser` |
| 设备轮询 | `POST https://<domain>/login/oauth/access_token` | 同上三个头 | `client_id=Iv1.b507a08c87ecfe98&device_code=…&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code` |
| Copilot token | `GET https://api.<domain>/copilot_internal/v2/token` | `Accept: application/json`、`Authorization: Bearer <github access token>`、+ `COPILOT_HEADERS` | — |
| 模型目录 | `GET https://<apiHost>/models` | `Accept: application/json`、`Authorization: Bearer <copilot token>`、+ `COPILOT_HEADERS`、`X-GitHub-Api-Version: 2026-06-01` | — |
| 策略启用 | `POST https://<apiHost>/models/<modelId>/policy` | `Content-Type: application/json`、`Authorization: Bearer <copilot token>`、+ `COPILOT_HEADERS`、`openai-intent: chat-policy`、`x-interaction-type: chat-policy` | `{"state":"enabled"}` |

`<apiHost>` = `getGitHubCopilotBaseUrl(...)`：`proxy-ep=<host>` 的 `<host>` 把前缀 `proxy.` 换成 `api.`
（如 `proxy.individual.githubcopilot.com` → `api.individual.githubcopilot.com`），无 `proxy-ep` 时用
`copilot-api.<enterpriseDomain>`，再否则 `api.individual.githubcopilot.com`。

`COPILOT_HEADERS` 逐字：`{"User-Agent":"GitHubCopilotChat/0.35.0","Editor-Version":"vscode/1.107.0",
"Editor-Plugin-Version":"copilot-chat/0.35.0","Copilot-Integration-Id":"vscode-chat"}`。

#### 4.3.5 OpenRouter

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 授权 | `GET https://openrouter.ai/auth?callback_url=<urlencoded>&code_challenge=…&code_challenge_method=S256` | — | — |
| 兑换 | `POST https://openrouter.ai/api/v1/auth/keys` | `accept: application/json`、`content-type: application/json` | `{"code":"…","code_verifier":"…","code_challenge_method":"S256"}` |

响应成功：`{"key":"sk-or-v1-..."}`；失败：任意 JSON，detail 取 `error_description`/`message`/`error`/`error.message`。

#### 4.3.6 Kimi Code

host = `KIMI_CODE_OAUTH_HOST || KIMI_OAUTH_HOST || https://auth.kimi.com`（去尾斜杠）。

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 设备授权 | `POST <host>/api/oauth/device_authorization` | `Content-Type: application/x-www-form-urlencoded`、`Accept: application/json` | `client_id=17e5f671-d194-4dfb-9706-5516cb48c098` |
| 设备轮询 | `POST <host>/api/oauth/token` | 同上 | `client_id=…&device_code=…&grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code` |
| 刷新 | `POST <host>/api/oauth/token` | 同上 | `client_id=…&grant_type=refresh_token&refresh_token=…` |

设备授权响应字段：`device_code`、`user_code`、`verification_uri`、`verification_uri_complete`（都必填、非空、http(s)）、
`interval`（>0 才用）、`expires_in`（>0 才用）。token 响应：`access_token`、`refresh_token`、`expires_in`。

#### 4.3.7 Meta

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 设备授权 | `POST https://auth.meta.com/oidc/device/authorization/`（尾斜杠） | `Content-Type: application/x-www-form-urlencoded`、`Accept: application/json` | `client_id=1031625952748946` |
| 设备轮询 | `POST https://auth.meta.com/oidc/device/token/`（尾斜杠） | 同上 | `grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=…&client_id=…` |
| 铸造 key | `POST https://api.meta.ai/muse-code/key` | `Accept: application/json`、`Authorization: Bearer <identity token>`、`Content-Type: application/json`、`x-api-version: 1.0.0` | 字面量 `{}` |

铸造响应：`{"api_key":"LLM|…"}`；无 key 时可能为 `{"require_payment":true,"action_url":"https://…"}`。
401/403 → 会话失效。

#### 4.3.8 xAI

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| 设备码 | `POST https://auth.x.ai/oauth2/device/code` | `Accept: application/json`、`Content-Type: application/x-www-form-urlencoded` | `client_id=b1a00492-073a-47ea-816f-4c329264a828&scope=openid+profile+email+offline_access+grok-cli%3Aaccess+api%3Aaccess&referrer=kiana` |
| 轮询/刷新 | `POST https://auth.x.ai/oauth2/token` | 同上 | 轮询：`grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&client_id=…&device_code=…`；刷新：`grant_type=refresh_token&client_id=…&refresh_token=…` |

设备码响应：`device_code`、`user_code`、`verification_uri`（必填 https）、`verification_uri_complete`（可选 https）、
`expires_in`（必填 >0）、`interval`（>0 才用）。token 响应：`access_token`（必填）、`refresh_token`（刷新时可缺省）、
`expires_in`（可缺省 → 3600）。

#### 4.3.9 Radius（gateway）

`gateway` = `normalizeRadiusGatewayUrl(options.gateway)`（默认 `https://radius.kiana.dev`）。

| 用途 | 方法与 URL | 头 | body |
|---|---|---|---|
| discovery | `GET <gateway>/v1/oauth` | `accept: application/json` | — |
| token（兑换/刷新/轮询共用） | `POST <gateway>/v1/oauth/token` | `accept: application/json`、`content-type: application/x-www-form-urlencoded` | 兑换：`grant_type=authorization_code&client_id=kiana-gateway&redirect_uri=http%3A%2F%2F127.0.0.1%3A1456%2Foauth%2Fcallback&code=…&code_verifier=…`；刷新：`grant_type=refresh_token&client_id=kiana-gateway&refresh_token=…`；轮询：`grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&client_id=kiana-gateway&device_code=…` |
| 设备授权 | `POST <gateway>/v1/oauth/device` | 同上 | `client_id=kiana-gateway&scope=gateway+offline_access` |
| 浏览器授权 | `GET <authorizationEndpoint>?response_type=code&client_id=kiana-gateway&redirect_uri=http%3A%2F%2F127.0.0.1%3A1456%2Foauth%2Fcallback&scope=gateway+offline_access&code_challenge=…&code_challenge_method=S256&handoff=url&state=<uuid v4>` | — | — |

token 响应：`access_token`、`refresh_token`、`expires_in`、可选 `scope`。
错误响应：JSON `{error, error_description}`（或纯文本 body）。

### 4.4 回调服务器的 HTTP 行为

| 条件 | 状态码 | 页面 message | 对 wait 的影响 |
|---|---|---|---|
| `method !== GET` 或 path 不等 | 404 | `Callback route not found.` | 无 |
| state 不匹配（仅当 options.state 定义） | 400 | `State mismatch.` | 无 |
| 已 claimed 或已 settled | 409 | `This sign-in has already been handled.` | 无 |
| 有 `error` 查询参数 | 400 | `${providerName} authorization failed.`（details = `error_description ?? error`） | reject `Error("${providerName} authorization failed: ${description}")` |
| 无 `code` | 400 | `Missing authorization code.` | 无 |
| `complete()` 成功 | 200 | `Signed in to ${providerName}. You may now close this page.` | resolve 其返回值 |
| `complete()` 抛错 | 502 | `${providerName} sign-in failed.`（details = message） | reject 该错误（非 Error 包装为 `new Error(String(v))`） |
| 请求处理器内部未捕获异常（ChatGPT 专用服务器） | 500 | `Internal error while processing the callback.` | — |

**判定顺序（不可交换，决定了同一请求在不同状态下返回哪个码）**：

1. 通用服务器：`method !== "GET" || pathname !== path` → 404 ；
2. `options.state !== undefined && searchParams.get("state") !== options.state` → 400 ；
3. `claimed || settled` → 409 ；
4. `searchParams.get("error")` 存在 → 400 并 **settle(reject)** ；
5. `searchParams.get("code")` 缺失 → 400，**不** settle ；
6. `claimed = true` → 跑 `complete(code)` → 200 resolve / 502 reject。

推论：**已 claimed/已完成之后再来的"state 错误"请求返回 400（不是 409）**；**已完成之后带 `error` 的请求返回 409**；
**缺 code 的请求可以无限重试**（不 settle、不 claim）。ChatGPT 专用服务器的顺序不同：pathname 404 → `error` 400（reject）
→ 解析（catch 400，**不 settle**）→ 200 resolve；**没有** method 检查、**没有** state/claim 短路。

`cancel()` vs `close()`：`cancel()` → `wait()` **resolve `undefined`**（且此后回调 409）；`close()` → 若尚未 settle，
`wait()` **reject `"OAuth callback server closed"`** 并 `server.close()`。`close()` 在已 settle 时是幂等无操作。

响应头（通用服务器）：`content-type: text/html; charset=utf-8`、`cache-control: no-store`。
ChatGPT 专用服务器只有 `Content-Type: text/html; charset=utf-8`（无 cache-control）。
`Content-Length` 由 Node 以 chunked 传输代替（无显式长度）。

### 4.5 JSON 序列化细节（影响请求 body）

- `JSON.stringify` 保留插入顺序；数值格式：整数无 `.0`，`1e21` 及以上用指数（`1e+21`），`1e-7` 用指数，
  `0.000001` 用小数，`-0` → `0`，`NaN`/`Infinity` → `null`。本单元的 JSON body 只含字符串与整数，风险低，
  但 `JSON.stringify(json)` 用于错误消息（如 `Invalid Meta device authorization response: ${JSON.stringify(json)}`），
  因此 Rust 侧错误文本里的 JSON 必须与 V8 输出一致（键顺序 = 服务端返回顺序 → 必须 `preserve_order`）。
- `undefined` 值的键在对象中被省略（影响：`refreshGitHubCopilotAccessToken` 的 `enterpriseUrl: undefined`）。
- `Number.MAX_SAFE_INTEGER` = 9007199254740991，JSON 输出为 `9007199254740991`（非指数）。

### 4.6 浏览器页面 HTML（逐字模板）

`oauthSuccessHtml(message)` → `title = heading = "Authentication successful"`；`oauthErrorHtml(message, details?)` →
`title = heading = "Authentication failed"`，`details` 可选。模板（**逐字，含缩进与空行**；`LOGO_SVG` 为单行）：

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>${title}</title>
  <style>
    :root {
      --text: #fafafa;
      --text-dim: #a1a1aa;
      --page-bg: #09090b;
      --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    }
    * { box-sizing: border-box; }
    html { color-scheme: dark; }
    body {
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--page-bg);
      color: var(--text);
      font-family: var(--font-sans);
      text-align: center;
    }
    main {
      width: 100%;
      max-width: 560px;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
    }
    .logo {
      width: 72px;
      height: 72px;
      display: block;
      margin-bottom: 24px;
    }
    h1 {
      margin: 0 0 10px;
      font-size: 28px;
      line-height: 1.15;
      font-weight: 650;
      color: var(--text);
    }
    p {
      margin: 0;
      line-height: 1.7;
      color: var(--text-dim);
      font-size: 15px;
    }
    .details {
      margin-top: 16px;
      font-family: var(--font-mono);
      font-size: 13px;
      color: var(--text-dim);
      white-space: pre-wrap;
      word-break: break-word;
    }
  </style>
</head>
<body>
  <main>
    <div class="logo">${LOGO_SVG}</div>
    <h1>${heading}</h1>
    <p>${message}</p>
    ${details ? `<div class="details">${details}</div>` : ""}
  </main>
</body>
</html>
```

`LOGO_SVG` 逐字（单行）：

```html
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#F09082" d="M165.29 165.29H517.36V400H400V282.65H165.29Z"/><path fill="#4D9ABF" d="M165.29 282.65H282.65V400H400V517.36H282.65V634.72H165.29Z"/><path fill="#F1BE58" d="M517.36 400H634.72V634.72H517.36Z"/></svg>
```

`escapeHtml(value)` 顺序（**必须按此顺序**，否则会二次转义）：
`&`→`&amp;`、`<`→`&lt;`、`>`→`&gt;`、`"`→`&quot;`、`'`→`&#39;`（用 `replaceAll`）。
注意 `${details ? ... : ""}` 在无 details 时留下一个只有两空格缩进的空行。

### 4.7 AbortSignal 语义（本单元依赖的精确行为）

- `new AbortController().signal.reason` 在未 abort 时为 `undefined`；`abort()` 后为
  `DOMException`，`name = "AbortError"`，`message = "This operation was aborted"`。
- `AbortSignal.timeout(ms).reason` 为 `DOMException`，`name = "TimeoutError"`，
  `message = "The operation was aborted due to timeout"`。
- `AbortSignal.any([...])`：任一信号 abort 时以**该信号的 reason 原样**（同一对象）abort 结果信号。
- `signal.throwIfAborted()` 抛 `signal.reason`（若 reason 非 Error 也会原样抛）。
- `raceWithAbortSignal(operation, signal)`：
  - `signal.aborted` → `void operation.catch(() => {})` 然后 `Promise.reject(abortReason(signal))`；
  - 否则注册一次性监听，settled 后清理；`abortReason` 在 `reason === undefined` 时构造
    `Error("The operation was aborted")` 且 `name = "AbortError"`（这一分支在 Node 中几乎不可达）。
- `Models.getAuth` 取消时向上抛的是 `AbortError`（测试断言 `name === "AbortError"`）。

Rust 侧建议 `AbortSignal` 抽象为 `{ cancelled: AtomicBool, reason: Mutex<AbortReason>, notify: Notify }`，
`AbortReason = Abort | Timeout | Custom(Arc<dyn Any>)`，并提供 `throw_if_aborted() -> Result<(), AbortError>`。
`AbortError` 的 `Display` 必须输出 `This operation was aborted`（DOMException 的 message），
`TimeoutError` 输出 `The operation was aborted due to timeout`（若在日志/错误中出现）。

---

## 5. 跨单元接口

### 5.1 本单元提供（其它单元消费）

| 符号 | 消费方 | 契约 |
|---|---|---|
| `ProviderAuth` / `ApiKeyAuth` / `OAuthAuth` | ai-core（`models.ts`、各 provider 工厂） | 见 2.1；`login`/`resolve`/`check`/`refresh`/`toAuth` 的签名与语义 |
| `Credential` / `ApiKeyCredential` / `OAuthCredential` / `OAuthCredentials` / `CredentialInfo` | ai-core、coding-agent | 见 4.1 的 JSON 形状与字段约束 |
| `CredentialStore` | ai-core（`Models`）、coding-agent（`AuthStorage`、`RuntimeCredentials`） | `read`/`list`/`modify`/`delete`；`modify` 串行化、唯一写路径 |
| `AuthContext` / `defaultProviderAuthContext()` | ai-core、coding-agent | env/fileExists 语义（2.2） |
| `envApiKeyAuth(name, envVars)` / `lazyOAuth({...})` | ai-core 的 provider 工厂 | 2.3 |
| `InMemoryCredentialStore` | ai-core（`createModels` 默认值）、测试 | 2.4 |
| `resolveProviderAuth(provider, store, ctx, overrides?)` | ai-core `Models.getAuth/checkAuth/getAvailable/applyAuth` | 2.5；优先级与错误包装 |
| `refreshStoredOAuthCredential(store, providerId, oauth, needsRefresh, signal)` | ai-core `Models.resolveRefreshCredential` | 2.5；锁内二次检查 + 忽略 signal |
| `ModelsError` / `ModelsErrorCode` | 所有上层（CLI/TUI 展示） | `name = "ModelsError"`、`code` 六值、message 含 cause 细节 |
| `AuthPrompt` / `AuthEvent` / `AuthInteraction` / `ProviderAuthInteraction` / `AuthInfoLink` | coding-agent TUI（`LoginDialogComponent`）、扩展 API | 2.1；`prompt` 的 4 种类型与 `notify` 的 4 种事件 |
| `LoginOptions` | coding-agent（`getDeviceId`/`agentName`） | 2.1 |
| `AuthResult` / `AuthCheck` / `AuthType` / `AuthOperationOptions` / `ModelAuth` | ai-core、coding-agent CLI | 2.1 |
| 9 个 `load*OAuth()` + `registerBundledOAuthFlowLoaders()` | ai-core 的 `lazyOAuth`、`bun-oauth.ts` | 2.9 |
| `anthropicOAuth`、`openaiCodexOAuth`、`openaiChatGPTOAuth`、`githubCopilotOAuth`、`openRouterOAuth`、`kimiCodingOAuth`、`metaOAuth`、`xaiOAuth`、`createRadiusOAuth({name,gateway})` | `bun-oauth.ts`、测试、直接 import 的宿主 | 2.10–2.18 |
| `startOAuthCallbackServer()` / `waitForCallbackOrManualInput()` | 各 OAuth 流程 | 2.8 |
| `pollOAuthDeviceCodeFlow()` / `abortableSleep()` / 类型 | 各设备码流程 | 2.7 |
| `generatePKCE()` | 各浏览器流程 | 2.6 |
| `src/oauth.ts` 的 7 个类型 | coding-agent 扩展 OAuth 兼容 | 2.19；**运行时为空** |
| `registerBunOAuthFlows()` | Bun 单文件二进制入口 | 2.19 |

### 5.2 本单元消费（其它单元实现）

| 依赖 | 需要的精确形状 |
|---|---|
| `../types.ts`（ai-core） | `ProviderEnv = Record<string, string \| undefined>`、`ProviderHeaders = Record<string, string \| null>`（用于 `ModelAuth.headers`） |
| `utils/abort.ts`（ai-core） | `abortReason(signal): unknown`、`operationSignal(signal?): AbortSignal`、`raceWithAbortSignal<T>(op, signal): Promise<T>` |
| `utils/models-error.ts`（ai-core） | `ModelsError`（构造 `(code, message, {cause})`，message 自动拼接 cause）、`ModelsErrorCode` |
| `utils/provider-env.ts`（ai-core） | `getProviderEnvValue(name, env?): string \| undefined` |
| `utils/sleep.ts`（ai-core） | `sleep(ms, signal): Promise<void>`（abort 时 reject `signal.reason`） |
| `utils/oauth-page.ts`（ai-core） | `oauthSuccessHtml(message): string`、`oauthErrorHtml(message, details?): string`（§4.6 逐字） |
| `utils/diagnostics.ts`（ai-core） | `formatThrownValue(value): string` |
| `providers/github-copilot.models.ts`（ai-core） | `GITHUB_COPILOT_MODELS`（键集合 = 内置模型 id） |
| `providers/radius-config.ts`（ai-core） | `normalizeRadiusGatewayUrl(value): string` |
| Node 内建（Rust 标准库/tokio 替代） | `node:http`（`createServer`、`ServerResponse.writeHead/end`、`server.listen/address/close/closeAllConnections/once/on/off`）、`node:crypto`（`randomBytes`、`randomUUID`、`getRandomValues`、`subtle.digest`）、`node:fs/promises`（`access`）、`node:os`（`homedir`）、`fetch`/`Response`/`Headers`、`AbortController`/`AbortSignal`、`URL`/`URLSearchParams`、`atob`/`btoa`、`setTimeout`/`clearTimeout` |

### 5.3 ai-core 侧调用点（契约的另一半，供对照实现）

`Models`（ai-core，`packages/ai/src/models.ts`）：

- `resolveRefreshCredential(provider, stored, signal)`：oauth 且 `Date.now() < stored.expires` → 原样返回；
  已过期且 signal 未 abort → `refreshStoredOAuthCredential(store, id, oauth, cur => Date.now() >= cur.expires, signal)`；
  api_key → `apiKey.resolve({ctx, credential, signal})`，结果转 `{type:"api_key", key: result.auth.apiKey, env: result.env}`。
- `checkProviderAuth(provider, credential, signal)`：oauth → `{source:"OAuth", type:"oauth"}`（无 oauth handler → undefined）；
  api_key 有 `check` → 调它（异常包 `API key auth check failed for provider <id>`）；否则 `resolveProviderAuth(...)`。
- `getAuth(providerId|model, overrides)`：`resolveProviderAuth(provider, store, ctx, {...overrides, signal})`；
  传 model 时用 `mergeHeaders(result.auth.headers, model.headers)` 合并（大小写不敏感、后者覆盖）。
- `login(providerId, type, interaction, options)`：未知 provider → `ModelsError("provider", "Unknown provider: <id>")`；
  无该方法 → `ModelsError("auth", "<provider.name> does not support <type> login")`；
  调 `method.login({...interaction, signal}, options)` 并 race signal；随后 `credentials.modify(providerId, async () => credential, {signal})`，
  **只有当 mutation 尚未开始时** abort 才 reject（避免丢弃已开始的写入）；失败 →
  `ModelsError("auth", "Credential store modify failed for <id>", {cause})`；返回 credential。
- `logout(providerId, options)`：`credentials.delete`；失败 → `ModelsError("auth", "Credential store delete failed for <id>")`。
- `applyAuth`：`getAuth(model, {apiKey: options?.apiKey, env: options?.env, signal})`；无 → `ModelsError("auth", "Provider is not configured: <provider>")`。

### 5.4 与 `ai-core.md` 的分工与勘误

分工：`src/auth/**` 的行为以**本文件**为准；`src/utils/**`、`src/providers/**`、`src/models.ts` 的行为以
`ai-core.md` 为准。两处交集：

- `src/oauth.ts` / `src/bun-oauth.ts`：ai-core §2.1.15 有简述，本文件 §2.19 给出逐字导出面与不变量
  （`oauth.ts` 运行时为空、`bun-oauth.ts` 顶层 import 全部实现）。
- `utils/abort.ts` / `abort-signals.ts` / `models-error.ts` / `sleep.ts` / `provider-env.ts` / `oauth-page.ts`：
  ai-core §2.2.x 给出实现，本文件 §2.20 + §4.6/§4.7 给出本单元所需的精确语义（含 `raceWithAbortSignal`
  吞掉被弃 promise、`withCauseDetail` 的 `message.includes(detail)` 去重）。

勘误（以本文件为准）：

1. ai-core §5.2 写作 `AuthInteraction`(`prompt`/`notify`/`select`/`signal`)——实际类型只有 `prompt` 与 `notify`，
   **没有 `select` 方法**；"选择"通过 `prompt({type: "select", options})` 表达（返回值是 option id）。
2. ai-core §5.2 写作 `AuthResolution {auth, env?, source}`——实际类型名是 **`AuthResult`**（无 `AuthResolution`）。
3. ai-core §2.1.15 未提及 `KIANA_OAUTH_CALLBACK_HOST` 的"模块初始化时读取"陷阱（Anthropic/ChatGPT 是模块级常量，
   Codex/OpenRouter 是每次调用读取），以及 Anthropic 端口被占时的随机端口回退（#10571）。

---

## 6. npm 依赖与 Rust crate 映射、语义差异桥接

### 6.1 映射表

| npm / Web API | Rust crate | 备注 |
|---|---|---|
| `node:http` `createServer` / `listen` / `address` / `close` / `closeAllConnections` | `hyper` + `hyper-util` 或 `axum`（`tokio::net::TcpListener`） | 需能拿到**实际端口**（`listener.local_addr()`）；需"只接受现有连接、停止监听"的 `close`，以及强制断开空闲连接的等价物（`closeAllConnections`）；绑定失败需给出 `EADDRINUSE` |
| `fetch`（POST/GET、`response.ok/status/statusText/text()/json()/headers.get()`） | `reqwest`（`rustls-tls`，`json`、`gzip` 可选） | 错误信息不同（见 6.2 #3）；`statusText` 需自建映射（Node 从 HTTP reason phrase 取） |
| `AbortController`/`AbortSignal`/`AbortSignal.any`/`AbortSignal.timeout` | `tokio_util::sync::CancellationToken` + 自定义 `AbortSignal`/`AbortReason` | 见 4.7 与 6.2 #18 |
| `atob` / `btoa` | `base64` crate（`STANDARD`/`URL_SAFE_NO_PAD`） | `atob` 的宽松/报错行为需包装（6.2 #1） |
| `crypto.getRandomValues` / `randomUUID` / `randomBytes` / `subtle.digest("SHA-256")` | `getrandom`/`rand::rngs::OsRng`、`uuid`（v4）、`sha2` | UUID v4 小写带连字符；`randomBytes(16).toString("hex")` = 32 位小写 hex |
| `URL` / `URLSearchParams` | `url` crate + 自实现 form 序列化 | WHATWG 规范化与表单编码必须自建（6.2 #2、#4） |
| `node:fs/promises` `access` / `node:os` `homedir` | `tokio::fs::metadata` / `dirs::home_dir` | 保留 `~` 拼接语义 |
| `node:crypto` 动态 import 的"仅 Node 可用"守卫 | 无需（Rust 恒可用） | `createState()` 的 Node-only 错误在 Rust 中不可达，但保留错误类型/文案以对齐测试 |
| `setTimeout`/`clearTimeout` | `tokio::time::sleep` / `Interval` / `Instant` | 需可注入的 fake clock（`tokio::time::pause`/`advance` 或自定义 Clock trait） |
| `proper-lockfile`（宿主 auth.json） | `fs2`/`fd-lock` + 自实现 stale/重试 | 锁语义见 4.1；Rust 侧建议独立 crate `kiana-auth-storage` 或 coding-agent 的模块 |
| `JSON.stringify(obj, null, 2)` 落盘 | `serde_json` + `preserve_order` + 手写 2 空格缩进（`PrettyFormatter::with_indent(b"  ")`） | 键顺序 = 插入顺序；`undefined` 键省略；无末尾换行 |
| `JSON.parse` | `serde_json`（`preserve_order`） | 数字：整数保持 i64/u64，超出则 f64（与 JS 双精度差异见 6.2 #5） |
| `crypto.randomUUID()`（OpenRouter 路径、Radius state） | `uuid::Uuid::new_v4()` | 小写带连字符 |
| `structuredClone`（宿主） | `Clone` derive | — |
| `console.error`（本单元不使用） | — | 刷新失败**不得**写 stderr（测试断言） |
| `vi.useFakeTimers`/`vi.setSystemTime`（测试） | 自定义 `Clock` trait + `tokio::time::pause` | 时间线断言精确到毫秒 |

建议在 `kiana` 工作区中新增（或复用 ai-core 已规划的）以下依赖：
`tokio`（`rt-multi-thread`、`macros`、`time`、`sync`、`net`、`fs`）、`tokio-util`（`CancellationToken`）、
`reqwest`（`rustls-tls`、`json`、`stream`）、`serde`/`serde_json`（`preserve_order`）、`indexmap`、`url`、
`percent-encoding`、`base64`、`sha2`、`rand`/`getrandom`、`uuid`（`v4`）、`thiserror`/`async-trait`、
`http`/`http-body-util`/`hyper`/`hyper-util`（或 `axum`）、`futures-util`、`dirs`、`tracing`（可选，仅诊断）。

### 6.2 必须桥接的 JS/Node 语义差异

1. **`atob`（严格 base64，非 base64url）**：只接受 `A-Za-z0-9+/` 与 `=` 填充、忽略 ASCII 空白（空格/`\t`/`\n`/`\f`/`\r`，
   但**不**含 `\v` 0x0B、NBSP、U+3000、U+FEFF）；数据字符长度 mod 4 为 1（含长度 1）→
   `InvalidCharacterError: "The string to be decoded is not correctly encoded."`；含 `-`/`_` →
   `InvalidCharacterError: "Invalid character"`；填充必须有且仅有在末尾、且补齐到 4 的倍数（`YQ=`、`YQ===`、`AAAAA=` 都 → `"Invalid character"`；
   `YQ`、`YWI`、`YWJjZA` 这类**无填充**输入是合法的）。**精确规则与实测样本见 §8.4 #1**（原文此处的"长度 mod 4 为 1"
   只描述了错误的一半，会误拒 `YQ`/`YWI` 这类合法输入）。
   项目里两处用途：client id 解码（固定字符串，无风险）与 `decodeJwt`（JWT payload 常为 base64url →
   **含 `-`/`_` 时 atob 抛错 → `decodeJwt` 返回 null → `Failed to extract accountId from token`**）。
   Rust 若用 `STANDARD` 解码 JWT payload 会成功而 TS 失败 → **必须**模拟"严格标准 base64"行为。
2. **WHATWG URL 规范化**：小写 scheme/host、去掉默认端口（`https://x:443` → `https://x`）、
   `\` → `/`、`.`/`..` 折叠、非 ASCII host → punycode（`bücher.example` → `xn--bcher-kva.example`）、
   非 ASCII path → UTF-8 百分号编码、控制字符（如 ESC `0x1B`）→ `%1B`、保留 userinfo、
   query 中的 `\x1b` 等不编码（`https://github.com/login/\x1b]8;;evil` → `https://github.com/login/%1B]8;;evil`）。
   这是 `verification_uri` 规范化与 `normalizeDomain` 的基础（`new URL("company.ghe.com")` 会抛 → 补 `https://`）。
3. **`fetch` 错误面**：网络失败 → `TypeError: fetch failed`（`cause` 为底层错误）；DNS/连接拒绝/超时
   都是这个 TypeError。本单元只在少数地方检查 `signal.aborted` 后改写文案；其余原样冒泡到
   `formatErrorDetails`（Anthropic）或 `error.message`（Codex 刷新）或直接抛（其他）。
   Rust 侧为 reqwest 错误定义 `Display`，尽量输出与 Node 相同的 `fetch failed`（必要时包装）。
4. **`URLSearchParams` 序列化**：空格 → `+`；`A-Za-z0-9*-._` 不编码；`~` → `%7E`；`!`→`%21`；`(`/`)`→`%28`/`%29`；
   `'`→`%27`；`=`/`&` 在值中 → `%3D`/`%26`。**与 `url::form_urlencoded` 的差异**：`url` crate 不编码 `~` 并且
   对 `*` 的编码策略不同 → 必须自写 `form_urlencode`。解析方向：`+` → 空格；`%XX` 解码（含 `%00`）。
5. **数字与 JSON**：`expires` 等时间戳为毫秒（< 2^53，安全）；`Number.MAX_SAFE_INTEGER` 需精确输出；
   `serde_json` 默认会把 `9007199254740991` 输出为整数（正确）；但若建模为 f64 会输出 `9007199254740991.0`
   —— **必须**用 `i64`/`u64` 或 `serde_json::Number`。反向：解析服务端 JSON 的 `expires_in` 等为 f64 语义
   （`typeof === "number"`），Rust 需接受整数与浮点（`serde_json::Value::Number` → `as_f64`）。
6. **`Math.floor`/`Math.max`**：`intervalSeconds * 1000` 后 `floor`（负数 floor 向 -∞，但此处已保证 >0）。
7. **JS `Date.now()`**：墙钟毫秒（f64）。Rust `SystemTime::now().duration_since(UNIX_EPOCH).as_millis() as i64`。
   注意 fake timers 测试需要"可推进的墙钟"，建议 `Clock` trait：`now_ms() -> i64`。
8. **`Number.isFinite`**：`NaN`/`±Infinity` 为 false；JSON 中不可能出现（但对象可能是 `{}` 或字段缺失）。
9. **truthiness**：`credential?.key`（`""` 为假）、`env[name] || base.env(name)`、`if (value)`、`if (error)`、
   `if (trimmed && !enterpriseDomain)`、`description || String(status)`。Rust 侧 **空字符串不等于 truthy**，
   必须显式 `!s.is_empty()`；但注意 `getProviderEnvValue` 用 `||`，因此空串会继续回落到下一来源，
   而 `defaultProviderAuthContext.env` 对纯空白返回 undefined。
10. **字符串 `trim()`**：JS 的 `trim` 去除 Unicode 空白（含 ` `、`　`、BOM `﻿`）。
    Rust `str::trim` 用 `char::is_whitespace`（**不含 BOM `﻿`**）。凡用于 URL/凭证/域名的 trim，
    需自实现"JS trim"（`WhiteSpace` + `LineTerminator`，见 ECMA-262 定义）。
11. **`split`/`slice`/`includes`**：全部按 UTF-16 码元；本单元只用在 ASCII 场景（`#`、`code=`、`.`），
    但 `input.split("#", 2)` 的 "limit" 语义（只切前两个元素，剩余部分整体归第二个）必须照做。
12. **正则**：`/^[0-9a-f]{8}-...$/i`（UUID，固定大小写不敏感 ASCII）、`/proxy-ep=([^;]+)/`、`/^proxy\./`、
    `/\/+$/`、`/^https?:\/\//iu`、`/\s+/`、`/^Bearer\s+(.+)$/iu`、`/^(\d+)(ms|s|m|h)$/iu`。
    `regex` crate 均可直接编译（无 lookbehind/backref）。注意 `/iu` 的 Unicode 大小写折叠对 ASCII 无影响。
13. **`Object.hasOwn(GITHUB_COPILOT_MODELS, id)`**：只认自有属性；Rust 用 `HashSet<String>`/`BTreeSet`。
14. **`Array.prototype.flatMap`/`filter`/`map` 顺序**：保持数组顺序；`[...new Set([...a, ...b])]` 去重且保留首次出现。
15. **`JSON.stringify(json)` 作为错误消息的一部分**：必须与 V8 输出逐字一致（键顺序 = 解析顺序 → `preserve_order`；
    紧凑无空格；`null` → `"null"`；`undefined` 字段省略）。Kimi/Meta/Codex 的多条错误文案依赖它。
16. **`response.text()` / `response.json()`**：按 UTF-8 解码（无效字节 → U+FFFD）。**原文"不做 BOM 处理"是错的**：
    WHATWG `Response` 的 text/json 解码器会**剥离一个**前导 UTF-8 BOM（`EF BB BF`）——实测
    `await new Response("\uFEFF{\"a\":1}").text()` → `'{"a":1}'`（U+FEFF 被去掉），`json()` 同理会成功；
    连续两个 BOM 只去掉第一个（第二个作为 U+FEFF 保留在字符串里）。对照：**`JSON.parse` 本身不剥 BOM**
    （`JSON.parse("\uFEFF{}")` 抛 `SyntaxError`）。因此 Rust 侧：所有 `response.text()`/`response.json()` 的等价实现
    必须先剥一个 `\u{FEFF}`；而 coding-agent 的 `stripBom(content)`（`content.startsWith("\uFEFF") ? slice(1)`，
    对 `readFileSync(..., "utf-8")` 的结果）是**另一次**剥 BOM，两者不要混淆。
17. **`response.statusText`**：Node undici 从 HTTP reason phrase 取（如 `Unauthorized`、`Too Many Requests`）；
    HTTP/2 或空 phrase 时为空串。Rust 侧用 `StatusCode::canonical_reason()`（大多数一致）。
18. **AbortSignal 竞态**：`raceWithAbortSignal` 的"内部继续跑、外部立即拒绝"无法直接映射到 `select!`
    （`select!` 会 drop 另一分支）。正确做法：把内部 future `tokio::spawn`，保留 `JoinHandle`，
    abort 时丢弃 handle（任务继续），并用 `abort_reason.clone()` reject；或实现自定义
    `AbortableFuture`（poll 两个 future，abort 后仍继续 poll 内部但不返回）。`InMemoryCredentialStore`
    的 `enqueue` 与 `refreshStoredOAuthCredential` 的"锁等待可取消但刷新不可取消"都依赖这一点。
19. **计时器精度/顺序**：`abortableSleep(0)` 仍异步；`setTimeout` 的最小顺序（同一 tick 内先注册先执行）；
    fake timers 测试按毫秒推进，因此 Rust 侧 `tokio::time` 的 auto-advance 行为不同 → 单元测试需注入 Clock。
20. **HTTP 响应头与传输**：Node 会附加 `connection: keep-alive`、`date`、`keep-alive: timeout=5`、
    `transfer-encoding: chunked`；hyper 默认也可能 chunked 但头部集合不同。**测试只断言状态码与
    `content-type`**，因此不要求头部逐字一致；但 `content-type` 的**值**（含大小写键）在通用服务器
    为 `text/html; charset=utf-8`、ChatGPT 服务器为 `Content-Type: text/html; charset=utf-8`。
21. **`closeAllConnections()`**：hyper 无直接等价；可用 `hyper_util::server::graceful::GracefulShutdown` +
    记录连接并在 finally 中强制 drop，或对每个连接注册 `tokio::sync::watch` 取消信号。ChatGPT 流程的正确性依赖它。
22. **IPv6 括号**：`redirectHost.includes(":") ? `[${host}]` : host` —— Rust 侧用 `host.contains(':')` 判断
    （未做 IPv6 字面量解析，照抄即可）。
23. **`crypto.randomUUID()` 在路径中**：`/oauth/callback/<uuid>`；`new URL` 会保留路径（无编码变化）；
    测试断言 `/^\/oauth\/callback\/[0-9a-f-]+$/`。
24. **`getProviderEnvValue` 的模块级缓存**：`/proc/self/environ` 读取只发生一次并缓存在 `Map`（Bun 沙箱场景）；
    普通 Node 下 `process.env` 非空 → 永不读。Rust 直接读 `std::env::var`，无需该回退（但若要与 Bun 二进制
    行为一致，可选实现同款回退）。

### 6.3 需要"可注入"的外部性（便于测试与差分）

- **Clock**：`now_ms()`（`Date.now`）、`sleep(Duration)`、`timeout(Duration)`。
- **HttpClient**：`post_json`、`post_form`、`get_json`、`get_raw`（返回 status/statusText/headers/bytes），
  供测试 mock（对应 `vi.stubGlobal("fetch", ...)`）。
- **Rand**：`random_bytes(n)`、`random_uuid()`。
- **Env**：`get_env(name)`（对应 `process.env`）。
- **Listener**：`bind(host, port) -> TcpListener`（便于注入 EADDRINUSE）。
- **Interaction**：`&dyn AuthInteraction`（prompt/notify），测试用记录型实现。
- **Browser opener**：本单元**不**打开浏览器（只发 `auth_url`/`device_code` 事件；宿主负责 `openBrowser`），
  因此无需抽象。

---

## 7. 测试清单

所有测试位于 `packages/ai/test/`，使用 vitest 4。Rust 侧应把每一条改写为 `#[tokio::test]`（部分用
`rstest`/`#[test_case]`），并保留 fake clock（`vi.useFakeTimers` + `vi.setSystemTime` 的毫秒时间线）。

### 7.1 `oauth-auth.test.ts`（13 用例）——OAuthAuth 适配器与 lazy 加载链

`describe.sequential("OAuthAuth adapters")`：

1. **`keeps the extension OAuth barrel free of built-in flow implementations`**：`import * as extensionOAuthCompatibility from "../src/oauth.ts"` 不含 `loginAnthropic`、`anthropicOAuth`（模块运行时为空）。
2. **`identifies only subscription-backed OAuth flows as subscriptions`**：`anthropicOAuth`、`openaiChatGPTOAuth`、`openaiCodexOAuth`、`githubCopilotOAuth`、`kimiCodingOAuth`、`xaiOAuth` 的 `isSubscription === true`；`openRouterOAuth.isSubscription !== true`。
3. **`anthropic toAuth derives the api key from the access token`**：`{apiKey: access}`。
4. **`OpenAI exposes ChatGPT OAuth alongside API-key auth`**：`openaiProvider().auth.apiKey` 已定义；`auth.oauth` 匹配 `{name:"OpenAI (ChatGPT subscription)", isSubscription:true, loginLabel:"Sign in with ChatGPT"}`。
5. **`openai-codex toAuth derives the api key from the access token`**。
6. **`openrouter derives the api key and keeps the permanent credential on refresh`**：`refresh(credential)` **返回同一对象**（`toBe`）。
7. **`xAI toAuth derives the api key from the access token`**。
8. **`github-copilot toAuth derives baseUrl from the token proxy endpoint`**：`tid=abc;exp=123;proxy-ep=proxy.enterprise.example;rest` → `{apiKey: access, baseUrl: "https://api.enterprise.example"}`。
9. **`github-copilot toAuth falls back to the enterprise domain, then the individual endpoint`**：`enterpriseUrl: "https://company.ghe.com"` → `https://copilot-api.company.ghe.com`；无 → `https://api.individual.githubcopilot.com`。
10. **`anthropic refresh exchanges the refresh token and returns a typed credential`**：`type`/`access`/`refresh` 更新，`expires > Date.now()`。
11. **`github-copilot refresh preserves the enterprise domain`**：首个请求 URL 含 `api.company.ghe.com`；`refreshed.enterpriseUrl === "company.ghe.com"`；`/models` 返回 `{data: []}`。

`describe("OAuth through Models.getAuth (lazy load chain)")`：

12. **`resolves stored anthropic oauth credentials via the lazy flow import`**：存储 `expires = now + 10min`；`getAuth("anthropic")` → `auth.apiKey === "oauth-access-token"`、`source === "OAuth"`。
13. **`resolves stored github-copilot oauth credentials including per-credential baseUrl`**：`proxy.business.githubcopilot.com` → `https://api.business.githubcopilot.com`。

### 7.2 `oauth-callback-server.test.ts`（12 用例，`describe.sequential`）

`describe("OAuth callback server")`（每例后 `close()` 所有服务器）：

1. stray 请求与完成：`redirectUri` 形状；错误 path 404；state 错 400 + content-type + `State mismatch.`；POST 404；缺 code 400；成功 200 + `Authentication successful` + `Signed in to Example.` + 三个 logo 颜色 + `wait()` 得 `completed:the-code`。
2. `redirectHost` 生效（`localhost`）且 `state: undefined` 时跳过检查。
3. `complete` 失败 → 502 + `Example sign-in failed.` + 原始 message；`wait()` reject。
4. provider 重定向 error → 400 含 description；`wait()` reject `Example authorization failed: User denied access`。
5. 只完成第一个回调：第二个并发请求 409；`cancel()` 之后已 claimed 的仍完成（200）且 `wait()` 得 `done`。
6. `cancel()` → `wait()` 为 undefined；迟到回调 409。
7. abort → `Login cancelled`；`timeoutMs: 10` → `Example sign-in timed out`；预中止 → `start` reject `Login cancelled`。
8. 指定端口被占 → reject `{code: "EADDRINUSE"}`（**不**换端口）。

`describe("waitForCallbackOrManualInput")`：

9. 浏览器回调胜出 → `{type:"callback", value}` 且 prompt 的 `signal.aborted === true`。
10. 粘贴胜出 → `{type:"manual", input:"pasted"}`。
11. 无 callback 服务器 → 只用手动。
12. prompt 失败向上传播（`prompt cancelled`）。

### 7.3 `oauth-device-code.test.ts`（5 用例）

1. **`polls immediately and returns the completed value`**：`intervalSeconds: 2`、`expiresInSeconds: 30` → poll 于 t=0，+1999 仍只有 1 次，+1 后第二次 complete。
2. **`can wait before the first poll`**：`waitBeforeFirstPoll: true` → +1999 无 poll，+2000 首次。
3. **`increases the interval by 5 seconds after slow_down without a server interval`**：2s + slow_down → 第二次于 +7000。
4. **`honors a server-provided slow_down interval`**：`intervalSeconds: 30` → 第二次于 +30000。
5. **`cancels an in-flight wait`**：等待中 abort → reject `Login cancelled`。

### 7.4 `anthropic-oauth.test.ts`（7 用例，`describe.sequential`）

1. **`keeps the localhost redirect_uri for manual callback login`**：浏览器模式下粘贴重定向 URL；exchange body 的 `redirect_uri === "http://localhost:53692/callback"`；`grant_type`/`code`；只调用一次 fetch。
2. **`offers browser login first and uses the selected Anthropic copy code flow`**：select prompt 的精确 message/options；copy_code 的 `redirect_uri === "https://platform.claude.com/oauth/code/callback"`；粘贴 `code#state`；`state` 与授权 URL 的 state 一致。
3. **`cancels when Anthropic login method selection is cancelled`**：prompt 抛 `Login cancelled` → login reject。
4. **`omits scope from refresh token requests`**：refresh body 无 `scope`。
5. **`anthropicOAuth.login resolves through the manual_code prompt and aborts it after settling`**：manual_code 的 `signal` 在完成后被 abort。
6. **`completes login through the browser callback and shows the sign-in page`**：回调页 200 含 `Signed in to Anthropic.`；exchange 的 `redirect_uri` 等于回调服务器的 redirectUri。
7. **`falls back to a free callback port when the preferred port cannot be bound`**（#10571）：占用 53692 → redirectUri 主机 `localhost`、path `/callback`、port ≠ 53692。

### 7.5 `openai-codex-oauth.test.ts`（10 用例）

1. **`logs in with the OpenAI Codex device code flow`**：设备码请求体/头；轮询时间线 t=0、+5000；notify 字段（`verificationUri` = `https://auth.openai.com/codex/device`、`expiresInSeconds: 900`）；兑换 body（`redirect_uri` = 设备回调）；凭证含 `accountId`。
2. **`offers browser login first and uses the selected OpenAI Codex device code flow`**：select 精确文案；选中 `device_code` 走设备流程。
3. **`cancels when OpenAI Codex login method selection is cancelled`**。
4. **`cancels the OpenAI Codex device code flow while waiting`**：abort → `Login cancelled`。
5. **`times out the OpenAI Codex device code flow after 15 minutes`**：文案 `Device flow timed out`（无 slow_down）。
6. **`treats OpenAI Codex device auth 403 and 404 responses as pending`**。
7. **`includes the response body in OpenAI Codex device auth poll failures`**：`OpenAI Codex device auth failed with status …: <body>`。
8. **`does not write token refresh failures to stderr`**：401 刷新 → 错误含 `OpenAI Codex token refresh failed (401)` 与 body；`console.error` 未被调用。
9. **`uses the app's agent name as the browser login originator`**：`agentName: "my-app"` → query `originator=my-app`。
10. **`falls back to the pasted redirect URL when the fixed callback port is taken`**：占用 1455；粘贴回调；`redirect_uri` = `http://localhost:1455/auth/callback`；`accountId === "acct"`。

测试辅助 `createAccessToken(accountId)`：JWT = `base64(header).base64(payload).signature`，payload 为
`{"https://api.openai.com/auth":{"chatgpt_account_id": accountId}}`（**base64 含 `+`/`/`/`=`**，因此 `atob` 可解）。

### 7.6 `openai-chatgpt-oauth.test.ts`（7 用例）

1. **`registers a user-owned client and stores its issued ID and granted scopes`**：授权 URL 参数逐项断言；兑换 body 的 `client_id` 来自回调 `client_id`；凭证含 `clientId`/`scopes` 数组。
2. **`uses the app's agent name as the name hint`**。
3. **`rejects registration without an issued client ID`**：reject 文案；fetch **未**被调用。
4. **`rejects a token response that did not grant direct token use`**：缺 `chatgpt.tokens.use.direct` → `grant did not include chatgpt.tokens.use.direct`。
5. **`requires a device ID before starting authorization`**：无 deviceId 与非法 UUID 均 reject `requires a device ID`；授权未开始。
6. **`requires refresh responses to rotate the refresh token`**：缺 `refresh_token` → `token response has invalid refresh_token`。
7. **`refreshes with the credential's issued client ID and stores replacement scopes`**：body 含 `client_id`/`refresh_token`/`resource`，**无 scope**；`expires ≈ now + (3600−180)s`；`scopes` 替换。

### 7.7 `github-copilot-oauth.test.ts`（13 用例）

1. **`filters models to the authenticated account picker catalog`**：`availableModelIds` 过滤 `filterModels`。
2. **`falls back to explicitly enabled policy models when the picker catalog is empty`**（仅 Individual）。
3. **`does not fall back to policy models for non-Individual accounts`**。
4. **`does not retry model catalog throttling during credential refresh`**：429 时只请求 1 次。
5. **`reports device-code details through onDeviceCode`**。
6. **`updates only known, tool-capable, unconfigured account model policies`**：只对 `unconfiguredModelId` 发 policy 请求；`catalogRequestCount === 1`。
7. **`retries a throttled policy update after Retry-After`**：429 + `Retry-After: 1` → t+1000 第二次请求（999ms 时仍 1 次）。
8. **`continues policy updates after a transport failure`**。
9. **`stops policy updates and persists authentication when the retry delay exceeds the login budget`**：`Retry-After: 5` vs 5s 预算 → 只处理第一个模型，但凭证仍持久化（`store.read` 相等）。
10. **`rejects a non-http(s) verification_uri before it reaches onDeviceCode`**（`$(id>/tmp/pwned)`）。
11. **`normalizes verification_uri before it reaches onDeviceCode`**（ESC → `%1B`）。
12. **`waits before polling and increases the interval after slow_down`**：时间线 +5000/+10000/+17000（server interval=7）。
13. **`times out after repeated slow_down responses`**（`expires_in: 25` → 长版本超时文案）。

测试用 `testCopilotAccessToken = "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;"`，
`testCopilotModelsUrl = "https://api.individual.githubcopilot.com/models"`。

### 7.8 `openrouter-oauth.test.ts`（13 用例）

1. **`is exposed alongside API-key auth`**；2. **`resolves the same stored OAuth key for chat and image models`**；
3. **`runs PKCE on a one-shot loopback callback and exchanges the code for a permanent API key`**（请求体、callback path、challenge 校验、唯一一次 fetch）；
4. **`reports token exchange failures through both the callback page and login`**（502 + `...(HTTP 403): invalid code`）；
5. **`allows only one token exchange for a callback`**（第二次 409）；
6. **`rejects a successful response that does not contain a key`**；
7. **`mints a key from a pasted redirect URL when the loopback callback never arrives`**；
8. **`accepts a bare authorization code from the manual prompt`**（`"  manual-code  "` → trim）；
9. **`fails login when the manual prompt is cancelled`**（且不发请求）；
10. **`rejects empty manual input without exchanging a code`**（`"   "` → `Missing authorization code`）；
11. **`closes the pending callback when login is cancelled`**（取消后 fetch 回调地址 reject）；
12. **`rejects before opening a callback server when login is already cancelled`**（不 emit 事件）；
13. **`uses the configured OAuth callback host`**（`KIANA_OAUTH_CALLBACK_HOST=localhost` → callback hostname `localhost`）。

### 7.9 `kimi-coding-oauth.test.ts`（6 用例）

1. **`logs in with the device authorization flow`**：表单/头；notify 用 `verification_uri_complete`（`https://www.kimi.com/code?user_code=ABCD-1234`）；时间线 +5000/+10000；`expires` 无余量。
2. **`fails when the device code expires`**（`expired_token` → 含 `expired`）。
3. **`fails when the user denies the login`**（`access_denied` → 含 `denied`）。
4. **`honors the KIMI_CODE_OAUTH_HOST override`**（尾斜杠被去掉；URL 序列）。
5. **`refreshes tokens and returns a Bearer header for requests`**：`expires ≥ now + 3600s`；`toAuth` → `{headers:{Authorization:"Bearer new-access"}}`。
6. **`retries refresh on 429 and fails unauthorized on invalid_grant`**：429 后 t+1000 重试成功（2 次调用）；`invalid_grant` 不重试且含 `unauthorized`。

### 7.10 `meta-oauth.test.ts`（4 用例）

1. **`logs in with the device flow and mints a Model API key`**：notify 用 complete URI；时间线 +10000（两次轮询，pending 在 +5000）；凭证 `refresh = identity-token`、`access = LLM|minted-key`、`expires = now + 10000 + 24h`；mint 头 `Authorization: Bearer identity-token`。
2. **`re-mints the API key from the stored identity token on refresh`**。
3. **`reports the setup URL when Meta issues no key`**（`Complete setup at https://dev.meta.ai/billing`）。
4. **`uses the minted key as the request api key`**。

### 7.11 `xai-oauth.test.ts`（9 个 `it` + 2 个 `it.each`，展开后 14 个用例）

1. **`uses the device grant, delays polling, and handles pending and slow_down`**：表单（client_id/scope/referrer）；时间线 +5000、+10000（slow_down interval=10）、+20000；`expires = start + 20000 + 21600s − 300s`。
2. **`falls back to the default poll interval when the response reports interval 0`**（+5000）。
3. **`prefers verification_uri_complete when the server provides it`**。
4. **`rejects a non-https verification_uri_complete`**。
5. **`rejects a non-https verification URI: %s`**（`it.each(["http://…","file:///etc/passwd","not a url"])`）。
6. **`fails when device authorization is denied: %s`**（`it.each(["access_denied","authorization_denied"])`）。
7. **`cancels while waiting for the first token poll`**（只发 1 个请求）。
8. **`refreshes tokens and preserves an unrotated refresh token`**（`name === "xAI (Grok/X subscription)"`；`toAuth` → `{apiKey}`）。
9. **`assumes a one-hour lifetime when expires_in is missing`**（`+3600s − 300s`）。
10. **`rejects token responses with missing fields`**。
11. **`surfaces the upstream error code and description on refresh failure`**。

### 7.12 `radius-oauth.test.ts`（4 用例）

1. **`uses gateway endpoints directly for device login`**：URL 序列、表单字段、notify 字段、凭证含 `scope` 与 60s skew。
2. **`refreshes directly through the gateway without discovery`**。
3. **`discovers only the interactive browser authorization endpoint`**（`{issuer}` → `Invalid Radius OAuth config from <gateway>`）。
4. **`exchanges the browser callback code before showing the sign-in page`**：失败 502 含 `code expired`、login 错误含 `invalid_grant: code expired`；成功 200 含 `Signed in to Radius.`。

### 7.13 `env-api-keys.test.ts`（7 用例，属 ai-core 但覆盖本单元的 `envApiKeyAuth` 消费面）

`does not treat generic GitHub tokens as GitHub Copilot credentials`、`resolves GitHub Copilot credentials from COPILOT_GITHUB_TOKEN`、
`resolves ZAI China Coding Plan credentials from ZAI_CODING_CN_API_KEY`、`reports ANTHROPIC_AUTH_TOKEN but preserves OAuth token API key lookup`、
`does not return ANTHROPIC_AUTH_TOKEN as an API key`、`preserves ANTHROPIC_OAUTH_TOKEN as an API key`、
`falls back to ANTHROPIC_API_KEY for API key lookup`。

### 7.14 相邻单元的 auth 相关测试（不在本单元文件内，但会约束本单元行为）

`packages/ai/test/`：

| 文件 | 与本单元相关的断言 |
|---|---|
| `providers.test.ts` | `envApiKeyAuth`：存储 key 优先 → `source "stored credential"`；env 回退 → `source === "SECOND_KEY"`；全缺 → undefined；`login` 的 secret prompt → `{type:"api_key", key:"entered-key"}`。另有 Anthropic bearer/OAuth token 优先级、Bedrock/AWS、Cloudflare env、Vertex ADC 等（ai-core）。 |
| `models-runtime.test.ts` | **优先级/回退**：`resolves auth: stored credential owns the provider, ambient only when nothing stored`（stored api_key 的 `source` 为 `"stored"`——此处是**测试自定义** `ApiKeyAuth` 的 source，不是 `envApiKeyAuth` 的 `"stored credential"`）、`a stored credential without a matching handler blocks ambient fallback`（对应 §2.5 步骤 5 的"否则 undefined，绝不回退 env"）。**刷新窗口**：`refreshes expired oauth credentials and persists the rotated credential`、`refreshes oauth credentials with less than five minutes remaining`、`honors a caller's longer OAuth minimum validity`（`minOAuthValidityMs` 覆盖 5 分钟默认）、`refreshes expired OAuth before refreshing models`。**取消/持久化**：`persists an OAuth refresh that started before the model refresh was cancelled`、`persists an OAuth refresh that started before the request was cancelled`（`getAuth` reject `AbortError` 后凭证仍被更新）、`cancels queued credential mutations without running them later`、`passes caller signals to provider auth callbacks`、`stops waiting for non-cooperative auth callbacks`、`restores cached models before waiting for network auth`。**其它**：`checks provider auth without refreshing OAuth and filters available models`、`runs provider login and logout through the credential store`。 |
| `classifier-models.test.ts` | `lists OpenAI Decisions models only for API key credentials`（依赖 `openai` provider 的 `filterAllModels` 与 oauth 凭证类型）。 |
| `oauth.ts`（helper） | 读写 `~/.kiana/agent/auth.json`：`JSON.stringify(storage, null, 2)` + `chmodSync(0o600)`；目录 `mkdirSync(recursive, 0o700)`；过期 OAuth 用 provider 的 `auth.oauth.refresh` 刷新后写回；`toAuth(...).apiKey`。 |

`packages/coding-agent/test/`：

| 文件 | 覆盖 |
|---|---|
| `auth-storage.test.ts`（**30** 例，原写 28 漏了两个 `test.skipIf(process.platform === "win32")` 权限用例） | `reads and resolves stored API-key credentials` / `resolves command-backed API-key credentials` / `returns OAuth credentials unchanged` / `credential-scoped env takes precedence and remains inspectable` / `coalesces file reloads across concurrent readers and storage instances` / `keeps a coalesced reload alive while another credential reader is waiting` / `creates new auth files with owner-only permissions`（skipIf win32）/ `preserves the mode of an existing auth file`（skipIf win32）/ `modify persists a credential while preserving unrelated external edits` / `modify with undefined leaves the current credential unchanged` / `serializes concurrent modifications` / `delete removes one credential while preserving others` / `in-memory storage implements the same credential-store behavior` / `does not write after lock acquisition failure and recovers on retry` / `retries a briefly contended file lock` / `retries transient EPERM on Windows (async)` / `surfaces persistent EPERM on Windows after a bounded number of retries (async)` / `does not retry EPERM on other platforms (async)` / `retries transient EPERM on Windows (sync)` / `surfaces a compromised file storage lock` / `pre-aborted file operations do not create the backing file or run the mutation` / `aborts while waiting for a held file lock without running the mutation later` / `releases a file lock acquired concurrently with cancellation before mutation` / `holds the file lock until a cancelled active callback settles without committing it` / `cancels a signalled credential read waiting for a held file lock` / `serializes in-memory mutations across providers` / `cancels a queued in-memory mutation without running it later` / `preserves the stored credential after cancelling an active refresh mutation` / `translates a credential-store refresh failure and allows a later retry` / `does not overwrite malformed auth files`。两个权限用例的断言：新建文件 `mode & 0o777 === 0o600`（父目录 0o700），已存在文件（先 `chmod 0o660`）用 `modify` 写回后 mode 仍为 `0o660`（TS 只在**创建**时用 0o600，不 chmod 已存在文件——§4.1 与此处一致，但**不要**误实现为"每次写完都 chmod 0o600"）。 |
| `auth-check.test.ts`（10 例） | ready/由 `--model` 解析 provider/`--no-refresh` 不刷新/默认刷新/未知 provider not_ready/未解析的 stored env 引用不算配置/损坏状态 invalid/不创建 auth 文件/`--json`+`--credentials`+`--no-refresh` 组合/无 catalog 存储的 runtime。 |
| `auth-url-copy.test.ts`（3 例） | 登录对话框的 copy 键复制 auth URL 而非输入到 code 框；无 URL 时忽略；MCP 登录页复制授权 URL。 |
| `runtime-credentials.test.ts`（5 例） | 运行时覆盖屏蔽存储凭证且不持久化；枚举合并且不暴露 key；转发 signal；持久删除被取消时保留覆盖；delete 同时清覆盖与存储。 |

### 7.15 Rust 侧测试要求（差分/一致性）

1. 上述每个用例都要有对应 Rust 测试；fixture 的服务端响应与请求断言逐字复制（含 JSON 键顺序）。
2. 需要 fake clock 的用例（device-code、Copilot 时间线、Meta/Kimi 的 expires）必须用可注入 Clock，
   并断言**毫秒级**时间点（`+5000`、`+10000`、`+17000` 等）。
3. 回调服务器测试需真实 TCP（`127.0.0.1:0`），并断言状态码、content-type、页面文本片段。
4. `auth.json` 差分测试：给定一组凭证，Rust 写出的文件字节必须与 JS 的 `JSON.stringify(data, null, 2)`
   完全一致（键顺序、`undefined` 省略、无末尾换行）。
5. 错误文案差分：所有 §3.6 的字符串都要有断言（包括 `ModelsError` 拼接后的完整 message）。
6. 并发/取消语义：`modify` 队列取消、刷新忽略 signal、刷新后强校验（`minOAuthValidityMs`）都必须有独立测试。

---

## 8. Rust 模块布局与最难复刻部分

### 8.1 crate 与模块布局

本单元归属 `kiana-ai` crate（与 ai-core 共享），新增 `src/auth/` 子树；不新建 crate，避免 `Provider` / `Credential`
类型跨 crate 重复定义。

```
kiana/
└── crates/
    └── kiana-ai/
        ├── Cargo.toml
        └── src/
            ├── lib.rs                     # pub mod auth; pub use auth::{...};
            ├── auth/
            │   ├── mod.rs                 # 重导出 + OAuthAuth 注册表（lazy）
            │   ├── types.rs               # ProviderAuth / Credential / AuthPrompt / AuthEvent / AuthInteraction / ...
            │   ├── context.rs             # AuthContext / FileSystemEnv（env + fileExists）
            │   ├── helpers.rs             # env_api_key_auth(...) / lazy_oauth(...)（对应 src/auth/helpers.ts）
            │   ├── credential_store.rs    # CredentialStore（内存 + 文件）、per-provider 队列、锁、权限
            │   ├── file_store.rs          # ~/.kiana/agent/auth.json 读写、mkdir 0700、chmod 0600、lockfile
            │   ├── resolve.rs             # resolve_provider_auth / refresh_stored_oauth_credential / AuthResult
            │   └── oauth/
            │       ├── mod.rs             # pub use 各 flow；`OAUTH_PROVIDERS: &[&dyn OAuthAuth]`
            │       ├── pkce.rs            # random_state / generate_pkce / base64url
            │       ├── device_code.rs     # run_device_code_flow(...) 通用 RFC 8628 轮询器
            │       ├── callback_server.rs # OAuthCallbackServer / wait_for_callback_or_manual_input
            │       ├── anthropic.rs
            │       ├── openai_codex.rs
            │       ├── openai_chatgpt.rs
            │       ├── github_copilot.rs
            │       ├── openrouter.rs
            │       ├── kimi_coding.rs
            │       ├── meta.rs
            │       ├── xai.rs
            │       ├── radius.rs
            │       └── load.rs            # get_oauth_auth(id) -> Option<Arc<dyn OAuthAuth>>（懒加载）
            └── utils/
                ├── oauth_page.rs          # render_oauth_page / escape_html / LOGO_SVG
                ├── abort.rs               # AbortSignal 抽象（见 6.3）
                ├── abort_signals.rs       # any / timeout
                ├── sleep.rs               # sleep(ms, &signal)
                ├── headers.rs             # resolve_header_values（$VAR / !cmd / $$ / $!）
                ├── diagnostics.rs         # kiana_log（含 redact）
                └── models_error.rs        # ModelsError + with_cause_detail
```

`kiana-ai/src/oauth.rs`（扩展兼容 barrel）与 `bun-oauth.rs` 属于 ai-core 已规划的 `oauth` 模块，本单元只提供其
底层实现；在 `lib.rs` 中以 `pub mod oauth_compat;` 暴露与 TS 等价的常量（空 built-in 实现 + 类型别名）。

`providers/*.rs` 的 `auth` 字段构造本单元导出的 `OAuthAuth` 具体类型（`AnthropicOAuth` 等），其 `to_auth` /
`refresh` 直接调用本单元函数。

### 8.2 关键类型草图

```rust
// ---------- types.rs ----------
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Credential {
    ApiKey(ApiKeyCredential),   // { type: "api_key", key, env?, ...extra }
    OAuth(OAuthCredential),     // { type: "oauth", access, refresh, expires, ...extra }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    pub key: Option<String>,   // TS `key?: string`（`type:"api_key"` 可完全没有 key，见 §2.3/§4.1 的 empty-key-provider）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<IndexMap<String, String>>,
    #[serde(flatten)]
    pub extra: IndexMap<String, JsonValue>, // 保序、保真回写（env 之外的未知字段）
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub access: String,
    pub refresh: String,
    pub expires: f64,        // JS number（毫秒，可含小数）
    #[serde(flatten)]
    pub extra: IndexMap<String, JsonValue>, // accountId/clientId/scopes/enterpriseUrl/scope/deviceId...
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKeyAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<Arc<dyn OAuthAuth>>, // 序列化时跳过（TS 中为函数对象）
}

#[derive(Clone, Debug)]
pub struct ApiKeyAuth {
    pub name: String,
    pub login: Arc<dyn Fn(LoginContext) -> BoxFuture<'static, Result<ApiKeyCredential, AuthError>> + Send + Sync>,
    pub resolve: Arc<dyn Fn(&AuthContext) -> BoxFuture<'static, Result<AuthResult, AuthError>> + Send + Sync>,
    // ... 见 5.1 表；此处省略
}

// ---------- 交互 ----------
#[derive(Clone, Debug)]
pub enum AuthPrompt {
    Text { message: String, placeholder: Option<String> },
    Secret { message: String, placeholder: Option<String> },
    Select { message: String, options: Vec<AuthSelectOption> },
    ManualCode { message: String, placeholder: Option<String> },
}

#[derive(Clone, Debug)]
pub enum AuthEvent {
    Info { message: String, detail: Option<String> },
    AuthUrl { url: String, instructions: Option<String> },   // TS 的 auth_url 事件**没有** openBrowser 字段（原稿多写）
    DeviceCode { user_code: String, verification_uri: String, interval_seconds: Option<f64>,
                 expires_in_seconds: Option<f64>, open_browser_on_confirm: Option<bool>,
                 copy_code_on_confirm: Option<bool> },
    Progress { message: String },
}

#[async_trait]
pub trait AuthInteraction: Send + Sync {
    async fn prompt(&self, req: AuthPromptRequest) -> Result<AuthPromptResponse, AuthError>;
    fn notify(&self, event: AuthEvent);
    fn signal(&self) -> &AbortSignal;   // 组合后的取消信号
}
```

要点：

- `Credential` 用 `#[serde(tag="type")]` + `#[serde(flatten)] extra: IndexMap` 保留未知字段（`accountId`、
  `clientId`、`scopes`、`enterpriseUrl`、`deviceId`、`ghe` 等），保证「读入再写回」字节稳定。
- `expires` 必须是 `f64`（TS 中为 JS number）；写出时需按 JS `JSON.stringify` 规则格式化（整数值不带 `.0`，
  超出 `1e21` 用指数、`-0` 写 `0`）。
- `AuthPrompt`/`AuthEvent` 的字段名与 TS 完全一致（`intervalSeconds`、`expiresInSeconds`、`verificationUri`、
  `userCode`、`openBrowserOnConfirm`、`copyCodeOnConfirm`；`info` 事件另有 `links: Option<Vec<AuthInfoLink>>`，
  `AuthInfoLink = { url, label? }`）。**注意原先多写的 `openBrowser` 并不存在于 TS 的 `AuthEvent` 里**
  （`auth_url` 只有 `url` + `instructions`）；`AuthPrompt` 的四种变体还各自带可选的 `signal`（单次提示的取消信号），
  Rust 侧对应 `AuthPromptRequest.signal`。Rust 侧用
  `#[serde(rename_all = "camelCase")]` 并保证只做内部传递，TUI 层直接消费。
- `AuthInteraction` 在 Rust 中用 `async_trait` + `BoxFuture`，TUI 侧实现为 channel + oneshot。

```rust
// ---------- credential_store.rs ----------
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn read(&self, provider: &str) -> Result<Option<Credential>, AuthError>;
    async fn list(&self) -> Result<Vec<CredentialListItem>, AuthError>;  // 顺序 = providers 插入序
    async fn modify<F, Fut>(&self, provider: &str, f: F) -> Result<(), AuthError>
        where F: FnOnce(Option<Credential>) -> Fut + Send + 'static,
              Fut: Future<Output = Result<Option<Credential>, AuthError>> + Send;
    async fn delete(&self, provider: &str) -> Result<(), AuthError>;
}
```

`modify` 是唯一写路径；实现内是「per-provider FIFO 队列 + 全局 map of queues」，算法逐字见 §2.4。
Rust 泛型 + `dyn` 的兼容写法：对外用 `modify` 泛型方法时不满足 object safety，因此实际对外暴露
`Arc<dyn CredentialStore>` 时改用 `modify_boxed(provider, Box<dyn FnOnce(Option<Credential>) -> BoxFuture<...>>)`，
并在 `CredentialStore` 上提供 blanket 泛型装饰方法。这是 TS 与 Rust 在 trait 设计上的主要差异，需在实现时
二选一并在 crate 内部固定。

```rust
// ---------- oauth trait ----------
#[async_trait]
pub trait OAuthAuth: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn is_subscription(&self) -> bool { false }
    fn login_label(&self) -> &str;
    async fn login(&self, ctx: LoginContext) -> Result<OAuthCredential, AuthError>;
    async fn refresh(&self, cred: &OAuthCredential, signal: &AbortSignal) -> Result<OAuthCredential, AuthError>;
    async fn to_auth(&self, cred: &OAuthCredential) -> Result<ModelAuth, AuthError>;
}

// `ModelAuth` = TS 的 `{ apiKey?, headers?, baseUrl? }`，不是 provider 的 `ProviderAuth`。
#[derive(Clone, Debug, Default)]
pub struct ModelAuth {
    pub api_key: Option<String>,
    pub headers: Option<IndexMap<String, Option<String>>>, // ProviderHeaders 允许 null 值
    pub base_url: Option<String>,
}

// `LoginContext` = `ProviderAuthInteraction & LoginOptions`（TS 侧二者分列，Rust 合并为一个入参）。
pub struct LoginContext {
    pub interaction: Arc<dyn AuthInteraction>,
    pub get_device_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub agent_name: Option<String>,
}

// TUI 侧的请求/响应（Rust 用具体类型，避免 stringly-typed 的返回值）。
pub struct AuthPromptRequest {
    pub prompt: AuthPrompt,
    pub signal: AbortSignal, // 单次提示的取消信号（AuthPrompt.signal）
}
pub enum AuthPromptResponse {
    Value(String), // text/secret/manual_code 的输入；select 的 option id
}
```

### 8.3 与 ai-core 的对接点

- `resolve_provider_auth`（§2.5）返回的 `AuthResult` 必须与 ai-core 的 `ProviderAuthResult` 同构：
  `{ api_key: Option<String>, headers: Option<IndexMap<String,String>>, base_url: Option<String>, source: Option<String> }`。
  `source` 取值逐字：`"stored credential"`（`envApiKeyAuth`）、`"OAuth"`（OAuth 解析）、以及各 provider 自定义。
- `ModelsError` 的 `with_cause_detail` 在 ai-core 的 `utils/models_error.rs` 中实现；本单元只产出
  `AuthError { message, cause: Option<Box<dyn Error>> }`，拼接由 ai-core 负责（§5.3）。
- `CredentialStore` 的默认实现 `FileCredentialStore::new(agent_dir)` 由 coding-agent 的 `AuthStorage` 注入；
  kiana-ai 只提供 `InMemoryCredentialStore`（测试用）。

### 8.4 最难复刻的部分（按风险降序）

1. **`atob` 的严格标准 base64 语义**（§6.2 #1）
   `decode_jwt` 用 `atob`，而 Node `atob` 拒绝 base64url 的 `-`/`_`、拒绝非法填充。原文的
   "末尾 `=` ≤ 2 个 + 总长 `%4 == 0`" **不是** Node 的真实规则（实测 Node v26 `Buffer` 的 `atob`，即 simdutf 后端）。
   真实规则（去掉所有 ASCII 空白 `\t \n \f \r 空格` 后按字符逐个校验，注意 `\v`(0x0B) **不**算空白）：

   1. 任一非 `[A-Za-z0-9+/=]` 字符 → `InvalidCharacterError: "Invalid character"`；
   2. 填充只允许出现在**末尾**，且 `=` 的数量必须恰好补齐到 4 的倍数（`n` 个数据字符 + `p` 个 `=`，`(n+p) % 4 == 0`，
      且 `p ∈ {0,1,2}`；`p==1` 时要求 `n % 4 == 3`，`p==2` 时要求 `n % 4 == 2`）；否则 `"Invalid character"`；
   3. 剩余数据字符数 `n % 4 == 1`（含 `n==1`）→ `"The string to be decoded is not correctly encoded."`；
   4. 否则解码（长度 2/3 mod 4 的**无填充**输入是**合法**的，会补零位解码，例如 `"YQ"` → `"a"`、`"YWI"` → `"ab"`、
      `"YWJjZA"` → `"abcd"`）。

   实测样本（必须作为差分用例）：`"YQ"`→`"a"`、`"YWI"`→`"ab"`、`"YQ=="`→`"a"`、`"YQ==="`→`Invalid character`、
   `"YQ="`→`Invalid character`、`"Y"`→`not correctly encoded`、`"YWJjZA="`→`Invalid character`、`"YW Jj"`→`"abc"`、
   `"AAA="`→2 字节、`"AAAAA"`→`not correctly encoded`、`"AAAAA="`→`not correctly encoded`、`"AAAA=="`→`Invalid character`、
   `"YQ\u00A0=="`（NBSP）→`Invalid character`。
   Rust `base64::STANDARD` 更宽松，必须手写该校验，否则返回 `None`（TS 侧为 try/catch 返回 null）。
   任一宽松处理都会把 `Failed to extract accountId from token` 变成成功登录。

2. **`raceWithAbortSignal` / `AbortSignal` 的 reason 语义**（§4.7、§6.2 #18）
   TS 的 `raceWithAbortSignal(promise, signal)`：signal 已 abort → 立即抛 `signal.reason`；否则 race，且
   **被抛弃的原 promise 的 rejection 必须被吞掉**（否则 unhandled rejection 会让进程崩溃）。Rust 需
   `tokio::select!` + `futures::future::abortable` + 对 loser 的 `drop`（`JoinHandle` 自然取消即可，但
   若底层是 `spawn` 的 IO 需显式忽略错误）。`AbortError` 的 message 必须逐字：
   `"This operation was aborted"`；`TimeoutError`：`"The operation was aborted due to timeout"`；
   `AbortSignal.timeout(ms)` 超时后 reason 为 `TimeoutError`；`AbortSignal.any([])` 永挂起。
   同时注意「刷新一旦开始就忽略调用方 signal」（§2.5），即刷新内部用**独立**的 15 s timeout signal。

3. **文件锁 + 原子写 + 权限**（§4.1）
   原文这里的锁参数（`stale` 10s、`retries` 10 次 `retryDelay` 100ms）**与源码不符**，正确值如下
   （`packages/coding-agent/src/core/auth-storage.ts`，与 §4.1 一致）：

   - 异步：`lockfile.lock(authPath, { realpath: false, retries: 0, stale: 30_000, onCompromised })`；
     **`retries: 0`**（重试由调用方自己做），`stale = 30s`，`maxDelayMs = 2_000`，`deadline = now + 30_000`；
     失败码仅 `ELOCKED`（以及 Windows 上的 `EPERM`）可重试；
     `baseDelayMs = min(10 * 2**retry, maxDelayMs / 2)`；`delayMs = min(round(baseDelayMs * (1 + Math.random())), remainingMs)`；
     `EPERM` 额外最多 `MAX_EPERM_RETRIES = 5` 次；每次尝试前后都 `signal?.throwIfAborted()`；
     拿到锁后若 `signal.aborted` 则**先 `await release()` 再 `throwIfAborted()`**。
   - 同步：`lockfile.lockSync(path, { realpath: false })`，最多 `maxAttempts = 10` 次，每次失败后忙等 `20ms`。
   - `onCompromised` 只记录错误；后续每个检查点 `throwIfCompromised()` → 抛该错误，
     没有记录时用 `"Auth storage lock was compromised"`；`finally` 里 `release()` 的错误被忽略。
   - 写文件：`writeFileSync(path, next, { encoding: "utf-8", mode: 0o600 })` —— **mode 只在创建时生效**，
     已存在文件不改 mode/ACL（**没有** `chmod 0o600`！）；内容为 `JSON.stringify(data, null, 2)`，无末尾换行。
     目录：`mkdirSync(dir, { recursive: true, mode: 0o700 })`，同样只在创建时生效（**没有**后续 chmod）。
   - 文件不存在时先写 `"{}"`（同一 `mode: 0o600` 选项），再读、再加锁之后的操作。
   - Rust 用 `fs2::FileExt::try_lock_exclusive` + 自旋无法等价（`proper-lockfile` 是 **mkdir 目录锁**，
     崩溃后靠 `stale` 清理）；建议**逐字实现 mkdir 锁**以保证与 TS 进程共享 `~/.kiana/agent/auth.json` 时互操作。
     Windows 上 `rename` 到已存在文件会失败，需 `replace` 语义（TS 用 `writeFile` 覆盖，不 rename）。

4. **JSON 序列化的字节级一致**（§4.5）
   `JSON.stringify(data, null, 2)`：键顺序 = 对象插入序；`undefined` 字段省略（Rust `Option::None` +
   `skip_serializing_if`）；数字格式（整数无小数点、`1e21` → `1e+21`、`1e-7` → `1e-7`、`NaN`/`Infinity` →
   `null`）；字符串转义（`\"`、`\\`、`\b\f\n\r\t`、`<0x20` 用 `\u00XX`、非 ASCII 原样输出、孤立代理项
   `\uD800` 形式）。Rust `serde_json` 默认会保留 `1.0` 为 `1.0`、非 ASCII 转义策略不同，因此需要**自定义
   `Formatter`**（或自写 writer）实现 JS 规则。测试用 §7.15 #4 的字节比对兜底。

5. **`URLSearchParams` 表单编码**（§6.2 #4）
   空格 → `+`；`~` → `%7E`（url crate 的 `form_urlencoded` 保留 `~`，必须后处理）；保留
   `*`、`-`、`_`、`.`；其余非字母数字 → `%XX` 大写。`application/x-www-form-urlencoded` 请求体
   的字段顺序必须是插入序（影响部分服务端测试的断言）。同样，URL query（`new URL().searchParams`）
   使用 `%20` 而非 `+`，这是两套不同编码，不能混用。

6. **WHATWG URL 归一化**（§6.2 #2）
   `verification_uri` 校验用 `new URL(...)`：小写协议/主机、去掉默认端口（`:443`/`:80`）、IDN → punycode、
   path 中点段归一化（`.`/`..`）、控制字符百分号转义（ESC → `%1B`，见 §7.7 #11）、`http://` 与 `file://`
   被 `protocol !== "https:"` 拒绝。Rust `url` crate 大体一致，但：`url` 会拒绝某些 WHATWG 接受的输入
   （如未编码空格会成功吗？WHATWG 会编码为 `%20`）；需要一组差分测试覆盖 `"not a url"`、`file:///etc/passwd`、
   含 ESC 的 URL、带默认端口的 URL。

7. **`closeAllConnections()` 与回调服务器生命周期**（§2.8）
   Node `server.closeAllConnections()` 强制断开 keep-alive 连接，使 `close()` 立即完成；Rust/hyper 需主动
   `graceful_shutdown` + 记录所有连接并 abort。回调服务器的状态机（pending → claimed → completed/cancelled）
   必须精确：重复回调 409、state 不匹配 400、POST 404、缺 code 400、`cancel()` 后迟到请求 409、错误路径 404、
   页面 content-type `text/html; charset=utf-8`。端口策略因 flow 而异：Anthropic 首选 53692，被占则回退随机端口；
   OpenAI Codex 首选 1455，被占则**直接失败**并转入手动粘贴回调 URL 的路径；OpenRouter 用 `port: 0`（临时端口）；
   其余 flow 无回调服务器。见 §2.8/§2.10/§2.11/§2.14。

8. **设备码轮询的时间线与 `slow_down`**（§2.7）
   首次轮询时机（默认立即，Copilot `waitBeforeFirstPoll` 先等一个 interval）；初始 `intervalMs =
   max(1000, floor((intervalSeconds ?? 5) * 1000))`；`slow_down` 时若服务端给了 `interval > 0` 且有限，则
   `intervalMs = max(1000, floor(interval * 1000))`，否则 `intervalMs = max(1000, intervalMs + 5000)`；
   睡眠时长为 `min(intervalMs, remainingMs)`；`expires_in` 到点用长/短两版超时文案（Copilot 用长版）；
   403/404 视为 pending（Codex）。所有这些必须在 fake clock 下毫秒级一致。

9. **HTML 页面字节一致**（§4.6）
   `renderOAuthPage` 模板、`escapeHtml` 的转义顺序（`&` 先于 `<`/`>`/`"`/`'`）、`LOGO_SVG` 内联、
   三处 logo 颜色、成功/失败文案与 `detail` 的拼接方式。TUI/浏览器差分测试需按字节比较。

10. **JS 真值与 `trim`**（§6.2 #9/#10）
   `if (value)` 对 `""`、`0`、`NaN`、`null`、`undefined` 为假；`?.trim()` 后空串再判断；`String(value)`
    对 `0` 得 `"0"`（Rust 中 `f64` → `"0"` 而非 `"0.0"`）；`expires_in: 0` 不能当作缺失（xAI 用它触发
    默认 5s 的语义是 `interval ?? 5` 而非 `|| 5`，注意 xAI 对 `interval: 0` 的回退是显式实现，见 §2.18）。
    数字解析：`Number("0x10")`=16、`Number("")`=0、`Number(" 12 ")`=12、`Number("1e3")`=1000。OAuth
    响应字段校验必须用 JS `Number()` 语义而非 serde 的严格数字。

11. **`fetch` 失败与错误消息**（§3.6、§6.2 #3）
    网络错误在 Node 中表现为 `TypeError: fetch failed`（cause 为 `ECONNREFUSED` 等）；`response.text()`
    读取失败、非 JSON 响应、HTTP 状态与 body 的拼接格式（`"<name> failed (HTTP <status>): <body>"`）都必须
    逐字复刻。注意不同 flow 的文案模板不同（§3.6 表），且部分 flow 只取 body 前 N 字符或尝试 JSON 解析
    `error`/`error_description`。

12. **并发语义：刷新与取消**（§2.5）
    双重检查锁内刷新：拿到锁后重读，若已被他人刷新且仍有效则直接用；刷新**不受调用方 signal 影响**，
    但刷新结束后若调用方已 abort 仍要抛 `AbortError`？——TS 的实际顺序是：先 `await` 刷新（独立 signal），
    再把新凭证写回 store（`modify`，此写入即使调用方 abort 也完成），最后 `throwIfAborted(signal)`。Rust
    必须按同一顺序，且 `modify` 队列中「已开始的任务不能被取消、排队未开始的任务可被取消」（coding-agent
    `auth-storage` 测试断言）。

13. **懒加载与循环依赖**（§2.9、§5）
    TS 用动态 `import()` 做 per-provider 懒加载，避免 `models.ts` 静态引入所有 flow。Rust 无动态导入，
    改为显式 `match` + `OnceLock` 缓存；但必须保证「未使用的 flow 不触发网络/配置读取」，且
    `oauth.ts` barrel 不包含 built-in 实现（§7.1 #1）。若用 `inventory` 之类注册机制会破坏该断言。

14. **文本级逐字**：`Login cancelled`、`Device flow timed out`、`xAI device authorization was denied`、
    `Untrusted verification URI`、`Invalid xAI OAuth response field: <field>`、`Missing authorization code`、
    `State mismatch.`、`Authentication successful`、`Example sign-in failed.` 等，均见 §3.6。建议集中到一个
    `messages` 模块并用 `const` 常量，避免拼写漂移。

### 8.5 实现顺序建议

1. `types.rs` + `context.rs` + `helpers.rs` + `utils/abort*.rs`（其余全部依赖）
2. `pkce.rs` + `device_code.rs` + `callback_server.rs`（可独立测试的三个基础件）
3. `oauth_page.rs`（差分字节测试）
4. `resolve.rs` + `credential_store.rs` + `file_store.rs`
5. 九个 flow（按测试文件逐个落地：anthropic → openai-codex → github-copilot → openrouter → kimi → meta →
   xai → radius → openai-chatgpt）
6. `helpers.rs`（`env_api_key_auth` / `lazy_oauth`）+ 与 ai-core/coding-agent 的对接（`AuthStorage`、`/login` TUI）

### 8.6 明确不在本单元但必须对齐

- `Models.getAuth` / `checkProviderAuth` / `login` / `logout` / `applyAuth` 的编排与 `ModelsError` 文案
  （ai-core，§5.3 只给接口契约）。
- `coding-agent` 的 `AuthStorage`（文件锁、`!command` 解析、credential-scoped env）与 `/login` TUI 对话框
  （tui 单元），本单元只提供 `CredentialStore`/`AuthInteraction` 的 trait 与默认实现。
- `compat/extension-oauth-types.ts` 的 7 个类型定义本体（ai-core，§2.1.15）；`src/oauth.ts` / `src/bun-oauth.ts`
  的导出面**属本单元**（§2.19），只是其类型来源文件在 ai-core。

---

## Audit additions（独立审计补充：原规范遗漏或含糊的行为）

本节由独立审计逐文件重读源码后追加。上文已就地修正的错误不重复；下面是**上文完全未提及**或只给了结论、
不足以直接实现/验收的行为。所有断言均来自 `packages/ai/src/auth/**`、`packages/ai/src/{oauth,bun-oauth}.ts` 与
`packages/ai/test/**`、`packages/coding-agent/{src,test}/**` 的实际代码。

### A.1 逐 flow 的 HTTP 超时矩阵（原文只在个别小节零散提到）

本单元**只有部分**请求带超时；把"所有请求都有 30s 超时"当成通则会在差分测试里立刻暴露：

| flow | 请求 | 超时 |
|---|---|---|
| anthropic | `postJson`（exchange + refresh） | `AbortSignal.any([signal, AbortSignal.timeout(30_000)])`（**固定 30s**，无单独常量名） |
| openai-codex | exchange、refresh、device usercode、device token | **无任何超时**，只用调用方 `signal` |
| openai-chatgpt | `requestToken`（exchange + refresh） | **无超时**，只用 `signal` |
| github-copilot | 设备码、轮询、Copilot token、`/models`、`/policy` | 每个请求 `AbortSignal.any([requestSignal, AbortSignal.timeout(5000)])`（**单请求 5s**）；另有可选的整体重试预算 `AbortSignal.timeout(maxElapsedMs)`（登录 `/models` 与 `/policy` 用 5000，refresh 用 0=不设预算） |
| openrouter | exchange | 自建 controller + `setTimeout(30_000)` abort（reason 为 `Error("OpenRouter OAuth token exchange timed out")`）；**回调服务器整体** `timeoutMs: 5 * 60 * 1000` |
| kimi-coding | device authorization、轮询、refresh | `requestSignal` = `any([timeout(30s), signal])` |
| meta | device authorization、轮询、**mint**、refresh（= mint） | 同上 30s（含 mint，见 §2.16 修正） |
| xai | `postForm`（设备码、轮询、refresh） | **无超时** |
| radius | discovery、token、device | **无超时** |

推论：Rust 侧必须能表达"无超时的请求"（`reqwest` 默认无总超时，但**不要**设全局 timeout，否则 xAI/Radius/Codex/ChatGPT
在慢网下会与 TS 行为不同）。所有超时的 reason 都是 `TimeoutError` DOMException（`The operation was aborted due to timeout`），
只有 OpenRouter 的 exchange 超时是普通 `Error`。

### A.2 事件序列（notify 顺序）逐 flow

原文按流程描述了"notify 什么"，但没有给出**事件顺序**这一可观测契约；`openrouter` 甚至在 `auth_url` **之前**
先发 `progress`。Rust 侧按下列顺序逐字复刻（含"不发"的项）：

| flow / 分支 | notify 序列 |
|---|---|
| anthropic browser | `auth_url` → （`waitForCallbackOrManualInput`，内部 prompt）→ 兑换前 `progress("Exchanging authorization code for tokens...")` |
| anthropic copy_code | `auth_url` → prompt → `progress("Exchanging authorization code for tokens...")` |
| openai-codex browser | `auth_url` → prompt；**无 progress** |
| openai-codex device | `device_code`；（轮询）；**无 progress** |
| openai-chatgpt | `auth_url` → （prompt 与回调并行）→ 兑换前 `progress("Exchanging authorization code for tokens...")` |
| github-copilot | `device_code` → （轮询 + Copilot token + `/models`）→ **仅当** `policyModelIds.length > 0` 时 `progress("Enabling models...")` |
| openrouter | `progress("Listening for OpenRouter OAuth callback on <redirectUri>")` → `auth_url` → （paste 分支才）`progress("Exchanging authorization code for an API key...")` |
| kimi-coding | `device_code`；无 progress |
| meta | `device_code` → （轮询）→ `progress("Enabling Meta Model API access...")` → mint |
| xai | `device_code`；无 progress |
| radius browser | `progress("Listening for OAuth callback on http://127.0.0.1:1456/oauth/callback")` → `auth_url`（`Continue in your browser.`） |
| radius device | `device_code`；无 progress |

`info` 事件在本单元**从不**发出（原文 §3.5 已述，此处再次确认：`links` 字段从未被使用）。

### A.3 取消 / 预取消的**真实**行为（与"每个 flow 都用 interaction.signal"的直觉不同）

- `startOAuthCallbackServer` 自身的预取消是**同步 throw**（`"Login cancelled"`，socket 未创建）——已被
  `openrouter-oauth.test.ts` 的 `rejects before opening a callback server when login is already cancelled` 覆盖。
- 但 **anthropic browser** 与 **openai-codex browser** 把它包在 `.catch(() => undefined)` 里，因此**预取消被吞掉**，
  流程继续发出 `auth_url` 并进入粘贴提示（见 §2.10/§2.11 的修正）。openrouter 没有这一层 catch，所以预取消立即 reject。
- **openai-chatgpt** 的 `startCallbackServer(state).catch(...)` 只把 `EADDRINUSE` 转成端口占用文案，**其它错误原样抛**，
  所以预取消会 reject（但注意它是先 `agentHostId(options?.getDeviceId?.())`、再 `generatePKCE()`、再起服务器）。
- **radius browser** 没有 catch：预取消 → `"Login cancelled"`。但它在 `createRadiusOAuth.login` 里先做
  `prompt({type:"select"})`，所以真正到达服务器的预取消只可能发生在 select 返回之后。
- **github-copilot** 先 `prompt({type:"text"})`（**不带 signal**，见 §2.13），然后**显式**
  `if (interaction.signal.aborted) throw new Error("Login cancelled")`。因此"预取消 + 宿主 prompt 立即 reject"
  与"预取消 + 宿主 prompt 挂起"两条路径都会以 `Login cancelled` 收场，但**事件序列不同**（后者不会发 device_code）。
- **meta** 用 `try/catch` 包裹整个登录，`interaction.signal.aborted` 时统一改写成 `Login cancelled`；
  **kimi / xai / openrouter(manual 分支之外) / radius** 没有这层改写，依赖各 fetch 的 `signal`（xai 的 `postForm`
  有 `if (signal.aborted) throw new Error("Login cancelled")`；kimi 没有，只有 device-code 轮询器的 `abortableSleep`）。
- `abortableSleep(ms, signal, cancelMessage)` 的取消错误是**普通 `Error`**（`name === "Error"`），不是 `AbortError`；
  `sleep(ms, signal)`（utils，Kimi refresh 用）则 reject `signal.reason`（通常是 `DOMException`/`AbortError`）。
  两者不可混用。

### A.4 回调服务器的其它可观测细节

- 通用服务器 `sendPage` 写的是**小写** `content-type` 与 `cache-control: no-store`；ChatGPT 专用服务器写**大写** `Content-Type`
  且**没有** `cache-control`。Node 实际响应头还会带 `Date`、`Connection: keep-alive`、`Keep-Alive: timeout=5`、
  `Transfer-Encoding: chunked`（实测；测试只断言状态码与 content-type，见 §6.2 #20）。
- `redirectUri` 的判定只看 `redirectHost.includes(":")`（不是 IPv6 解析）：`KIANA_OAUTH_CALLBACK_HOST="::1"` 会得到
  `http://[::1]:<port>/path`；`"localhost"` 不含 `:`，不加括号。
- `waitPromise.catch(() => undefined)` 是**必须**的：`close()`/超时/abort 都可能让 `wait()` 永不被调用者 await，
  少了这行 Node 会报 unhandledRejection。
- `cancel()` 之后 `wait()` **resolve `undefined`**（不是 reject）；只有 `close()` 才以 `"OAuth callback server closed"` reject。
- 端口绑定失败时 reject 的是**原始 Node 错误对象**（`code: "EADDRINUSE"`），不是包装错误；Rust 侧若定义
  `AuthError::AddrInUse` 也必须能区分"端口占用"与其它 IO 错误（chatgpt 的文案改写依赖它）。
- 状态机顺序不可换：**state 检查在 `claimed || settled` 检查之前**，`error` 参数检查在 `claimed` 之后、`code` 之前。
  因此"重复回调 + state 错"返回 400（而非 409），"已完成 + 带 error 参数"返回 409（而非 400）。

### A.5 各 flow 的响应解析容错差异（影响错误文案与"是否继续"）

- **anthropic**：完全不校验响应字段。`expires_in` 缺失 → `expires = NaN`（`Date.now() + undefined*1000 - 300000`），
  `refresh_token`/`access_token` 缺失 → 凭证里对应字段为 `undefined`（`JSON.stringify` 时被省略）。Rust 侧若要
  字节级对齐 `auth.json`，必须允许 `access`/`refresh` 缺失（但 §4.1 的 `ReadOnlyAuthStorage` 校验会拒绝这种文件，
  这是上游的已知不一致，不要"顺手修掉"）。
- **openai-codex**：`readTokenResponse` 的 `response.json()` **无 catch**（原始 `SyntaxError` 冒泡）；
  `startOpenAICodexDeviceAuth` 与 `pollOpenAICodexDeviceAuth` 里的 `await response.json()` 同样**无 catch**
  （只有 `!response.ok` 分支读 text 时有 `.catch(() => "")`）。因此"非 JSON 响应"在 Codex 上是**未包装异常**，
  而在 anthropic 上会被包装成 `Token exchange returned invalid JSON...`。
- **openai-chatgpt**：`requestToken` 的 `await response.json()` **无 catch**（非 JSON → 原始 `SyntaxError`）；
  但 `typeof data !== "object" || null || Array.isArray` → `OpenAI OAuth token response must be an object`。
- **github-copilot**：`fetchJson` 的 `response.json()` 无 catch；`fetchGitHubCopilotModels` 的
  `parseGitHubCopilotModelCatalog(await response.json(), ...)` 也无 catch。
- **kimi / meta**：统一用 `readJson`（catch → `null`），因此非 JSON 响应**不会**抛解析错，而是走到
  `... status ...${errorDetail(json)==""}` 或 `Invalid ... response: null`（注意 `JSON.stringify(null)` = `"null"`）。
- **xai**：`postForm` 用 try/catch 包住 `response.json()`，非对象 → `{}`；解析失败且 `signal.aborted` → `Login cancelled`，
  否则 `xAI OAuth returned invalid JSON (HTTP <status>)`。
- **radius**：`requestOAuthToken` 的 `await response.json()` **无 catch**；`loadRadiusOAuthDiscovery` 与
  `requestDeviceAuthorization` 同样无 catch。`readOAuthResponseError` 自己 `text().catch(() => "")`。

### A.6 `Number.parseFloat` / `Date.parse` 的 Retry-After 宽容性（Copilot）

`fetchWithRateLimitRetry` 对 `retry-after` 头先 `Number.parseFloat(...)`：`"1.5"` → 1500ms、`"1abc"` → 1000ms、
`"  2 "` → 2000ms（parseFloat 会跳过前导空白并接受前缀数字）；`Number.isNaN` 时才退回 `Date.parse(retryAfter) - Date.now()`
（HTTP-date，如 `"Wed, 21 Oct 2015 07:28:00 GMT"`）。`Date.parse("5")` 会被当作**年份 2005**（→ 大负数 → `Math.max(0, …) = 0`
→ 立即重试）。Rust 侧必须自实现"JS parseFloat 前缀解析"而不是 `str::parse::<f64>`（后者对 `"1abc"` 失败）。
另外：`delayMs >= retryDeadline - Date.now()` 时**放弃重试并原样返回 429 响应**（不是抛错），
`response.body?.cancel()` 必须在 `sleep` 之前调用。

### A.7 测试清单补遗（原文 §7 完全未列出的测试文件）

| 文件 | 用例数 | 与本单元的关系 |
|---|---|---|
| `packages/ai/test/anthropic-auth-token.test.ts` | 10 | 直接约束 `providers/anthropic.ts` 的 `anthropicApiKeyAuth()`（见 §2.3 修正）：`ANTHROPIC_AUTH_TOKEN` → `{headers:{Authorization:"Bearer …"}}` 且**不带** OAuth beta；`ANTHROPIC_OAUTH_TOKEN` → `{apiKey}` 且 SDK 收到 `authToken` + `betas` 含 `oauth-2025-04-20`；显式 headers 覆盖。用例名：`resolves ANTHROPIC_AUTH_TOKEN as a bearer Authorization header`、`preserves ANTHROPIC_OAUTH_TOKEN as OAuth-shaped API auth`、`uses Authorization headers without OAuth-mode request shaping`、`threads authContext ANTHROPIC_AUTH_TOKEN through request headers`、`preserves OAuth request shaping for ANTHROPIC_OAUTH_TOKEN`、`lets explicit request headers override ANTHROPIC_AUTH_TOKEN`、`uses kiana's User-Agent by default for Anthropic Messages requests`、`lets explicit headers override the default Anthropic Messages User-Agent`、`preserves explicit Anthropic beta header replacement`、`preserves explicit Anthropic beta header suppression`。 |
| `packages/ai/test/anthropic-federation.test.ts` | 9 | `anthropicApiKeyAuth().resolve` 的 workload identity federation 分支：`resolves the federation variables as provider env with no request auth`、`passes ANTHROPIC_WORKSPACE_ID through when set`、`is not configured when a federation variable is missing`、`treats ANTHROPIC_SERVICE_ACCOUNT_ID as optional, like the SDK`、`keeps API key and auth token precedence over federation`、`hands the SDK a federation config instead of a key`、`threads authContext federation variables through Models`、`lets an explicit API key win over federation env`、`does not federate other anthropic-messages providers`（最后一条：`kimi-coding` 等同样走 `anthropic-messages` API 的 provider **不得**触发 federation）。 |
| `packages/ai/test/anthropic-federation-sdk.test.ts` | 2 | 用**真实** `@anthropic-ai/sdk` 对 fake fetch：`exchanges the identity token once across requests`（同一 client 复用只换一次）、`does not run the SDK credential chain for header-owned auth`（已有 `Authorization` header 时不走 SDK 凭证链——对应 `ANTHROPIC_AUTH_TOKEN` 分支的 `{headers:{Authorization}}`）。 |
| `packages/ai/test/radius-provider.test.ts` | 5 | `radiusProvider()` 的模型目录：`ships a static public catalog for the default gateway`、`does not apply the public Radius catalog to custom gateways`、`replaces the static public catalog with refreshed models`、`replaces the static public catalog with a cached catalog without network access`、`exposes no models when the organization disabled all of them`。约束本单元 `loadRadiusOAuth({name, gateway})` 的调用方契约。 |
| `packages/ai/test/abort.test.ts` | 39 个 `it`，分属 19 个 `describe.skipIf(!<某 env>)` 块（无密钥时 **0 个真跑**；顶层 `describe("AI Providers Abort Tests")` 不 skip，但内部每块都 skip） | 端到端 `stream(...,{signal})` 中断；模块顶层 `await Promise.all([resolveApiKey("openai-codex")])` 即**依赖 `test/oauth.ts` 的 auth.json 读取**。对 Rust 侧只需保证这些用例默认 skip 的语义（不应因缺凭证而失败）。 |
| `packages/coding-agent/test/radius.test.ts` | 6 | `restores the legacy credential catalog without network access`、`fetches and stores the catalog for configured Radius auth`、`does not refresh catalogs over the network by default`、`does not fetch or make Radius models available without configured auth`、`supports custom Radius gateways from models.json`、`requires baseUrl for custom Radius gateways`。约束 `credential.gatewayConfig` 的"旧版兼容"读取路径（§4.1 表格中 `gatewayConfig` 一行）。 |
| `packages/coding-agent/test/mcp-oauth-store.test.ts` / `mcp-oauth-refresh.test.ts` | 3 / 11 | 复用 `InMemoryAuthStorageBackend` 作为通用 KV 后端（`withLock`/`withLockAsync` 的 LockResult 契约），与 auth.json 无直接关系，但修改后端签名会破坏它们。 |

`packages/ai/test/oauth.ts`（helper，非测试）的精确行为（原文只写了一句）：
- `AUTH_PATH = join(homedir(), ".kiana", "agent", "auth.json")`（**硬编码 `~/.kiana`，不读 `KIANA_CODING_AGENT_DIR`**）；
- 模块级副作用：`const [openaiCodexToken] = await Promise.all([...])` 在**部分测试文件的顶层**执行（`abort.test.ts` 等），
  意味着**导入即读真实用户 auth.json**；文件不存在 / JSON 损坏 → `{}`；
- `resolveApiKey(provider)`：`api_key` → 直接返回 `entry.key`（**不**解析 `!command`/`$VAR`）；`oauth` →
  用 `builtinProviders()`（**会实例化全部 provider 工厂**）找 `auth.oauth`，`Date.now() >= credential.expires` 时
  `oauth.refresh(credential, new AbortController().signal)`，失败则 `console.log(JSON.stringify(error))` 并返回 `undefined`
  （注意 `JSON.stringify(new Error("x"))` = `"{}"`）；成功后**无论是否刷新都**把 credential 写回（`JSON.stringify(storage, null, 2)`，
  `writeFileSync` 无 mode，随后 `chmodSync(AUTH_PATH, 0o600)`；目录不存在时 `mkdirSync(recursive, 0o700)`）；
  最终 `(await oauth.toAuth(credential)).apiKey`。

### A.8 `packages/ai/src/cli.ts`（124 行，`bin: kiana-ai`）——原文完全未覆盖

`package.json` 里 `"bin": { "kiana-ai": "dist/cli.js" }`，该脚本是**开发用** OAuth 登录器，直接依赖本单元的 `OAuthAuth`：

- 只列出 `builtinProviders()` 中 `auth.oauth !== undefined` 的 provider（共 **9** 个：`anthropic`、`github-copilot`、
  `kimi-coding`、`meta`、`openai-codex`、`openai`、`openrouter`、`radius`、`xai`——注意 `openai` 的 OAuth 是 ChatGPT 流程，
  `openai-codex` 是 legacy 流程，两者算两个 provider）。
- 命令：无参/`help`/`--help`/`-h` → 打印 `Usage: npx @earendil-works/kiana-ai <command> [provider]` 与 provider 列表；
  `list` → 每行 `${id.padEnd(20)} ${name}`；`login [provider]`（缺省时用数字菜单选择）；
  其它 → `Unknown command: ${command}`；未知 provider → `Unknown provider: ${providerId ?? ""}`。
- `answerPrompt`：`select` 打印 `\n${message}` + `  N. ${label}`，然后 `Enter number (1-${n}): `，非法 → `Invalid selection`；
  其它 prompt 打印 `${message}${placeholder ? ` (${placeholder})` : ""}: `（**注意末尾冒号与括号**）。
- `notify` 渲染：`auth_url` → `\nOpen this URL in your browser:\n${url}` + 可选 instructions；
  `device_code` → `\nOpen this URL in your browser:\n${verificationUri}` + `Enter code: ${userCode}`；
  `info`/`progress` → `console.log(event.message)`（`info` 忽略 `links`）。
- `login(...)` 的第二参固定 `{ getDeviceId: randomUUID }`（ChatGPT 因此可用）；凭证写入 **CWD 的 `auth.json`**
  （`JSON.stringify(auth, null, 2)`，无 chmod、无锁、无 BOM 处理），然后打印 `\nCredentials saved to auth.json`。
- 顶层 `main().catch(...)` → `console.error("Error:", error.message)` + `process.exit(1)`。
- 该 CLI **不**读 `KIANA_CODING_AGENT_DIR`、**不**用 `CredentialStore`、**不**做 refresh。
  Rust 若要 1:1 提供 `kiana-ai` bin，需单独实现；若决定不提供，应在 §8.6 显式标注为"不移植"。

### A.9 `provider-composer.ts` 的 `adaptOAuth` ——本单元类型的第一个外部消费者（原文未提）

`packages/coding-agent/src/core/provider-composer.ts:358` 把**旧版扩展**的 `ExtensionOAuthConfig` 适配成本单元的 `OAuthAuth`，
这是 `AuthPrompt`/`AuthEvent`/`OAuthAuth` 形状的**真实验收点**：

```ts
function adaptOAuth(config: ExtensionOAuthConfig): OAuthAuth {
	return {
		name: config.name,
		isSubscription: config.isSubscription,
		login: async (callbacks) => {
			const credential = await config.login({
				onAuth: (info) => callbacks.notify({ type: "auth_url", ...info }),
				onDeviceCode: (info) => callbacks.notify({ type: "device_code", ...info }),
				onPrompt: (prompt) => callbacks.prompt({ type: "text", ...prompt }),
				onProgress: (message) => callbacks.notify({ type: "progress", message }),
				onManualCodeInput: () => callbacks.prompt({ type: "manual_code", message: "Paste the authorization code" }),
				onSelect: (prompt) => callbacks.prompt({ type: "select", ...prompt }),
				signal: callbacks.signal,
			});
			return { ...credential, type: "oauth" };
		},
		refresh: async (credential, signal) => ({ ...(await config.refreshToken(credential, signal)), type: "oauth" }),
		toAuth: async (credential) => ({ apiKey: config.getApiKey(credential) }),
	};
}
```

要点：`login` 的形参只用了 `callbacks`（`ProviderAuthInteraction`），**忽略** `LoginOptions`；
`onManualCodeInput` 的 message 逐字 `Paste the authorization code`（**无**冒号、**无** placeholder）；
`{...credential, type: "oauth"}` 保留所有额外字段。`ExtensionOAuthConfig.usesCallbackServer` 标注 `@deprecated`、
"retained for extension source compatibility; ignored by canonical auth flows"。
Rust 侧若实现 `OAuthAuth` 的 FFI/兼容层，需保持同样的字段映射（尤其 `auth_url` 只能带 `url`/`instructions`，
`device_code` 只能带 5 个字段——多传字段会在 TS 侧被忽略、在 Rust 侧可能编译不过）。

### A.10 coding-agent CLI 的 `--min-expiry` 解析（原文 §3.8 只给了常量）

- `DEFAULT_BEARER_TOKEN_MIN_EXPIRY_MS = 30 * 60_000`（`cli/credential-print.ts:7`），仅在 `kind === "bearer_token"`
  且用户未给 `--min-expiry` 时作为 `minOAuthValidityMs` 传给 `getAuth`（`{...(kind === "bearer_token" ? { minOAuthValidityMs: minExpiryMs ?? DEFAULT } : {}), signal}`）。
- `--min-expiry` 只允许用于 `print-bearer-token`（否则 `--min-expiry is only supported by print-bearer-token`）；
  值必须匹配 `/^(\d+)(ms|s|m|h)$/iu`，否则 `--min-expiry must use a duration such as 30m or 1h`；
  换算 `ms`→1、`s`→1000、`m`→60_000、`h`→3_600_000。
- `print-api-key` 跳过 oauth 凭证；`print-bearer-token` 只认 oauth 凭证；两者在"类型不符"时分别抛
  `Provider "<id>" is configured with OAuth, not an API key` / `Provider "<id>" is not configured with an OAuth bearer token`；
  多匹配 → `Multiple configured providers matched (<a>, <b>). Specify --provider.`；无可用 → `No usable API key is configured` /
  `No usable OAuth bearer token is configured`。
- `bug-report.ts:183` 与 `session-share.ts:110` 也显式传 `minOAuthValidityMs: 5 * 60_000`（等于默认值，属冗余但可观测）。
- `parseAuthCommand` 的用法串逐字：`${APP_NAME} auth print-bearer-token --provider <provider> [--model <model>] [--min-expiry <duration>]` 等。

### A.11 `AuthStorage` 与 `InMemoryAuthStorageBackend` 的 LockResult 契约（宿主侧，影响本单元 trait 形状）

- `AuthStorageBackend.withLock<T>(fn: (current: string | undefined) => LockResult<T>): T` 与
  `withLockAsync<T>(fn: (current: string | undefined) => Promise<LockResult<T>>, options?): Promise<T>`，
  其中 `LockResult<T> = { result: T; next?: string }`——**`next` 是"要写回文件的字符串"**，不是凭证对象。
  Rust 侧若把 `CredentialStore` 直接实现为文件存储，可以不要这一层；但要保留"`next === undefined` 不写文件"的语义。
- `InMemoryAuthStorageBackend`：`withLockAsync` 用 promise 链串行；`operation` 内部 `throwIfAborted()` 两次；
  `raceWithAbortSignal(operation, options?.signal)` → **取消只影响等待者，不影响已排队的执行**；
  `asyncChain = operation.catch(() => {})`（失败不阻塞后续）。
- `AuthStorage.inMemory(data)` = `fromStorage(new InMemoryAuthStorageBackend())` 并先 `withLock(() => ({result: undefined, next: JSON.stringify(data, null, 2)}))`。
- `FileAuthStorageBackend` 构造时 `normalizePath(authPath)`；`authPath` 为 undefined（纯内存）时 `readLatestData`
  走 `reloadFromStorageAsync` 且**无 signal 时吞错回落缓存**。

### A.12 `AuthEvent` / `AuthPrompt` 的运行时形状（影响序列化与 TUI 绑定）

- `AuthPrompt` 是**交叉类型** `{ signal?: AbortSignal } & (union)`：即四种变体都可带 `signal`；`select` 的 options
  元素是 `{ id, label, description? }`（**`description` 可选，本单元从未使用**）。
- `AuthEvent.device_code` 的 `openBrowserOnConfirm` / `copyCodeOnConfirm` 注释逐字：
  "Offer best-effort browser opening on user confirmation. Defaults to true; clients may ignore it." /
  "Offer copying of userCode on user confirmation, replacing the clipboard. Defaults to true."
- `AuthInteraction.signal` 是**可选**（`signal?: AbortSignal`），`ProviderAuthInteraction` 才把它变为必需；
  所有 flow 的 `login` 形参都是 `ProviderAuthInteraction`（因此内部可无 `!`）。
- `notify(event)` 返回 `void` 且**同步**；`prompt(prompt)` 返回 `Promise<string>`（`select` 返回 option **id**）。
  本单元没有任何地方 await `notify`，也没有任何地方捕获其异常。

### A.13 Rust 草图与映射表的其余修正

- §8.2 的 `OAuthAuth` trait 多了 `fn id(&self) -> &str` 与 `fn login_label(&self) -> &str`（返回非 Option 的 `&str`）：
  TS 里**没有** `id` 字段，`loginLabel` 是 `string | undefined`。Rust 侧应为 `fn login_label(&self) -> Option<&str>`
  并删除 `id`（provider id 由 provider 自己持有；`load*OAuth` 也不需要 id）。`is_subscription` 返回 `bool` 与 TS 的
  `boolean | undefined` 在语义上等价（`undefined` 与 `false` 都表示"不是订阅"），可保留。
- §8.2 的 `LoginContext` 把 `ProviderAuthInteraction` 与 `LoginOptions` 合并；注意 TS 的 `login(interaction, options?)`
  第二参是**可选**的，且只有 `openai-codex`（`agentName`→originator）与 `openai-chatgpt`（`getDeviceId`/`agentName`）
  读它，其余 7 个 flow 完全忽略。
- §6.1 映射表 `atob`/`btoa` 一行写"`base64` crate（`STANDARD`/`URL_SAFE_NO_PAD`）"：`btoa` 只用于 PKCE 的
  `base64urlEncode`（先 `STANDARD` 再手工换成 `-`/`_` 并去 `=`），`atob` 必须按 §8.4 #1 的**新**规则手写校验；
  直接 `URL_SAFE_NO_PAD` 解码 JWT 会**接受** TS 会拒绝的 base64url payload，从而改变 `Failed to extract accountId` 的行为。
- §6.1 的 `structuredClone` 一行：本单元**不**调用 `structuredClone`（它只在 coding-agent 的 `ReadOnlyAuthStorage` 里）。
- §8.1 的模块清单缺 `credentials.rs`（`readStoredCredential` 的等价物）与 `dev_cli.rs`（若移植 `bin/kiana-ai`）；
  `oauth/mod.rs` 的 `OAUTH_PROVIDERS: &[&dyn OAuthAuth]` 是**错的**形状：本单元没有任何全局 provider 数组，
  只有 9 个 `load*OAuth` 函数 + 1 个可复写的 `bundledLoaders`。

### A.14 环境变量的完整清单（§4.2 之外）

本单元**直接**读取的环境变量只有三个：`KIANA_OAUTH_CALLBACK_HOST`、`KIMI_CODE_OAUTH_HOST`、`KIMI_OAUTH_HOST`
（均经 `getProviderEnvValue`，即 `env?.[name] || process.env[name] || BunSandbox(/proc/self/environ) || undefined`）。
`envApiKeyAuth` 的 `*_API_KEY` 不是本单元读的，而是 ai-core 的 `ctx.env(name)`；
`defaultProviderAuthContext.env` 对**纯空白**返回 `undefined`、对含空白的值返回**原串**（§2.2 已述）。
`getProviderEnvValue` 的 Bun 回退只在 `process.versions.bun` **且** `Object.keys(process.env).length === 0` 时触发，
且 `readFileSync("/proc/self/environ")` 的解析规则是 `split("\0")` + `indexOf("=") > 0` 才收录（值里可含 `=`）。
`procEnvCache` 是模块级 `Map`，**首次**调用时填充，之后**不再重读**（进程内环境变化不可见）。

### A.15 未在 §7 列出但会约束本单元的断言（补遗）

- `oauth-auth.test.ts` 的 `keeps the extension OAuth barrel free of built-in flow implementations` 用
  `import * as extensionOAuthCompatibility from "../src/oauth.ts"`，断言 `not.toHaveProperty("loginAnthropic")` 与
  `not.toHaveProperty("anthropicOAuth")`——注意 `oauth.ts` 是 **type-only**，Vite/vitest 下 `import *` 得到
  `{}`；Rust 侧对应"`oauth` 模块不含任何值导出"的编译期断言。
- `oauth-auth.test.ts` 的 `identifies only subscription-backed OAuth flows as subscriptions` **没有**包含
  `metaOAuth`（虽然它也是 `isSubscription: true`）；Rust 测试若照抄该列表，不应"补上" meta 后断言数量一致。
- `openai-codex-oauth.test.ts` 的 `createAccessToken` 用 `Buffer.toString("base64")`（**带 `=` 填充的标准 base64**），
  因此 `decodeJwt` 的 `atob` 能解；Rust 测试 fixture 必须生成**同样**的带填充 base64，否则会掩盖 §8.4 #1 的差异。
- `oauth-callback-server.test.ts` 里 `redirectUri` 断言 `/^http:\/\/127\.0\.0\.1:\d+\/callback$/`，且
  `start()` 默认 `state: "expected-state"`、`complete: async (code) => \`completed:${code}\``；
  `waitForCallbackOrManualInput` 的 `pendingPrompt` 依赖 `prompt.signal` 被 abort 才 reject（对应 `finally` 里的
  `manualAbort.abort()`）。
- `github-copilot-oauth.test.ts` 用 `testCopilotAccessToken = "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;"`
  与 `testCopilotModelsUrl = "https://api.individual.githubcopilot.com/models"`；`github-copilot-oauth.test.ts` 共 743 行。
- `xai-oauth.test.ts` 的 `it.each` 展开后：`rejects a non-https verification URI: %s`（3 例）与
  `fails when device authorization is denied: %s`（2 例），它们由 **`postForm` 的 `Accept` 头大小写**等细节间接约束。
