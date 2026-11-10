# 模型与 provider

本页讲什么：kiana 内置哪些 provider、各自认哪些环境变量，`--provider` / `--model` / `--list-models`
怎样选模型，交互模式里的 `/model`、`/scoped-models`、`/thinking` 怎样切换模型与思考等级，以及
`kiana auth` 三个子命令和 `/login`、`/logout` 的当前行为。

> 版本与平台口径见 [快速开始](getting-started.md#版本说明)：workspace 包版本 `0.1.0`，
> `kiana --version` 输出 `1.4.0`（TS 兼容常量）；当前只在 Linux x86_64 上验证。

## 快速开始

```bash
# 用 provider + 模型 id
kiana --provider openai --model gpt-4o -p "解释这段代码"

# 直接用 provider/id（不需要 --provider）
kiana --model anthropic/claude-sonnet-4-5

# 模糊匹配 + 指定 thinking 等级
kiana --model sonnet:high

# 列出当前认证就绪的模型（支持模糊搜索）
kiana --list-models
kiana --list-models sonnet

# 检查认证
kiana auth check --provider openai --json

# 打印真实请求会使用的 API key
kiana auth print-api-key --provider openai
```

交互模式里：

```text
/model                        打开模型选择器
/model anthropic/claude-sonnet-4-5
/scoped-models                圈定 Ctrl+P 循环的模型
/thinking high                设置当前会话思考等级
/login openai                 登录 provider
/logout                       删除已保存的凭据
```

完整 CLI 选项见 [CLI 参考](cli.md)；交互命令全集见 [交互模式](interactive-mode.md)。

## 认证：key 从哪里来

三种来源，按优先级从高到低：

1. **显式 `--api-key`**：只用于本次进程，写入运行时，不落盘；也不参与“多个 provider 同名模型”
   的裁决（错误信息会写 `--api-key does not choose a provider.`）。纯 OAuth provider
   （如 `openai-codex`）会忽略它。
2. **已保存凭据**（`<agentDir>/auth.json`，默认 `~/.kiana/agent/auth.json`，受
   `KIANA_CODING_AGENT_DIR` 影响）：由 `/login` 写入。
3. **环境变量 / 云凭据链**：provider 自己的 API key 变量、AWS 凭据链、Google ADC 等。

关键规则：

- **存储凭据“拥有”该 provider**：存在已保存凭据时，不再回退到环境变量。存储的是 OAuth 但
  provider 没有对应 OAuth 处理器，或 OAuth 刷新失败，结果是“没有可用认证”，不会静默改用
  env key。
- 只有什么都没保存时，才对有 API-key 处理器的 provider 读取环境变量/ambient 凭据。
- 环境变量空串等同于未设置。
- Anthropic 的 `ANTHROPIC_AUTH_TOKEN` 作为 `Authorization: Bearer` 发送，不按普通 API key
  处理；请求 key 的选择会跳过它。Anthropic 的发现顺序是
  `ANTHROPIC_AUTH_TOKEN` → `ANTHROPIC_OAUTH_TOKEN` → `ANTHROPIC_API_KEY`。
- `--api-key` 要求先选出模型；没有 `--model` / `--provider --model` / `--models` 时会报
  `--api-key requires a model to be specified via --model, --provider/--model, or --models`。

## 支持的 provider

内置 provider 共 42 个（顺序固定为 `spec/ai-core.md` §2.3.1）。下表的环境变量来自
`kiana --help` 的 Environment Variables 段；带 `†` 的变量**不在 `--help` 清单里**，但已在
provider 源码中核对。`--help` 清单本身可以在 `kiana --help` 中复查。

| provider id | 显示名 | 认证 | 环境变量 |
| --- | --- | --- | --- |
| `amazon-bedrock` | Amazon Bedrock | AWS 凭据链 / bearer token | `AWS_PROFILE`、`AWS_ACCESS_KEY_ID`、`AWS_SECRET_ACCESS_KEY`、`AWS_BEARER_TOKEN_BEDROCK`、`AWS_REGION`；另见下方 ambient 变量 |
| `ant-ling` | Ant Ling | API key | `ANT_LING_API_KEY` |
| `anthropic` | Anthropic | API key + OAuth | `ANTHROPIC_API_KEY`、`ANTHROPIC_OAUTH_TOKEN`、`ANTHROPIC_AUTH_TOKEN` |
| `azure` | Azure | API key | `AZURE_OPENAI_API_KEY`；配合 `AZURE_OPENAI_BASE_URL` 或 `AZURE_OPENAI_RESOURCE_NAME`，可选 `AZURE_OPENAI_API_VERSION`、`AZURE_OPENAI_DEPLOYMENT_NAME_MAP` |
| `baseten` | Baseten | API key | `BASETEN_API_KEY` |
| `cerebras` | Cerebras | API key | `CEREBRAS_API_KEY` |
| `cloudflare-ai-gateway` | Cloudflare AI Gateway | API key | `CLOUDFLARE_API_KEY` + `CLOUDFLARE_ACCOUNT_ID` + `CLOUDFLARE_GATEWAY_ID` |
| `cloudflare-workers-ai` | Cloudflare Workers AI | API key | `CLOUDFLARE_API_KEY` + `CLOUDFLARE_ACCOUNT_ID` |
| `deepseek` | DeepSeek | API key | `DEEPSEEK_API_KEY` |
| `fireworks` | Fireworks | API key | `FIREWORKS_API_KEY` |
| `github-copilot` | GitHub Copilot | API key + OAuth | `COPILOT_GITHUB_TOKEN` † |
| `google` | Google | API key | `GEMINI_API_KEY` |
| `google-vertex` | Google Vertex AI | API key 或 ADC | `GOOGLE_CLOUD_API_KEY` †，或 `GOOGLE_APPLICATION_CREDENTIALS` † + `GOOGLE_CLOUD_PROJECT` †/`GCLOUD_PROJECT` † + `GOOGLE_CLOUD_LOCATION` † |
| `groq` | Groq | API key | `GROQ_API_KEY` |
| `huggingface` | Hugging Face | API key | `HF_TOKEN` † |
| `kimi-coding` | Kimi For Coding | API key + OAuth | `KIMI_API_KEY` |
| `meta` | Meta | API key + OAuth | `META_API_KEY` |
| `minimax` | MiniMax | API key | `MINIMAX_API_KEY` |
| `minimax-cn` | MiniMax CN | API key | `MINIMAX_CN_API_KEY` † |
| `mistral` | Mistral | API key | `MISTRAL_API_KEY` |
| `moonshotai` | Moonshot AI | API key | `MOONSHOT_API_KEY` |
| `moonshotai-cn` | Moonshot AI CN | API key | `MOONSHOT_API_KEY` |
| `nvidia` | NVIDIA | API key | `NVIDIA_API_KEY` |
| `openai` | OpenAI | API key + OAuth | `OPENAI_API_KEY` |
| `openai-codex` | OpenAI Codex (legacy) | 仅 OAuth | —（没有 API key 变量） |
| `opencode` | OpenCode Zen | API key | `OPENCODE_API_KEY` |
| `opencode-go` | OpenCode Go | API key | `OPENCODE_API_KEY` |
| `openrouter` | OpenRouter | API key + OAuth | `OPENROUTER_API_KEY` |
| `qwen-token-plan` | Qwen Token Plan | API key | `QWEN_TOKEN_PLAN_API_KEY` |
| `qwen-token-plan-cn` | Qwen Token Plan CN | API key | `QWEN_TOKEN_PLAN_CN_API_KEY` |
| `qwen-token-plan-individual` | Qwen Token Plan Individual | API key | `QWEN_TOKEN_PLAN_API_KEY` |
| `radius` | Radius | API key + OAuth | `RADIUS_API_KEY` † |
| `together` | Together | API key | `TOGETHER_API_KEY` |
| `typesafe` | TypeSafe | API key | `TYPESAFE_API_KEY` † |
| `vercel-ai-gateway` | Vercel AI Gateway | API key | `AI_GATEWAY_API_KEY` |
| `xai` | xAI | API key + OAuth | `XAI_API_KEY` |
| `xiaomi` | Xiaomi | API key | `XIAOMI_API_KEY` |
| `xiaomi-token-plan-ams` | Xiaomi Token Plan AMS | API key | `XIAOMI_TOKEN_PLAN_AMS_API_KEY` |
| `xiaomi-token-plan-cn` | Xiaomi Token Plan CN | API key | `XIAOMI_TOKEN_PLAN_CN_API_KEY` |
| `xiaomi-token-plan-sgp` | Xiaomi Token Plan SGP | API key | `XIAOMI_TOKEN_PLAN_SGP_API_KEY` |
| `zai` | Z.AI | API key | `ZAI_API_KEY` |
| `zai-coding-cn` | Z.AI Coding CN | API key | `ZAI_CODING_CN_API_KEY` |

补充变量（同样未在 `--help` 列出，均已在代码中核对）：

| 变量 | 用途 |
| --- | --- |
| `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`、`AWS_CONTAINER_CREDENTIALS_FULL_URI`、`AWS_WEB_IDENTITY_TOKEN_FILE` | Amazon Bedrock ambient 凭据链 |
| `ANTHROPIC_FEDERATION_RULE_ID`、`ANTHROPIC_ORGANIZATION_ID`、`ANTHROPIC_SERVICE_ACCOUNT_ID`、`ANTHROPIC_IDENTITY_TOKEN_FILE`、`ANTHROPIC_WORKSPACE_ID` | Anthropic workload identity federation；`FEDERATION_RULE_ID`、`ORGANIZATION_ID`、`IDENTITY_TOKEN_FILE` 必须同时非空，`SERVICE_ACCOUNT_ID` / `WORKSPACE_ID` 可选 |

### OAuth 登录支持的 provider

| provider id | OAuth 名称/入口 | 备注 |
| --- | --- | --- |
| `anthropic` | Anthropic (Claude Pro/Max) | 订阅制 |
| `github-copilot` | GitHub Copilot | 订阅制 |
| `kimi-coding` | Kimi Code (subscription) | 订阅制 |
| `meta` | Meta (Muse subscription) | 订阅制 |
| `openai` | OpenAI (ChatGPT subscription) | 订阅制 |
| `openai-codex` | OpenAI (ChatGPT Plus/Pro) | 仅 OAuth，没有 API key 入口 |
| `openrouter` | OpenRouter OAuth | — |
| `radius` | Radius | OAuth access token 作为网关 bearer key |
| `xai` | xAI (Grok/X subscription) | 订阅制 |

其余 provider 只支持 API key（含 AWS/ADC 这类 ambient 凭据）。

## 用 CLI 选择模型

### `--provider` 与 `--model`

| 写法 | 含义 |
| --- | --- |
| `--model gpt-4o` | 裸模型 id；大小写不敏感的精确匹配优先，失败后再模糊匹配 |
| `--model openai/gpt-4o` | `provider/id`；斜杠前缀只是提示，仍会尝试完整文本 |
| `--model sonnet:high` | 在模型引用后加 `:<thinking>` |
| `--provider openai --model gpt-4o` | 把候选限制在 provider 内 |
| `--model openai/自定义模型` | provider 存在但模型 id 不在目录时，按“自定义模型 id”回退 |

规则：

- `--provider` **必须**和 `--model` 一起用（这里指主命令的模型选择；`kiana auth` 子命令的
  `--provider` 可以单独使用）；单独给出 `--provider` 会报
  `--provider requires --model (for example: --provider <provider> --model <pattern>)`。
- provider 名按小写精确匹配；未知 provider 报
  `Unknown provider "<name>". Use --list-models to see available providers/models.`
- `--model` 的解析对象是**全部**模型，不只已认证模型，因此可以用 `--api-key` 首次配置某个
  provider；但真正发起请求仍需要凭据。
- 精确匹配是模型 id（大小写不敏感）。模糊匹配同时看模型 id 和显示名；`-latest`/非日期别名
  优先于带日期版本。
- 完整文本能精确命中某个模型时，不会剥离 `:thinking` 后缀；因此 id 本身含冒号的模型
  （例如某些 OpenRouter 模型）可以正常选择。只有整段不命中时，才把合法的
  `:<thinking>` 后缀当作思考等级。
- 合法 thinking 后缀：`off`、`minimal`、`low`、`medium`、`high`、`xhigh`、`max`。
  非法后缀不会被剥离，会作为模型 id 的一部分继续解析；有 `--provider` 或 `provider/`
  锚点时可能因此回退成自定义模型 id。

### 歧义与自定义模型回退

同一个裸模型 id 出现在多个 provider 时，解析器用认证状态裁决：

- 命中的候选全在同一个 provider：直接选中，不看认证。
- `provider/id` 指定的 provider 已认证：选中它。
- `provider/id` 指定的 provider 未认证，但其余候选恰好一个已认证：选中那个已认证候选。
- `provider/id` 指定的 provider 未认证，且其余候选都没有可用认证：仍选中 named provider
  （这样 `--api-key` 可以首次配置它）。
- 其余候选中多个已认证，或裸 id 跨多个 provider 且没有可用来裁决的认证：报错：
  `Model "<x>" is ambiguous across providers: <provider/id 列表>. <提示> Use --provider or provider/model.`
  提示可能是 `--api-key does not choose a provider.`、
  `No matching provider is authenticated.` 或 `More than one matching provider is authenticated.`
- 显式指定了 provider（`--provider` 或 `provider/` 前缀）但模型 id 不在目录：回退成该 provider
  下的自定义模型，并打印警告
  `Model "<id>" not found for provider "<provider>". Using custom model id.`
- 裸模型 id 在任何 provider 都找不到：报
  `Model "<x>" not found. Use --list-models to see available models.`

### 启动时的模型与思考等级优先级

新会话：

1. `--model`（可带 `--provider`）。
2. `--models` / `settings.enabledModels` 圈定范围的第一个模型；若 settings 里有默认模型且落在
   范围内，优先用默认模型。
3. settings 的 `defaultProvider` + `defaultModel`（要求该 provider 已配置认证）。
4. 按内置默认模型表找到的第一个已认证模型；否则第一个可用模型。
5. 都没有：无模型，交互界面提示先登录。

打开已有会话时，`--model` 仍然优先；没有 CLI 模型时先恢复会话最后一次选择的模型（要求仍在
目录中且已认证），恢复失败才走上面的默认链。

新会话的思考等级优先级：`--thinking`（或 `--model` 的 `:<thinking>` 后缀，两者同时给出时
`--thinking` 覆盖后缀）→ `modelThinkingLevels`（键为精确 `provider/id`）→
`defaultThinkingLevel` → 内置默认 `medium`。打开已有会话时，显式值之后先看会话里的 thinking
条目；没有该条目则直接用 `defaultThinkingLevel`（缺失则 `medium`）。最后都按所选模型支持的
范围收窄；没有模型时强制 `off`。

### `--list-models`

```bash
kiana --list-models
kiana --list-models sonnet
kiana --list-models "openai gpt-4.1"
```

- 只列出**当前认证就绪**的模型，不是全部 42 个 provider 的完整目录。
- 六列：`provider`、`model`、`context`、`max-out`、`thinking`、`images`；上下文/输出用
  `K`/`M` 缩写，`thinking` 与 `images` 为 `yes`/`no`。
- 可选搜索词按 `"<provider> <id>"` 做模糊匹配；空白或 `/` 分隔的多个 token 必须全部命中。
  搜索词只有不以 `-` 或 `@` 开头时才会被消费。
- 无任何可用模型时打印 `No models available. ...` 的登录引导，而不是空表。
- 搜索无命中时打印 `No models matching "<search>"`。
- `models.json` 加载错误只以 `Warning: errors loading models.json:` 写到 stderr，表格继续输出。
- 命令成功退出 `0`；没有任何可用模型时也打印引导并以 `0` 结束；`get_available` 的认证错误会
  打印 `Error: ...` 并退出 `1`。

### 交互模式：`/model` 与 `/scoped-models`

`/model`：

- 不带参数：打开模型选择器。
- 参数精确命中当前可用模型（有 scoped models 时只在该范围内找）：直接切换，不写默认设置，
  状态行显示 `Model: <id>`。
- 未命中：打开模型选择器，并把参数作为搜索框初值。
- 选择器内 `Ctrl+S` 保存为默认模型，状态行显示 `Default model: <provider>/<id>`，写入
  settings 的 `defaultProvider` / `defaultModel`。
- 模型目录缓存未命中且没有 scoped models 时，会后台刷新目录（15 秒超时）；超时或失败只显示
  警告，仍用缓存模型。

`/scoped-models`：

- 多选启用哪些模型供 `Ctrl+P` / `Ctrl+Shift+P`（Windows/WSL 为 `Alt+P`）循环；`Ctrl+L`
  直接打开模型选择器。
- 选择变化立即写回当前会话；保存动作写入 settings 的 `enabledModels`，状态行显示
  `Model selection saved to settings`。全部选中时等价于清空该设置。
- 选择器内默认键位：`Ctrl+S` 保存、`Ctrl+A` 全选、`Ctrl+X` 清空、`Ctrl+P` 切换选中项所属
  provider 的全选状态、`Alt+Up` / `Alt+Down` 调整顺序（以 `/hotkeys` 为准）。
- 选择器同样会后台刷新目录（15 秒超时），刷新失败只警告。
- 用 `enabledModels` 或 `--models` 圈定后，循环只会经过范围内的模型；范围为空时循环全部可用模型。
- 循环成功状态为 `Switched to <name>`，模型支持 thinking 且等级非 `off` 时追加
  ` (thinking: <level>)`；没有可切换模型时显示 `Only one model available` 或
  `Only one model in scope`。

`--models` 与 `enabledModels`：

| 项 | 说明 |
| --- | --- |
| `--models <patterns>` | 逗号分隔；支持 glob（`anthropic/*`、`*sonnet*`）和模糊匹配，也支持 `model:thinking` |
| `settings.enabledModels` | 字符串数组，语义同上；`/scoped-models` 保存的是精确 `provider/id` |
| 优先级 | 同一次运行 `--models` 覆盖 `settings.enabledModels` |
| 匹配范围 | 只在认证就绪的模型里匹配；未命中的 pattern 以 `Warning:` 写到 stderr |
| 用途 | 决定 `Ctrl+P` 循环范围，以及新会话的初始模型候选 |

## Thinking 等级与预算

### 等级

`--thinking`、`--model ...:<level>`、`/thinking` 共用 7 个等级：

| 等级 | 含义 |
| --- | --- |
| `off` | 关闭思考 |
| `minimal` | 最小预算 |
| `low` | 低预算 |
| `medium` | 中预算（内置默认） |
| `high` | 高预算 |
| `xhigh` | 超高；预算复用 `high` |
| `max` | 最大；预算复用 `high` |

- 具体可用等级由模型目录的 `reasoning` / `thinkingLevelMap` 决定；非 reasoning 模型只支持
  `off`。
- 请求的等级超出模型能力时会被收窄到支持范围；界面状态行显示的是**请求值**，可能与实际
  生效值不同。
- `--thinking` 的值非法时只打印警告
  `Invalid thinking level "<x>". Valid values: off, minimal, low, medium, high, xhigh, max`，
  该值被忽略（不是致命错误）。

### `/thinking`

```text
/thinking
/thinking high
```

- 不带参数：打开选择器；当前模型的可用等级才会列出，设置里保存的默认等级会标注 `· default`。
- 带参数：在当前模型可用等级里做小写精确匹配；未命中显示
  `Unknown thinking level "<x>". Available levels: <列表>.`
- 直接应用不持久化时状态行显示 `Thinking level: <level>`；在选择器里保存默认时显示
  `Default thinking level: <level>`，写入 settings 的 `defaultThinkingLevel`。
- `Shift+Tab` 循环切换；模型不支持 thinking 时显示 `Current model does not support thinking`。

### `thinkingBudgets`

```json
{
  "thinkingBudgets": {
    "minimal": 1024,
    "low": 2048,
    "medium": 8192,
    "high": 16384
  }
}
```

| 键 | 类型 | 未设置时的实际回退 |
| --- | --- | --- |
| `thinkingBudgets.minimal` | number | `1024` |
| `thinkingBudgets.low` | number | `2048` |
| `thinkingBudgets.medium` | number | `8192` |
| `thinkingBudgets.high` | number | `16384` |
| `xhigh` / `max` | — | 没有独立键，复用 `high` 的预算 |

注意：

- 这四个默认值来自适配器常量 `DEFAULT_THINKING_BUDGETS`；settings schema 本身不写默认值。
- 只有使用 token budget 的适配器会读它；用 reasoning effort 表达的 provider 通常不读取
  `thinkingBudgets`。是否生效取决于模型与适配器。
- 在 thinking 预算与 `maxTokens` 共用上限的适配器里，会为最终回答保留至少
  `MIN_ANSWER_TOKENS`（1024）个 token；实际预算可能因此被截断。

## 自定义 provider 与模型：`models.json`

`models.json` 默认位于 `<agentDir>/models.json`（默认 `~/.kiana/agent/models.json`，受
`KIANA_CODING_AGENT_DIR` 影响）。文件是可选的；不存在等同于空配置。

最小示例：

```jsonc
{
  "providers": {
    "my-gateway": {
      "name": "My Gateway",
      "baseUrl": "https://gateway.example.com/v1",
      "api": "openai-completions",
      "apiKey": "$MY_GATEWAY_KEY",
      "models": [
        {
          "id": "my-model",
          "name": "My Model",
          "contextWindow": 128000,
          "maxTokens": 8192,
          "reasoning": true,
          "input": ["text", "image"]
        }
      ]
    }
  }
}
```

字段（camelCase）：

| provider 字段 | 类型 | 说明 |
| --- | --- | --- |
| `name` | string | 显示名 |
| `baseUrl` | string | API base URL，也会作用于该 provider 的内置模型 |
| `apiKey` | string | 字面量、`$NAME` / `${NAME}` 环境变量模板，或 `!command` 命令输出 |
| `api` | string | 适配器名，例如 `openai-completions`、`anthropic-messages`、`openai-responses` |
| `oauth` | `"radius"` | 使用内置 Radius OAuth 流程；要求同时提供 `baseUrl` |
| `headers` | object | 额外请求头；值同样支持 `$NAME` / `!command` |
| `authHeader` | bool | 把解析出的 API key 作为 `Authorization: Bearer` 发送 |
| `compat` | object | OpenAI 兼容层开关；字段较多，只在需要覆盖自动探测时使用 |
| `models` | array | 自定义模型；同 id 的 Chat 条目不重复追加 |
| `modelOverrides` | object | 按模型 id 覆盖目录字段，如 `name`、`reasoning`、`contextWindow`、`maxTokens` |

模型字段至少要有 `id`；常用字段还有 `name`、`api`、`baseUrl`、`reasoning`、
`thinkingLevelMap`、`input`、`contextWindow`、`maxTokens`、`cost`、`headers`、`compat`、
`samplingParams`。`contextWindow` / `maxTokens` 等数值字段允许小数，合成模型时截断。

解析规则：

- 支持 JSONC：`//` 行注释和尾逗号会被去掉；**不支持** `/* ... */` 块注释。
- `$NAME`、`${NAME}`、`$$`→`$`、`$!`→`!`；裸 `$`、未闭合 `${` 按字面保留。
  任一变量取不到非空值，整个模板解析失败。
- `!command` 通过 shell 执行，stdout 按 JS `trim()` 裁剪；10 秒超时、1 MiB 输出上限，
  失败或空输出视为没有值。
- **整份文件要么全部通过校验，要么整体拒绝**：任何 IO/JSON/schema 错误都会让
  `models.json` 的 provider 列表为空并记录错误，没有“坏条目跳过”的降级。
- 同 id 的 provider 会覆盖/合并内置 provider；`models` 按 id upsert。
- `oauth: "radius"` 缺 `baseUrl` 时，加载阶段能通过，合成 provider 时报错。

`--list-models` 会以警告显示 `models.json` 的加载/解析错误，并继续列出能加载到的模型。

## `kiana auth` 子命令

三个子命令都要求至少给出 `--provider` 或 `--model`。`kiana auth --help`（或
`kiana auth check --help`）打印完整用法。

### `auth check`

```bash
kiana auth check --provider openai
kiana auth check --provider anthropic --no-refresh
kiana auth check --model gpt-4o --json --credentials
```

| 选项 | 作用 |
| --- | --- |
| `--provider <id>` | 直接检查该 provider |
| `--model <ref>` | 先解析模型，再检查其 provider；可和 `--provider` 同用 |
| `--json` | 输出 JSON；字段顺序 `status`、`provider`、`reason`、`authType`、`credentials` |
| `--credentials` | 额外输出凭据；JSON 中放在 `credentials` 字段 |
| `--no-refresh` | 不刷新 OAuth；使用只读凭据存储，不会创建 `auth.json` |

行为：

- 默认会刷新快过期的 OAuth，并把刷新后的 token 写回存储。
- 非 JSON 输出通常只有 `ready` / `not_ready` / `invalid`；带 `--credentials` 且凭据可用时打印
  凭据本身，否则仍打印状态。
- `--json` 的 `status` 取值：`ready`、`not_ready`、`invalid`。
- `reason` 取值：`provider_not_found`、`credentials_not_configured`、`credential_not_available`、
  `invalid_state`。`credential_not_available` 只在 `--credentials` 取不到非空凭据时出现。
- `authType` 取值：`api_key`、`oauth`。
- 退出码：`0` = ready，`1` = not_ready，`2` = invalid；`check` 执行阶段的参数/运行时错误
  通常也映射为 `2`，而更早发生的未知选项解析可能返回 `1`。
- `--no-refresh --credentials` 会直接返回存储里的 OAuth `access`，**即使它已经过期**，也不会
  触发刷新。需要有效 token 时不要加 `--no-refresh`。

示例：

```bash
kiana auth check --provider openai --json
# {"status":"ready","provider":"openai","authType":"api_key"}

kiana auth check --provider anthropic --no-refresh --json
# {"status":"not_ready","provider":"anthropic","reason":"credentials_not_configured"}
```

### `auth print-api-key`

```bash
kiana auth print-api-key --provider openai
kiana auth print-api-key --provider openai --model gpt-4o
```

- 打印真实请求会使用的 API key，只输出 key 本身（末尾换行）。
- 显式 `--provider` 时，即使没有已保存凭据，也可以从环境变量解析并打印。
- 如果该 provider 已保存的是 OAuth 凭据，会报
  `Provider "<id>" is configured with OAuth, not an API key`。
- 只给 `--model` 时，候选 provider 仅限在 `auth.json` 里有存储凭据的 provider；纯环境变量
  认证的 provider 不会被考虑。多个 provider 命中时报
  `Multiple configured providers matched (...). Specify --provider.`。
- 成功退出 `0`；参数/解析/凭据错误退出 `1`，stderr 前缀 `Error:`。

### `auth print-bearer-token`

```bash
kiana auth print-bearer-token --provider openai-codex
kiana auth print-bearer-token --provider openai-codex --min-expiry 1h
```

- 只打印**已保存 OAuth 凭据**的 bearer token；环境变量中的 OAuth token 不会被它导出。
- provider 只有 API key 存储时，报
  `Provider "<id>" is not configured with an OAuth bearer token`。
- `--min-expiry <duration>`：默认 `30m`。取值形如 `1h`、`30m`、`90s`、`500ms`；token 剩余
  有效期低于该值时先刷新，刷新后仍不足则报错。
- 单位比较沿用上游实现的特殊行为：正则接受大小写，但只有小写的 `ms`/`s`/`m` 被识别，其它
  （包括大写 `M`/`S`/`H`）一律按小时计算。建议只使用小写单位。
- `--min-expiry` 只允许用于本子命令；`--json` / `--credentials` / `--no-refresh` 只允许用于
  `auth check`。
- 只给 `--model` 时同样只考虑有存储凭据的 provider。

### `/login` 与 `/logout`

登录/登出只在交互模式提供，**没有** `kiana login` / `kiana logout` CLI 子命令。

`/login`：

- `/login`：打开认证方式选择器（OAuth / API key / Radius 直达）。
- `/login <provider>`：按 **id 或显示名**做 trim 后的小写精确匹配；唯一命中时直接进入该
  provider 的登录流程；同一 provider 同时有 OAuth 和 API key 时再让你选方式；未命中则打开
  provider 选择器，并把参数作为搜索初值。
- API key 登录会把 key 写入 `auth.json`；OAuth 登录完成后的目录刷新在后台进行，15 秒超时只会
  显示警告，登录本身已经成功。
- 登录不会修改环境变量或 `models.json`。

`/logout`：

- 列出 `auth.json` 中已保存的凭据，删除选中 provider 的持久化凭据。
- OAuth 凭据删除后显示 `Logged out of <name>`。
- 存储的 API key 删除后显示
  `Removed stored API key for <name>. Environment variables and models.json config are unchanged.`
- 没有可删除凭据时显示
  `No stored credentials to remove. /logout only removes credentials saved by /login; environment variables and models.json config are unchanged.`
- `/logout` 不会删除环境变量里的 key，也不会改动 `models.json` 的 `apiKey`。

## 参考

### CLI flags

| Flag | 说明 |
| --- | --- |
| `--provider <name>` | 限制 `--model` 的搜索范围；必须与 `--model` 同用 |
| `--model <pattern>` | 模型 id、`provider/id`，可带 `:<thinking>` |
| `--models <patterns>` | 逗号分隔的循环范围；覆盖 `settings.enabledModels` |
| `--thinking <level>` | 7 个等级之一；非法值只警告并使用默认 |
| `--api-key <key>` | 本次运行使用的 API key；不落盘，不参与 provider 歧义裁决 |
| `--list-models [search]` | 列出认证就绪的模型，可选模糊搜索 |

### settings 键

| 键 | 类型 | 说明 |
| --- | --- | --- |
| `defaultProvider` | string | 默认 provider；与 `defaultModel` 配对 |
| `defaultModel` | string | 默认模型 id；`/model` 选择器保存时写入 |
| `defaultThinkingLevel` | 7 个等级之一 | 全局默认思考等级 |
| `modelThinkingLevels` | record<string, 等级> | 按精确 `provider/id` 覆盖思考等级，不做 glob |
| `enabledModels` | string[] | `Ctrl+P` 循环范围；支持 glob/模糊匹配 |
| `thinkingBudgets` | object | `minimal` / `low` / `medium` / `high` 四档 token 预算 |

settings 的分层与写入方式见 [设置（settings）](settings.md)。

### 其他 `KIANA_*` 环境变量

这些不是 provider key，但会影响模型配置、凭据与会话路径：

| 变量 | 说明 |
| --- | --- |
| `KIANA_CODING_AGENT_DIR` | agent 配置目录；默认 `~/.kiana/agent`，`models.json`、`auth.json` 与 settings 都在其下 |
| `KIANA_CODING_AGENT_SESSION_DIR` | 会话存储目录；被 `--session-dir` 覆盖，详见 [会话、分支与恢复](sessions.md) |
| `KIANA_API_BASE_URL` | Kiana 托管服务的基础 URL；无默认值，未设置时相关网络功能不可用 |
| `KIANA_INSTALLER_API_BASE` | 托管更新的 release API；默认 `KIANA_API_BASE_URL + /api/installer/releases` |
| `KIANA_PACKAGE_DIR` | 覆盖包目录（用于 Nix/Guix store 路径） |
| `KIANA_OFFLINE` | 设为 `1`/`true`/`yes` 时关闭启动期网络操作 |
| `KIANA_TELEMETRY` | 覆盖安装遥测：只要已定义（含空串）就完全覆盖 settings；`1`/`true`/`yes`（大小写不敏感，不做 trim）开启，任何其他值（含 `0`/`false`/`no`/`on`、空串）关闭；未设置时回退 `enableInstallTelemetry` |
| `KIANA_SHARE_VIEWER_URL` | `/share` 查看器链接的基础 URL；默认 `KIANA_API_BASE_URL + /session/` |

### 斜杠命令

| 命令 | 说明 |
| --- | --- |
| `/model [provider/model]` | 精确命中直接切换，否则打开选择器；`Ctrl+S` 保存默认 |
| `/scoped-models` | 选择 `Ctrl+P` 循环的模型，可保存到 `enabledModels` |
| `/thinking [level]` | 打开选择器或直接设置；保存默认时写入 `defaultThinkingLevel` |
| `/login [provider]` | OAuth / API key 登录 |
| `/logout` | 删除已保存凭据（不影响 env 与 models.json） |

## 限制与排查

### 当前限制

- **`--list-models` 不是完整目录**：只显示认证就绪的模型；没有可用认证时只打印登录引导。
- **`--model` 可以解析到未认证模型**：候选集中在单个 provider（或显式指定 provider）时不看认证，
  真正请求时才可能报没有 API key；用 `--list-models` 先确认。
- **`--api-key` 不持久化**：不会写 `auth.json`；也不会帮助在多个 provider 的同名模型之间做
  选择。
- **纯 OAuth provider 忽略 `--api-key`**：例如 `openai-codex`。
- **打印 bearer token 只认存储的 OAuth**：`ANTHROPIC_OAUTH_TOKEN` 这类环境变量不会被导出；
  只给 `--model` 时也只遍历有存储凭据的 provider。
- **`/logout` 不清理环境变量与 models.json**：它只删除 `/login` 写入的凭据。
- **OAuth 刷新依赖网络**：默认 15 秒刷新超时；刷新失败不会静默回退环境变量。CI 里检查认证
  建议加 `--no-refresh`。
- **`thinkingBudgets` 不是通用开关**：用 reasoning effort 的 provider 通常不读取它；
  `xhigh`/`max` 没有独立预算。
- **`--thinking` 拼错不报错**：只警告并忽略。
- **`models.json` 没有部分降级**：任何错误都会拒绝整份配置；只支持 `//` 行注释与尾逗号，
  不支持块注释。
- **`oauth: "radius"` 依赖 `baseUrl`**：缺了会在合成 provider 阶段报错。
- **没有 `kiana login` / `kiana logout` CLI**：登录登出只在交互模式。
- **平台**：只验证 Linux x86_64；跨平台行为未实测。
- **版本**：`kiana --version` 是 `1.4.0`，workspace 包是 `0.1.0`，这是有意的 TS 兼容常量。

### 常见错误

| 现象 | 原因 | 处理 |
| --- | --- | --- |
| `No models available. ...` | 没有任何认证就绪的模型 | 设置环境变量或用 `/login` 登录；再跑 `--list-models` |
| `No API key found for <provider>.` | 选中的 provider 没有 key | 设置对应环境变量、`--api-key`，或 `/login` |
| `Unknown provider "<x>"` | provider id 拼错 | 对照本页表格；`/login` 的选择器也列出所有 provider |
| `Model "<x>" not found` | 裸模型 id 不存在 | `kiana --list-models <x>` 模糊查找 |
| `Model "<x>" is ambiguous across providers: ...` | 同名模型跨多个 provider | 加 `--provider`，或写成 `provider/id` |
| `... Using custom model id.` | 模型不在目录但 provider 存在 | 确认 id 正确；这是警告，按自定义模型继续 |
| `--provider requires --model ...` | 只给了 `--provider` | 补 `--model`，或直接用 `--model provider/id` |
| `Provider "<id>" is configured with OAuth, not an API key` | 用 print-api-key 打印 OAuth provider | 改用 `auth print-bearer-token`，或先 `/logout` |
| `Provider "<id>" is not configured with an OAuth bearer token` | 用 print-bearer-token 打印非 OAuth provider | 用 `auth print-api-key`，或先 `/login` 走 OAuth |
| `Multiple configured providers matched (...)` | 只给 `--model` 且多个存储 provider 命中 | 加 `--provider` 收窄 |
| `No usable API key is configured` | 没有存储或环境 key | 配置对应环境变量或 `/login` |
| `Invalid thinking level "<x>". Valid values: ...` | `--thinking` 拼错 | 使用 7 个合法值；注意这只是警告 |
| `Unknown thinking level "<x>". Available levels: ...` | `/thinking` 参数不在当前模型支持范围 | 用选择器查看可用等级；非 reasoning 模型只有 `off` |
| `Current model does not support thinking` | `Shift+Tab`/循环遇到非 reasoning 模型 | 换支持 thinking 的模型 |
| `Warning: errors loading models.json:` | `models.json` 解析/校验失败 | 修正 JSON/字段；`models.json` 是整份拒绝 |
| `Error: Failed to resolve credential` | 凭据存储读取、OAuth 刷新或网络失败 | 检查 `auth.json` 与网络；或 `--no-refresh` 避免刷新 |
| `auth check` 退出 `1` | `not_ready`：provider 未注册或没凭据 | 看 `--json` 的 `reason` 字段 |
| `auth check` 退出 `2` | `invalid`：运行时/读取失败，或执行期参数错误 | 检查 agent 目录权限、models.json 与参数 |
| `/logout` 后请求仍成功 | 环境变量或 models.json 仍提供 key | 清掉环境变量或修改 models.json；`/logout` 只管存储凭据 |

更多启动与网络问题见 [故障排查](troubleshooting.md) 和
[遥测与离线](telemetry-and-offline.md)。

## 相关页面

- [快速开始](getting-started.md)：首次认证、`auth check` 与 `--api-key`
- [CLI 参考](cli.md)：完整 flag、子命令、环境变量与退出码
- [交互模式](interactive-mode.md)：`/model`、`/thinking`、`/login` 的界面行为
- [设置（settings）](settings.md)：`defaultProvider`、`enabledModels`、`thinkingBudgets` 等
- [会话、分支与恢复](sessions.md)：会话里保存的模型选择与 thinking 记录
- [故障排查](troubleshooting.md)：通用启动、网络与凭据问题
