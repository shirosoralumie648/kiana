# Skills 与扩展

本页讲什么：如何用 Markdown 技能（skills）、prompt 模板、主题（themes），以及如何安装、启用、禁用和排查扩展（extensions）与包（packages）。包是扩展、技能、prompt 模板和主题的分发单位；`kiana install` 把包写进 settings。

版本说明：workspace 包版本是 `0.1.0`，但 `kiana --version` 输出 `1.4.0`（有意保留的 TS 兼容常量），不是构建错误。

## 快速开始

| 我想… | 做法 |
| --- | --- |
| 创建一个技能 | 新建 `.kiana/skills/<name>/SKILL.md`，写 frontmatter 与正文；重启会话或 `/reload` |
| 让技能可被模型按需加载 | 在技能描述里写清适用任务；模型会看到 `skill` 工具与系统提示里的技能索引 |
| 用户主动调用技能 | `/skill:<name> [参数]` |
| 创建一个 prompt 模板 | 新建 `~/.kiana/agent/prompts/<name>.md`，输入 `/<name> 参数` |
| 换主题 | `--use-theme dark` 或 settings 的 `"theme": "dark"` |
| 安装一个包 | `kiana install ./local/path`（用户级）或 `kiana install ./local/path -l`（项目级） |
| 查看已安装包 | `kiana list` |
| 启用/禁用某个资源 | `kiana config`（TUI 中按 `Tab` 切换 global/project 作用域） |
| 临时加载一个扩展 | `kiana -e ./ext.ts` 或 `kiana --extension builtin:codemode` |

## Skills（技能）

技能是一个按需加载的 Markdown 指令包：平时只在系统提示里占一条索引，命中任务后由模型调用 `skill` 工具（或用户输入 `/skill:<name>`）加载正文。

### 技能放在哪里

| 位置 | 文件/目录 | 说明 |
| --- | --- | --- |
| 用户全局 | `<agentDir>/skills`，默认 `~/.kiana/agent/skills` | 总是加载 |
| 用户 `.agents` | `~/.agents/skills` | 与 `.kiana` 并行的通用约定目录 |
| 项目 | `<cwd>/.kiana/skills` | 仅项目受信时加载 |
| 项目祖先目录 | 从 `<cwd>` 向上直到 git 仓库根（含）的每个 `<dir>/.agents/skills` | 仅项目受信时加载；近处优先 |
| CLI | `--skill <path>`（可多次） | 文件必须是 `.md`；目录会递归扫描；相对路径按当前工作目录解析 |
| settings | `skills` 数组 | 项目条目相对 `<cwd>/.kiana`，用户条目相对 `<agentDir>`；项目条目需项目受信 |
| 包/扩展 | 安装包 `package.json` 的 `kiana.skills`，或包内约定 `skills/` 目录 | 由包解析器发现 |

`--no-skills`（`-ns`）只关闭自动发现与 settings 合并；显式 `--skill` 路径仍会加载，通过 `-e` 载入的包所附带的技能也仍会加载（当前实现）。

### 文件格式与识别规则

最小示例 `.kiana/skills/review/SKILL.md`：

```markdown
---
name: review
description: Review a diff for correctness bugs and missing tests.
allowed-tools: Read Grep Bash(git diff)
---

# Review

1. Read the requested files.
2. Report concrete defects with file and line.
```

识别规则：

- 目录根有 `SKILL.md`：整个目录算一个技能，加载后不再继续深入子目录。
- 没有 `SKILL.md` 时，目录根的直接 `.md` 文件也可以作为技能（必须有非空 `description`）；子目录只认 `SKILL.md`。
- 兼容 `.agents/skills` 约定：该目录下的散装 `.md` 在非根层级也可被识别（根层除外）。
- 每层目录的 `.gitignore`、`.ignore`、`.fdignore` 规则都会生效；以 `.` 开头的条目与 `node_modules` 被跳过；符号链接跟随目标。
- 技能名取 frontmatter 的 `name`，缺省取父目录名；必须匹配 `^[a-z0-9-]+$`、不超过 64 个 UTF-16 单元、首尾不能是连字符、不能有连续连字符。名字非法只产生警告，技能仍会加载。
- `description` 必填且不能为空白，最长 1024 个 UTF-16 单元；缺失时该文件不会成为技能。
- 同名技能只保留先加载的定义，后来的产生 `collision` 诊断。

### frontmatter 参考

| 字段 | 类型 | 当前行为 |
| --- | --- | --- |
| `name` | string | 技能名；缺省用父目录名；规则见上 |
| `description` | string | 必填；进入系统提示索引；超过 250 字符的展示会截断（内置技能不截断） |
| `allowed-tools` | string[] | 激活期间授予 Allow 权限规则；`Deny` 永远优先；无法解析的条目被忽略 |
| `disable-model-invocation` | bool | 为 `true` 时模型不能通过 `skill` 工具加载；用户 `/skill:` 仍可调用 |
| `user-invocable` | bool | 默认 `true`；为 `false` 时 `/skill:` 调用被拒绝 |
| `arguments` | 数组/映射 | 声明命名参数；`/skill:` 参数会按声明校验 |
| `argument-hint` | string | 斜杠命令补全/帮助里的参数提示 |
| `paths` | string/string[] | glob 条件可见性：本会话触达过匹配路径后，技能才进入模型索引 |
| `model` / `effort` | string | 已解析并写入激活台账，但**当前不会**切换模型或 thinking 级别（当前实现/限制） |
| `context` / `agent` | `inline`/`fork` + string | 已解析；`fork` 子代理执行**尚未实现**（当前实现/限制） |
| `hooks` | object | 已解析；技能激活期间的 hooks **尚未安装**（当前实现/限制） |
| 其它字段 | 任意 | 原样保留，不报错 |

`allowed-tools` 的常用写法：

| 写法 | 含义 |
| --- | --- |
| `Read` / `mcp__docs__search` / `*` | 工具名 Allow 规则 |
| `Bash(git status)` | 命令前缀 Allow |
| `Bash(git status:*)` / `Bash(git status*)` | 命令通配 Allow |
| `Bash(*)` | `bash` 工具 Allow |
| `Read(src/**)` / `Edit(src/**)` | 路径前缀 Allow（读/写动作） |

无法识别的括号语法会进入 `invalid` 列表，不会退化成宽泛的工具名规则。

### 调用与激活

- 模型路径：系统提示出现 `skill` 工具。只有在存在技能定义且 `skill` 工具被允许时才注册；扩展/SDK 注册了同名工具时扩展优先。工具返回 `<skill name location>…<skill_files>…<skill_content>…` 的转义内容，正文中的 `$ARGUMENTS`（全部参数按声明顺序空格连接）与 `$1`…`$N` 会被替换，并登记一次激活。
- 用户路径：`/skill:<name> [参数]`。校验 `user-invocable`，按声明校验参数；注入的 skill 块是**未替换占位符的正文**，原始参数文本追加在块末尾（当前实现）。
- 参数格式（`/skill:` 用户路径）：技能声明了 `arguments` 时，支持 `参数名=值`、`--参数名=值`、`--参数名 值`，或按声明顺序的位置参数；未知参数与缺失的必填参数会报错。没有声明 `arguments` 时原始参数文本原样追加。
- 停用：`/skill:deactivate <name>` 或 `/skill:off <name>`；会话结束也会清空全部激活。
- 激活 TTL：settings 顶层 `skillActivationTtlSeconds`（秒；旧的对象形式 `skills.activationTtlSeconds` / `skills.activationTtl` 会在加载时迁移到顶层）。值为 `0`、负数或非数字表示不自动过期；默认不过期。
- `enableSkillCommands`（默认 `true`）：控制 `/skill:` 是否出现在斜杠命令列表与自动补全里。会话对用户手输 `/skill:<name>` 的展开不依赖该开关（当前实现）。

`allowed-tools` 的授权只存在于内存激活台账，不会写入 settings，也不能覆盖 deny、只读模式或灾难性命令检查。

### 系统提示里的技能索引

会话按当前模型上下文窗口计算索引预算：`tokens × 4 × 1%`，未知窗口时用默认 8000 字符。非内置技能的单条描述先截到 250 字符；预算不足时按比例分配，低于 20 字符时只保留名字与位置；仍然超出则从尾部丢弃非内置条目。内置（bundled）技能不截断，且允许超出预算。

`paths` 非空的技能在会话触达匹配路径前不会进入索引；`disable-model-invocation` 的技能永远不进入模型索引，但这不影响用户调用。

## Prompt 模板

prompt 模板是“斜杠命令 + Markdown 正文 + 参数替换”的轻量机制。

### 放在哪里

| 位置 | 说明 |
| --- | --- |
| `<agentDir>/prompts` | 用户全局目录；只扫描直接子项，不递归 |
| `<cwd>/.kiana/prompts` | 项目目录；需项目受信；只扫描直接子项 |
| `--prompt-template <path>` | 可多次；文件或目录；相对路径按当前工作目录解析 |
| settings `prompts` 数组 | 项目条目相对 `<cwd>/.kiana`，用户条目相对 `<agentDir>` |
| 包/扩展 | 包的 `kiana.prompts` 或约定 `prompts/` 目录 |

`--no-prompt-templates`（`-np`）关闭自动发现与 settings 合并；显式 `--prompt-template` 仍会加载。

### 文件格式

模板名是文件名去掉末尾 `.md`（大小写敏感）。frontmatter 可选：

```markdown
---
description: Review a pull request diff.
argument-hint: <branch>
---

Review the diff against $1. Focus on $2.
```

| 字段 | 说明 |
| --- | --- |
| `description` | 缺省时取正文第一条非空行并截断到 60 个 UTF-16 单元 |
| `argument-hint` | 自动补全/命令列表里的参数提示；空串视为未提供 |

### 调用与参数替换

输入 `/<模板名> 参数` 时，会话在提交阶段展开：

| 占位符 | 替换结果 |
| --- | --- |
| `$ARGUMENTS` / `$@` | 全部参数以单个空格连接 |
| `$1` … `$N` | 第 N 个参数（1 起）；越界或 `$0` 为空串 |
| `${N:-默认值}` / `${@:-默认值}` / `${ARGUMENTS:-默认值}` | 目标缺失或为空串时使用默认值 |
| `${@:N}` / `${@:N:L}` | 从第 N 个参数起取 L 个；省略 L 取到末尾；N=0 等同 N=1 |

- 参数按 bash 风格引号分组：`"..."` / `'...'` 保留为单个参数，空引号产生空参数；**不支持反斜杠转义**。
- 模板名精确、大小写敏感；同名模板只有第一个生效；未命中模板时输入原样保留并作为普通消息发送。
- 替换只做一遍，代入的参数值不会被再次展开。
- `/reload` 会重载 prompt 模板；流式响应或压缩进行中会拒绝 reload 并提示先等待完成。

## 主题（Themes）

### 内置与自定义

内置主题：`system`、`dark`、`light`。`system` 用当前终端颜色现生成，并跟随终端明暗变化；未设置 `theme` 时使用 `system`。

自定义主题是 `.json` 文件，加载位置：

| 位置 | 说明 |
| --- | --- |
| `<agentDir>/themes/*.json` | 用户主题目录；只扫描直接子项 |
| `<cwd>/.kiana/themes/*.json` | 项目主题目录；需项目受信；只扫描直接子项 |
| `--theme <path>` | 可多次；`.json` 文件或目录；相对路径按当前工作目录解析 |
| settings `themes` 数组 | 项目条目相对 `<cwd>/.kiana`，用户条目相对 `<agentDir>` |
| 包/扩展 | 包的 `kiana.themes` 或约定 `themes/` 目录 |

`--no-themes` 关闭主题文件发现与 settings 合并；显式 `--theme` 仍会加载；内置 `system`/`dark`/`light` 不受影响。

### 主题文件

顶层是 JSON 对象。生产构建会安装严格校验器，非法主题在加载时被拒绝：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `name` | 是 | 主题显示名；不能包含 `/`（`/` 预留给 `light/dark` 自动形式） |
| `appearance` | 否 | `dark` / `light`；省略时由颜色推导 |
| `colors` | 是 | token → 颜色。颜色可以是 `#RGB`/`#RRGGBB`、`oklch(...)`/`okhsl(...)`、变量引用、`""`（终端默认色）或 0–255 的 256 色索引 |
| `vars` | 否 | 可复用颜色变量 |
| `export` | 否 | HTML 导出用的颜色 |

主题名取文件内容里的 `name`；文件名只用于筛 `.json`。生产校验器要求 `name` 与 `colors` 必填，缺失或非法的主题会被拒绝。同名主题按加载顺序保留先出现的一个。

### 选择主题

- settings `"theme"`：单个主题名，或 `light/dark` 自动形式（恰好一个 `/`，两侧非空；按终端当前明暗选择）。
- `--use-theme <name[/name]>`：只对本次交互运行生效，覆盖 `theme` 设置但不写回 settings；非交互模式不生效。
- `/settings` 的 theme 子菜单：交互式选择并持久化到 settings。
- 主题无法加载时回退到 `system`（设置里显式指定的非法主题会给出错误提示；启动期初始化失败则静默回退）。

```bash
kiana --theme ~/.kiana/agent/themes/my-theme.json --use-theme my-theme
kiana --use-theme "light/dark"
```

## 扩展与包（Extensions / Packages）

用户扩展用 TypeScript/JavaScript 编写（`.ts` / `.js`），在嵌入的 JS 运行时中加载；内置扩展是编译进二进制的 Rust 实现。包是扩展与资源的集合，可来自 npm、git、本地目录或内置。

### 命令

| 命令 | 作用 | 关键点 |
| --- | --- | --- |
| `kiana install <source> [-l]` | 安装包并把源写入 settings | 默认写用户 settings；`-l` 写项目 `.kiana/settings.json`，需要项目受信或 `--approve` |
| `kiana remove <source> [-l]` | 从 settings 删除包源（同时删除磁盘安装） | 无匹配时打印 `No matching package found for …` 并以失败退出 |
| `kiana uninstall <source> [-l]` | `remove` 的别名 | 同上 |
| `kiana list` | 列出用户与项目 settings 里的包 | 分组显示 `User packages:` / `Project packages:`；带过滤规则的条目标注 `(filtered)`，并尽量显示安装路径 |
| `kiana update [source/self/kiana]` | 更新 kiana、扩展或模型目录 | 无目标默认只更新自身；`--all` 同时更新自身与扩展；`--models` 只刷新模型目录；`--extension <source>` 只更新一个包；`--force` 强制重装自身 |
| `kiana config [-l]` | 打开资源启用/禁用 TUI | 默认从 global 开始，`Tab` 切换 global/project；`-l` 进入时使用项目作用域；项目作用域需要受信 |

`kiana install` 支持的源写法（来自 `kiana install --help`）：

```bash
kiana install npm:@foo/bar
kiana install git:github.com/user/repo
kiana install git:git@github.com:user/repo
kiana install https://github.com/user/repo
kiana install ssh://git@github.com/user/repo
kiana install ./local/path
```

安装成功后分别打印 `Installed <source>` / `Removed <source>`。settings 写入是异步队列，CLI 在返回前会 flush。

### settings：`packages`

`kiana install` 写入的是 `packages` 数组，元素可以是字符串或对象：

```json
{
  "packages": [
    "npm:@foo/bar",
    {
      "source": "./vendor/my-package",
      "autoload": true,
      "skills": ["!experimental/**"]
    }
  ]
}
```

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `source` | string | 必填；npm/git/本地路径等源字符串 |
| `autoload` | bool | 仅项目作用域的对象条目有意义：设为 `false` 时该条目是“增量覆盖”，解析与安装改用同 identity 的用户包作为基底，只追加显式列出的过滤模式；设为 `true` 或省略时按普通包处理 |
| `extensions` / `skills` / `prompts` / `themes` | string[] | 资源过滤规则：`!` 按 glob 排除，`+` 按精确路径重新启用，`-` 按精确路径最后关闭；无前缀条目按 glob 匹配 |

旧版把 `skills` 写成 `{ "enableSkillCommands": …, "customDirectories": [...] }` 的配置会在加载时迁移：`enableSkillCommands` 复制到顶层，`customDirectories` 替换为 `skills` 数组。新配置请直接使用数组。

### settings：`extensions`

`extensions` 是扩展路径与启用/禁用规则的数组：

```json
{
  "extensions": [
    "./tools/my-extension.ts",
    "-builtin:mcp"
  ]
}
```

| 写法 | 含义 |
| --- | --- |
| 普通路径 | 扩展文件或目录；目录按扩展发现规则解析 |
| `!path` / `-path` | 禁用（`!` 按模式匹配，`-` 按精确路径）；用于排除自动发现或内置扩展 |
| `+path` | 重新启用（精确路径）；内置扩展默认启用，用 `+builtin:<name>` 可覆盖项目级的 `-` |
| `+builtin:<name>` / `-builtin:<name>` | 指向内置扩展的精确覆盖；名字见下表。单独写不带前缀的 `builtin:<name>` 不会额外启用内置扩展（内置默认已启用） |

内置扩展（顺序固定）：

| 名称 | 说明 |
| --- | --- |
| `llama.cpp` | 本地 llama.cpp provider 与 `/llama` 命令；不可被同名扩展替换（`--no-extensions` 或 `-builtin:llama.cpp` 仍可关闭） |
| `codemode` | codemode 工具；可被同名扩展替换 |
| `tool-search` | 工具搜索；可被同名扩展替换 |
| `mcp` | MCP 支持与 `/mcp` 命令；可被同名扩展替换 |

`--no-mcp` 等价于按名禁用内置 `mcp`（不连接服务器、不暴露 MCP 工具），并不影响其它内置扩展。

### 扩展发现

扩展目录的自动发现位置：

1. `<cwd>/.kiana/extensions`（项目，需受信）
2. `<agentDir>/extensions`（用户全局）

显式配置路径（settings `extensions`、`--extension`、包内 `kiana.extensions`）会另行加入；命令行 `-e` 的临时来源优先于自动发现结果。

发现规则：

- 只下探一层：目录自身可以是扩展包（`package.json` 的 `kiana.extensions` → `index.ts` → `index.js`）；否则扫描直接子项中的 `.ts`/`.js` 文件，以及子目录里的 `package.json`/`index.ts`/`index.js` 入口。
- `.mjs`、`.cjs`、`.tsx` 不被识别；扩展入口名大小写敏感。
- 同一入口按解析后的绝对路径去重，保留首次出现的顺序；扩展加载失败只影响自身，其它扩展继续加载。

### CLI flag

| Flag | 作用 |
| --- | --- |
| `--extension, -e <path>` | 追加扩展文件、目录或 `builtin:<name>`；可多次。`--help` 只写了“扩展文件或 `builtin:<name>`”；传入 `npm:`/`git:` 等非本地源时会被原样交给包解析器（未在 `--help` 中列出） |
| `--no-extensions, -ne` | 关闭扩展发现与内置扩展；显式 `-e` 仍生效 |
| `--no-mcp` | 关闭内置 MCP 支持 |
| `--skill <path>` | 追加技能文件/目录；可多次 |
| `--no-skills, -ns` | 关闭技能发现与 settings 合并；显式 `--skill` 仍生效 |
| `--prompt-template <path>` | 追加 prompt 模板文件/目录；可多次 |
| `--no-prompt-templates, -np` | 关闭 prompt 模板发现与 settings 合并；显式路径仍生效 |
| `--theme <path>` | 追加主题文件/目录；可多次 |
| `--use-theme <name[/name]>` | 本次交互运行的初始主题 |
| `--no-themes` | 关闭主题发现与 settings 合并；显式 `--theme` 与内置主题仍生效 |

### 包与扩展的资源

包可以在 `package.json` 的 `kiana` 字段声明资源目录：

```json
{
  "name": "my-kiana-package",
  "kiana": {
    "extensions": ["./extensions"],
    "skills": ["./skills"],
    "prompts": ["./prompts"],
    "themes": ["./themes"]
  }
}
```

各字段是字符串数组；字段缺失或不是字符串数组时按“没有声明”处理，继续尝试约定的目录布局。扩展还可以处理 `resources_discover` 事件并返回技能、prompt 模板和主题路径，会话会把这些路径并入资源加载；`/reload` 会重新触发该事件。

### 网络与离线

| 变量/Flag | 作用 |
| --- | --- |
| `KIANA_OFFLINE=1` / `--offline` | 关闭启动期网络操作。离线时包解析不会自动安装缺失包；更新检查与更新直接跳过；已安装包的资源照常加载 |
| `KIANA_API_BASE_URL` | Kiana 自建服务根地址；模型目录、版本检查等派生端点都基于它 |
| `KIANA_INSTALLER_API_BASE` | 托管更新的 release API；默认 `KIANA_API_BASE_URL + /api/installer/releases` |
| `KIANA_PACKAGE_DIR` | 覆盖 package 目录（Nix/Guix store 等场景）；未设置时由可执行文件位置推断 |

## 限制与排查

### Skills

- `context: fork`、技能激活 `hooks`、`model` / `effort` 覆盖当前只解析、不生效（当前实现/限制）；不要依赖这些字段改变运行行为。
- `allowed-tools` 只产生 Allow 规则，且只在激活存活期间有效；`Deny`、只读模式和灾难性命令检查始终优先。
- 激活是内存状态，不会持久化；会话结束、显式停用或 TTL 到期后重新回到正常权限判断。
- 同名技能冲突只保留先加载者；资源加载器的逐文件诊断当前没有专门的 TUI 面板，排查时请直接检查文件与 frontmatter。
- `--no-skills` 不影响显式 `--skill`；如果技能“消失”，先确认它是否只存在于被关闭的自动发现目录或 settings 数组里。

### Prompt 模板

- 模板名精确、大小写敏感，不支持命名空间或子目录名；未命中时原样作为普通消息发送，不会报“未知命令”。
- 参数没有反斜杠转义；引号未闭合时剩余内容仍作为当前参数。
- 目录不递归扫描；嵌套目录里的模板需要单独加入 `--prompt-template` 或 `prompts` 数组。
- `/reload` 在流式响应或压缩期间会被拒绝；等当前响应结束后再试。

### Themes

- 主题名不能包含 `/`；`light/dark` 形式只用于自动选择。
- 只加载 `.json` 文件；目录不递归；非 `.json` 路径会产生 `theme path is not a json file` 诊断。
- 非法主题在启动期静默回退到 `system`；显式在设置里指定的非法主题会报错并同样回退。
- `--use-theme` 只影响交互模式，不会写入 settings。

### 扩展与包

- 扩展发现只下探一层，且只认 `.ts`/`.js`；`.mjs`/`.cjs`/`.tsx` 需要改名或提供 `index.ts`/`index.js`。
- 用户扩展是 TS/JS 文件，不是可加载的原生二进制；加载失败只影响该扩展，错误会出现在启动诊断里。
- `kiana install` 的 npm/git 源需要网络；本地路径必须存在。`-l` 写项目 settings 前会检查项目信任，未受信时提示 `Project is not trusted. Use --approve to modify local package config.`
- `kiana update` 会跳过固定版本的 npm 包；固定 ref 的 git 包仍参与更新以对齐 checkout。
- 离线模式下不会自动安装缺失包，也不会检查/执行更新。
- `kiana config` 是终端 TUI；需要在可交互终端中运行。
- 当前只在 Linux x86_64 上验证；其它平台的路径、终端主题与安装布局未实测。

## 相关页面

- [Hooks（钩子）](hooks.md)：在技能/扩展生命周期之外挂接工具、会话与压缩事件。
- [权限与审批](permissions.md)：`allowed-tools`、项目信任与审批规则。
