//! declarations.ts：把 JSON Schema 渲染成模型可见的 TypeScript 声明。
//!
//! TS 源：`packages/codemode/src/declarations.ts`（340 行）。逐字移植：这里输出的每个字节都会
//! 进入模型上下文（spec `agent-codemode.md` §3.5、§8.2 第 10 条），因此排版、排序、tag 顺序
//! 与转义全部按 TS 复刻，并用 TS 参照实现生成的黄金样例锁定（`tests/fixtures/declarations/`）。
//!
//! # 输出形状
//!
//! ```text
//! declare const tools: {
//!   /** Read a file. */
//!   read(args: { path: string; }): Promise<string>;
//! };
//!
//! /** List models. */
//! declare const models: {
//!   list(type: string): Promise<string[]>;
//! };
//! ```
//!
//! 节之间用一个空行（`"\n\n"`）分隔；没有 tools 也没有 globals 时输出空串。
//!
//! # JS 语义落点（ARCHITECTURE.md §6 / spec `agent-codemode.md` §6.1 第 1、2、9、10 条）
//!
//! - **UTF-16 长度**：`maxChars` 预算与 `MAX_TAG_CHARS = 80` 都按 UTF-16 码元计（[`js_length`]），
//!   不是 UTF-8 字节数。
//! - **UTF-16 排序**：`Object.keys(properties).sort()` 是默认字符串排序，即按 UTF-16 码元字典序
//!   （`cmp_utf16`）；Rust 的 `str::cmp` 是 UTF-8 字节序，对非 BMP 字符会给出不同顺序。
//!   `Object.keys` 自身的整数键优先序不影响结果，因为随后整体排序。
//! - **JS 数字文本**：`@minimum 1`、`@default 1e+21` 里的数字走 [`kiana_json::format_js_number`]
//!   （`Number::toString`）；`JSON.stringify` 的形状（`const`/`enum`/`@default`）走
//!   [`kiana_json::js_object::stringify`]（键序按 JS 规则、数字按 JS 文本）。
//! - **`String.prototype.trim`**：`description.trim()`、`line.trim()` 用
//!   [`kiana_json::js_string::js_trim`]（JS 的空白集合：含 U+FEFF，不含 U+0085）。
//! - **`new Set` 的 SameValueZero 去重**：`union` 按首次出现去重；`constraintTags` 的非边界 tag
//!   也按值去重（数字按 f64 值，`-0` 与 `0` 相同）。
//! - **`JSON.stringify("…")`** 即 [`kiana_json::js_string::js_quote`]：控制字符写 `\uXXXX`，
//!   U+2028/U+2029 原样保留（随后 tag 级转义再把它们变成 6 个字符）。
//!
//! # 已知差异
//!
//! - **`decodeURIComponent` 的畸形百分号转义**：TS 抛 `URIError: URI malformed`，且
//!   `schemaToType` 没有错误通道（异常直接穿过 `renderDeclarations` 冒泡给调用方，**可以被
//!   调用方 try/catch**）。冻结的 Rust 签名返回 `String`，与 `kiana-json` 的同类先例
//!   （`to_radix_string` 对越界 radix）一致：这里 `panic!("URI malformed")` 而不是静默当作
//!   `unknown`（planner 要求：不要静默吞掉）。与 TS 的差别是 Rust 的 panic 不能被调用点恢复，
//!   且在 `panic = "abort"` 的构建里直接终止进程；`$ref` 完全由模型/服务端控制，所以这是
//!   一条**面向不可信输入**的路径。彻底消解需要把签名改成 `Result`（§4.9 冻结契约变更），
//!   已上报架构者。畸形 ref 只可能来自手工构造或恶意的 schema；正常工具 schema 不可达。
//!   **抛错的时机**是稳定契约：每段先 `decodeURIComponent` 再判断「段是否存在」，而「是否
//!   存在」用 JS 的 `key in current`（走原型链，见 `resolve_ref`）。
//! - **递归深度**：TS 的 `toType` 是普通递归，超深嵌套抛**可捕获**的 `RangeError`。阈值完全由
//!   V8 的栈决定，不是常量：本机 Node 26 默认栈上逐层扫描的首个失败深度是对象 3295 层、数组
//!   11385 层、`anyOf` 6956 层（`--stack-size=2000` 时约翻倍）。Rust 的递归在栈耗尽时是
//!   不可捕获的 `fatal runtime error: stack overflow, aborting`，所以这里用 `MAX_TYPE_DEPTH`
//!   = 128 显式限深，超限部分渲染 `unknown` 而不是 abort。差别：≥128 层的 schema TS 能完整渲染
//!   （到数千层才抛），这里从第 128 层起是 `unknown`；≤127 层由黄金样例逐字节锁定，真实
//!   工具 schema（黄金样例中最深 11 层）达不到。`$ref` 另有 32 次展开上限与环检测。
//! - **非有限数字（`Infinity`）**：`JSON.parse("1e400")` 在 V8 里是 `Infinity`（`typeof` 为
//!   `"number"`），而 `kiana_json::parse` 按 ARCHITECTURE.md §8 第 32 条把非有限数存成
//!   `Value::Null`，且 `Null` 与真正的 JSON `null` 在 `Value` 里不可区分。两处 TS 分支依赖
//!   「是不是 number / 是不是 null」，所以这里**都会**偏离：
//!
//!   1. **数值类 tag 关键字**（`TAG_KEYWORDS` 里取值走 `typeof value === "number"` 的那 11 个，
//!      以及 `LOWER_BOUNDS`/`UPPER_BOUNDS` 的取 max/min）：TS 渲染 `@minimum Infinity`、
//!      `@multipleOf Infinity`、`@pattern Infinity` 等；这里因为值是 `null`，该 tag 整条消失
//!      （不是渲染成 `@minimum null`）。注意 `@integer` 只看 `type`，不受影响。
//!   2. **`@default` 本身是顶层非有限数**（`{"default": 1e400}`）：TS 的守卫
//!      `default !== undefined && default !== null` 对 `Infinity` 为**真**，于是渲染
//!      `@default null`（`JSON.stringify(Infinity) === "null"`）；这里看到的是 `Value::Null`，
//!      守卫为假，**整条 tag 消失**。`const`、`enum`、以及**嵌套**在默认值里的非有限数不受
//!      影响：`JSON.stringify` 只会把非有限数写成 `null`，两边文本一致
//!      （`{"const":1e400}` → `null`；`default: {"v":1e400}` → `@default {"v":null}`）。
//!
//!   不能在本层修：把 `null` 当作 `Infinity` 会同时让真正的 `"default": null` /
//!   `"minimum": null` 产生 tag，而 TS 对它们是**抑制**的（前者极常见，如 Pydantic 的
//!   `Field(None)`），净效果更差。消除它需要 `kiana-json` 换数字表示（§4.9 契约变更），
//!   不属于本单元；已上报架构者。
//! - **`undefined` 与 JSON 值**：`CodemodeJsonSchema` 是 [`serde_json::Value`]，没有 `undefined`。
//!   TS 里 `{const: undefined}` 渲染 `unknown`、`default: undefined` 不出 tag；Rust 侧对应的
//!   「键缺失」形态与之等价（`const` 键缺失 → 走后面的分支；`default` 键缺失 → 无 tag），
//!   而 `JSON.stringify(undefined)` 的 `undefined` 结果在 Rust 里不可达。
//!
//! # 在系统里的位置
//!
//! 本仓库里直接读到的调用方：
//! - `kiana-builtin-extensions` 的 `codemode` 工具：`codemode/tool.rs` 用
//!   [`render_tool_sample`] 生成 code mode 目录里每个工具的声明段、用
//!   [`render_tool_output_type`] 概括一次调用解析成什么；`codemode/execute.rs` 用
//!   [`render_tool_sample`] 生成 `ALL_TOOLS` 的样例（作为沙箱工具的 `description`）。
//! - `kiana-durable` 的 `codemode/describe.rs`：`sample_of` 用 [`render_tool_sample`] 生成
//!   code mode 提示段里的工具样例。
//! - [`render_declarations`] 生成整块声明；在本仓库的 Rust 生产代码里目前没有调用点，
//!   由 `tests/declarations.rs` 的黄金样例逐字节校验。
//!
//! # 一次渲染的流程
//!
//! 1. [`schema_to_type`] 新建 `SchemaContext`（引用解析根、当前展开路径、展开计数、深度、
//!    是否输出 tag），再调用 `to_type` 按固定分支顺序递归；对象与数组分支把子 schema
//!    继续交给同一条递归。
//! 2. 有字符预算时渲染两遍：第一遍带 tag；超过预算则**不带 tag** 重渲染；仍超长返回
//!    `unknown`。两遍各自新建上下文，展开计数与深度互不继承。
//! 3. [`render_declarations`] 组装文本：tools 一节、每个顶层 global 一条 `declare function`、
//!    每个命名空间一节，节间用空行连接。
//!
//! # 使用注意
//!
//! - 输出会进入模型上下文，排版、排序与 tag 顺序都是逐字节冻结的契约，改动必须同步
//!   `tests/fixtures/declarations/` 的 TS 参照黄金样例。
//! - 这些函数只返回 `String`，没有错误通道；本模块**显式触发**的唯一 panic 是 `$ref`
//!   畸形百分号转义（见上）。
//! - `MAX_TAG_CHARS` 在行终止符转义**之前**测量：转义会把一个字符变成 6 个字符，所以最终
//!   输出行可能超过 80 个 UTF-16 码元。黄金样例里有含 `\r`/`\n`/U+2028 的 tag
//!   （`schema_cases.json`），转义后的文本由黄金样例逐字锁定。

use std::cmp::Ordering;

use indexmap::{IndexMap, IndexSet};
use serde_json::Map;
use serde_json::Value;

/// `declarations.ts` 的公开再导出：把工具名转成脚本标识符（实现见 `identifier` 模块）。
///
/// TS 的 `declarations.ts` 同时 import 并**再导出** `toCodemodeIdentifier`：
/// `export { toCodemodeIdentifier };`（spec §2.2.4 的导出清单、§3.3 的「主入口 + /declarations + /portable」）。
/// 因此这里是 `pub use` 而不是普通 `use`：`kiana_codemode::declarations::to_codemode_identifier` 必须可用。
pub use crate::identifier::to_codemode_identifier;
use crate::runtime::prelude_source::js_length;
use crate::types::{CodemodeJsonSchema, CodemodeTool};

/// 每级缩进（TS: `INDENT = "  "`）。
const INDENT: &str = "  ";
/// 渲染出的输入类型的最大 UTF-16 长度；超过先丢 tag，再不行渲染为 `unknown`。
///
/// [`render_tool_signature`] 的 `input_max_chars` 为 `None` 时用它作为默认预算。
///
/// TS: `declarations.ts#DEFAULT_INPUT_SCHEMA_MAX_CHARS`。
pub const DEFAULT_INPUT_SCHEMA_MAX_CHARS: usize = 16_000;
/// 每个被渲染的 schema 允许的本地 `$ref` 展开次数，防止共享定义撑爆输出。
const MAX_REF_EXPANSIONS: usize = 32;
/// 以 `@keyword value` 形式出现在属性注释里的 JSON Schema 关键字（顺序即输出顺序）。
const TAG_KEYWORDS: [&str; 11] = [
    "minimum",
    "exclusiveMinimum",
    "maximum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "pattern",
    "format",
    "minItems",
    "maxItems",
];
/// 取值全部同时成立的**下界**类关键字：多个分支时只有最大值有意义。
const LOWER_BOUNDS: [&str; 4] = ["minimum", "exclusiveMinimum", "minLength", "minItems"];
/// 取值全部同时成立的**上界**类关键字：多个分支时只有最小值有意义。
const UPPER_BOUNDS: [&str; 4] = ["maximum", "exclusiveMaximum", "maxLength", "maxItems"];
/// 单个属性 tag 的最大长度（UTF-16 码元）；更长（大默认值、长 pattern）的整条省略。
const MAX_TAG_CHARS: usize = 80;
/// 渲染类型时允许的 schema 嵌套层数；更深的层渲染成 `unknown`。
///
/// TS 的 `toType` 是普通递归，超深 schema 抛**可捕获**的 `RangeError: Maximum call stack size
/// exceeded`，阈值只取决于 V8 的栈而不是 schema：本机 Node 26 默认栈上逐层扫描的首个失败深度是
/// 对象 3295 层、数组 11385 层、`anyOf` 6956 层（`--stack-size=2000` 时约翻倍）。Rust 侧同样的
/// 递归在栈耗尽时是 `fatal runtime error: stack overflow, aborting`，**不可捕获**，会杀掉整个
/// 进程——比 TS 的异常严重得多。因此这里显式限深，超过就渲染 `unknown`（与
/// `kiana-telemetry/src/jsjson.rs` 的 `MAX_DEPTH = 128` 取同一数值，理由相同：防栈爆）。
///
/// 实测（debug 构建、2 MiB 线程栈——tokio worker 与 libtest 的默认值）：最耗栈的形状
/// （`{type:"object",properties:{a:…}}` 链）在 595 层 schema 时 abort；本上限 128 用掉约
/// 512 KiB（每层约 4 KiB），留 4 倍余量。release 构建每层栈开销约为 debug 的 1/3，余量更大。
///
/// 已知偏差：嵌套 ≥128 层的 schema，TS 能完整渲染（直到 ~1400 层才抛 `RangeError`），
/// 这里从第 128 层起渲染 `unknown`。≤127 层由黄金样例逐字节锁定；真实工具 schema
/// （本 crate 的黄金样例中最深 11 层）远达不到。
const MAX_TYPE_DEPTH: usize = 128;

/// `renderDeclarations(options)` 的选项。
///
/// TS 的字段是可选的（`tools?: readonly CodemodeTool[]`），缺省按空数组处理；Rust 侧用空切片
/// 表达同一语义（[`RenderDeclarationsOptions::default`]）。
///
/// TS: `declarations.ts#RenderDeclarationsOptions`。
#[derive(Debug, Clone, Copy, Default)]
pub struct RenderDeclarationsOptions<'a> {
    /// 成为 `declare const tools` 成员的脚本可见工具。
    pub tools: &'a [CodemodeTool],
    /// 成为 `declare function …` 或 `declare const <ns>` 成员的全局函数。
    pub globals: &'a [CodemodeTool],
}

impl<'a> RenderDeclarationsOptions<'a> {
    /// 只给 tools 的选项（globals 为空）。
    pub fn tools(tools: &'a [CodemodeTool]) -> Self {
        RenderDeclarationsOptions {
            tools,
            globals: &[],
        }
    }

    /// 只给 globals 的选项（tools 为空）。
    pub fn globals(globals: &'a [CodemodeTool]) -> Self {
        RenderDeclarationsOptions {
            tools: &[],
            globals,
        }
    }
}

/// `renderDeclarations({tools, globals})`：渲染脚本可见 API 的 TypeScript 声明。
///
/// tools 成为 `declare const tools` 的成员，globals 成为 `declare function` 语句，`ns.member`
/// 形式的 global 归入 `declare const ns`。描述成为 doc comment，schema 成为类型。节之间用空行
/// 分隔；输入为空时返回空串。
///
/// 输出顺序固定：先 tools 节（若有），再按输入顺序输出所有顶层 `declare function`，最后按
/// 命名空间**首次出现**的顺序输出各 `declare const <ns>` 节。因此即使输入里顶层 global 与
/// `ns.member` 交错，顶层函数也总排在命名空间节之前；同一命名空间的成员合并到一个对象类型。
///
/// 名字以 `.` 开头时，命名空间部分是空串，输出 `declare const : {`，不是合法 TypeScript。
/// 名字以 `.` 结尾时命名空间正常、成员名是空串：带 `signature` 时成员就是该签名，实际输出是
/// 合法的调用签名类型 `declare const trailing: {\n  (): void;\n};`（多行，不是单行；
/// `declaration_cases.json` 的 “namespace first-dot grouping and insertion order” 逐字节锁定）。
/// 这两种形态由黄金样例逐字节锁定，是冻结行为，不要“顺手修复”。
///
/// # Panics
///
/// 工具 schema 的 `$ref` 含畸形百分号转义时 panic `"URI malformed"`，穿过本函数冒泡给调用方
/// （TS 侧是同样穿透的 `URIError`）。详见 [`schema_to_type`]。
///
/// TS: `declarations.ts#renderDeclarations`。
pub fn render_declarations(options: RenderDeclarationsOptions<'_>) -> String {
    let mut sections: Vec<String> = Vec::new();
    // tools 节先输出：所有工具共用同一个对象类型，每个成员是“缩进的 doc comment + 签名”。
    if !options.tools.is_empty() {
        let members: Vec<String> = options
            .tools
            .iter()
            .map(|tool| {
                format!(
                    "{}{INDENT}{}",
                    doc_comment(tool.description.as_deref(), INDENT),
                    render_tool_signature(tool, None)
                )
            })
            .collect();
        sections.push(format!(
            "declare const tools: {{\n{}\n}};",
            members.join("\n")
        ));
    }
    // globals 在**同一次遍历**里分流：无点号的名字立即成为顶层 `declare function`；
    // 带点号的按第一个点切成命名空间 + 成员。顶层函数因此总排在所有命名空间节之前。
    // 命名空间按首次出现的顺序保留（TS 的 `Map` 语义），成员保持 globals 的输入顺序。
    let mut namespaces: IndexMap<String, Vec<String>> = IndexMap::new();
    for global in options.globals {
        match global.name.find('.') {
            None => sections.push(render_global(
                &format!("declare function {}", global.name),
                global,
                "",
            )),
            Some(dot) => {
                // 只按**第一个**点切分：`a.b.c` 的命名空间是 `a`，成员名保留 `b.c`。
                let namespace = &global.name[..dot];
                let member = render_global(&global.name[dot + 1..], global, INDENT);
                namespaces
                    .entry(namespace.to_string())
                    .or_default()
                    .push(member);
            }
        }
    }
    // 每个命名空间一个 `declare const <ns>` 对象类型；成员已带 INDENT 缩进。
    for (namespace, members) in &namespaces {
        sections.push(format!(
            "declare const {namespace}: {{\n{}\n}};",
            members.join("\n")
        ));
    }
    // 节之间恰好一个空行；没有节时 join 得到空串。
    sections.join("\n\n")
}

/// 一个工具作为 `tools` 对象的成员：`name(args: T): Promise<R>;`，名字是脚本使用的标识符。
///
/// 输入类型超过 `input_max_chars`（缺省 [`DEFAULT_INPUT_SCHEMA_MAX_CHARS`]）时渲染为 `unknown`。
///
/// - `input_max_chars` 是 `Option` 而不是 0 哨兵：`Some(0)` 表示显式的零预算，只有 `None`
///   才回退到默认值（`explicit_zero_input_max_chars_is_honoured`）。
/// - `tool.signature` 被忽略：该字段只对 globals 生效
///   （`global_signatures_win_and_tool_signatures_are_ignored`）。
/// - 输出类型不受字符预算限制；但仍受 `MAX_TYPE_DEPTH` 与 `MAX_REF_EXPANSIONS` 约束，
///   超限部分渲染为 `unknown`（输出类型走 [`render_tool_output_type`]，即
///   `schema_to_type(schema, None)`）。
///
/// # Panics
///
/// 同 [`schema_to_type`]：schema 的 `$ref` 含畸形百分号转义时 panic `"URI malformed"`。
///
/// TS: `declarations.ts#renderToolSignature`。
pub fn render_tool_signature(tool: &CodemodeTool, input_max_chars: Option<usize>) -> String {
    let input = match &tool.input_schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(
            schema,
            Some(input_max_chars.unwrap_or(DEFAULT_INPUT_SCHEMA_MAX_CHARS)),
        ),
    };
    format!(
        "{}(args: {input}): Promise<{}>;",
        to_codemode_identifier(&tool.name),
        render_tool_output_type(tool.output_schema.as_ref())
    )
}

/// 一个工具的样例：描述 + 工具声明。用于工具列表与 `ALL_TOOLS` 条目。
///
/// 描述先做 JS `trim` 再原样插入，**不转义**（这条路径不是 JSDoc）。描述缺失或全为空白时，
/// 返回值以两个换行开头；调用方若需要干净文本要自己再 trim
/// （`kiana-builtin-extensions` 的 `render_tool_section` 就是先 trim 再拼接）。
///
/// # Panics
///
/// 同 [`schema_to_type`]：schema 的 `$ref` 含畸形百分号转义时 panic `"URI malformed"`。
///
/// TS: `declarations.ts#renderToolSample`。
pub fn render_tool_sample(tool: &CodemodeTool, input_max_chars: Option<usize>) -> String {
    let declaration = format!(
        "declare const tools: {{ {} }};",
        render_tool_signature(tool, input_max_chars)
    );
    let description = tool
        .description
        .as_deref()
        .map(kiana_json::js_string::js_trim)
        .unwrap_or("");
    format!("{description}\n\ncodemode tool declaration:\n```ts\n{declaration}\n```")
}

/// 一次工具调用解析出的类型：输出 schema 的类型；没有 schema 时是 `unknown`。
///
/// 不做字符预算：没有 `max_chars` 参数，本函数走 `schema_to_type(schema, None)`。
/// 但仍受 `MAX_TYPE_DEPTH` 与 `MAX_REF_EXPANSIONS` 约束，超限部分渲染为 `unknown`。
///
/// TS: `declarations.ts#renderToolOutputType`。
pub fn render_tool_output_type(schema: Option<&CodemodeJsonSchema>) -> String {
    match schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(schema, None),
    }
}

/// `renderGlobal(head, global, indent)`：一个全局函数/命名空间成员的声明体。
///
/// `global.signature` 存在时按原样拼接并补 `;`，调用方要传不含结尾分号的
/// “参数表 + 返回类型”；否则用 input/output schema 生成 `(args: T): Promise<R>`。
fn render_global(head: &str, global: &CodemodeTool, indent: &str) -> String {
    let doc = doc_comment(global.description.as_deref(), indent);
    if let Some(signature) = &global.signature {
        return format!("{doc}{indent}{head}{signature};");
    }
    let input = match &global.input_schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(schema, None),
    };
    let output = match &global.output_schema {
        None => "unknown".to_string(),
        Some(schema) => schema_to_type(schema, None),
    };
    format!("{doc}{indent}{head}(args: {input}): Promise<{output}>;")
}

/// `docComment(description, indent)`：把描述渲染成 JSDoc 注释（含结尾换行）。
///
/// 空描述 → 空串；`*/` 写成 `*\/`（反斜杠保留）；按 `/\r?\n/` 分行：单行写成
/// `/** text *\/`，多行写成 `/**` / ` * line` / ` *\/`。
///
/// 转义 `*/` 是必要的：描述来自工具，未转义的 `*/` 会提前闭合 JSDoc，
/// 描述的后半段就变成声明体里的裸文本。
fn doc_comment(description: Option<&str>, indent: &str) -> String {
    let Some(description) = description else {
        return String::new();
    };
    let text = kiana_json::js_string::js_trim(description);
    if text.is_empty() {
        return String::new();
    }
    let text = text.replace("*/", "*\\/");
    let lines = split_crlf(&text);
    if lines.len() == 1 {
        return format!("{indent}/** {} */\n", lines[0]);
    }
    let mut out = format!("{indent}/**\n");
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str(" *");
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
    }
    out.push('\n');
    out.push_str(indent);
    out.push_str(" */\n");
    out
}

/// `text.split(/\r?\n/)`：CRLF 与单个 LF 都是分隔符，**裸 CR 不是**（`"abc\r"` 切出 `["abc\r"]`）。
fn split_crlf(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(index) = rest.find('\n') {
        let piece = &rest[..index];
        // 只有紧挨着 LF 的那个 CR 属于分隔符；行尾孤立的 CR 保留。
        parts.push(piece.strip_suffix('\r').unwrap_or(piece));
        rest = &rest[index + 1..];
    }
    parts.push(rest);
    parts
}

/// `propertyKey(name)`：合法标识符原样，否则写成 JSON 字符串字面量。
fn property_key(name: &str) -> String {
    if is_identifier(name) {
        name.to_string()
    } else {
        kiana_json::js_string::js_quote(name)
    }
}

/// TS 的 `IDENTIFIER = /^[A-Za-z_$][A-Za-z0-9_$]*$/`。
///
/// JS 的 `$` 只在输入末尾匹配（无 `m` 标志时也不容忍结尾换行，见 `declarations.test.ts` 之外的
/// 实测），因此 `"abc\n"` 不匹配、会被引号包起来。
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

/// `union(types)`：按首次出现去重；含 `unknown` → `unknown`；空 → `never`；否则 `" | "` 连接。
///
/// TS 是 `[...new Set(types)]`：插入序去重，然后 `includes("unknown")` 毒化整个联合。
fn union(types: Vec<String>) -> String {
    let unique: IndexSet<String> = types.into_iter().collect();
    if unique.contains("unknown") {
        return "unknown".to_string();
    }
    if unique.is_empty() {
        return "never".to_string();
    }
    unique.into_iter().collect::<Vec<_>>().join(" | ")
}

/// `schemaToType(schema, {maxChars})`：把 JSON Schema 转成 TypeScript 类型表达式。
///
/// 对象在单行（`{ a: string; b?: number; }`），属性按名字（UTF-16 码元序）排序；属性有描述或类型
/// 表达不了的约束（注释形如 `// @integer @minimum 1 @default 20`）时改成每属性一行；数组写成
/// `Array<T>`。本地引用（`#/$defs/...`、`#/definitions/...`）相对 `schema` 解析；递归与远程引用
/// 渲染为 `unknown`。结果超过 `max_chars` 时**不带 tag** 再渲染一次，仍超长则 `unknown`。
///
/// 形如 `#/$defs/a~1b` 的转义段先用 `decodeURIComponent` 解码再把 `~1`→`/`、`~0`→`~`；
/// 百分号转义畸形时与 TS 一样中止（见模块文档的「已知差异」）。每段都**先解码、再查是否存在**
/// （`key in current` 走原型链），所以 `#/toString/%ZZ` 这类引用同样抛错。
///
/// # Panics
///
/// `$ref` 里出现畸形的百分号转义（如 `%ZZ`、`%E0%A4%A`、孤立代理项 `%ED%A0%80`）时 panic
/// `"URI malformed"`，对应 TS 的 `URIError: URI malformed`。
///
/// **这是与 TS 的已知差异**：TS 的异常可以被调用方 `try/catch`（`schemaToType` 与
/// `renderDeclarations` 都没有错误通道，异常直接冒泡），Rust 的 panic 不能被调用点恢复；若
/// 将来把 release profile 设成 `panic = "abort"`，它会直接终止进程。`$ref` 来自工具 schema，
/// 也就是模型/服务端可控的输入，所以调用方若要防御这类输入，需要自己包 `catch_unwind`。
/// 彻底消解需要把签名改成 `Result`（§4.9 冻结契约变更），已上报架构者。
///
/// TS: `declarations.ts#schemaToType`。
pub fn schema_to_type(schema: &CodemodeJsonSchema, max_chars: Option<usize>) -> String {
    let render = |tags: bool| {
        let mut context = SchemaContext {
            root: schema,
            resolving: Vec::new(),
            expansions: 0,
            tags,
            depth: 0,
        };
        to_type(schema, &mut context)
    };
    let typed = render(true);
    let Some(max_chars) = max_chars else {
        return typed;
    };
    // 预算按 UTF-16 码元比较，且是闭区间（正好等于预算算通过）。
    if js_length(&typed) <= max_chars {
        return typed;
    }
    // 超预算时先丢掉 tag（tag 只会往类型里追加文本），仍超长才退化为 `unknown`。
    // 第二遍同样从空上下文开始，引用展开次数与深度都会重新计数。
    let untagged = render(false);
    if js_length(&untagged) <= max_chars {
        untagged
    } else {
        "unknown".to_string()
    }
}

/// `toType` 的上下文：引用解析的根、当前路径上正在展开的 ref、已展开次数、是否输出 tag。
struct SchemaContext<'a> {
    root: &'a CodemodeJsonSchema,
    /// 当前路径上正在展开的引用（TS 的 `Set<string>`），用于在递归类型处停下。
    resolving: Vec<String>,
    /// 本次渲染已成功展开的 `$ref` 次数。整次渲染共享，不随分支回退；
    /// 达到 `MAX_REF_EXPANSIONS` 后剩下的引用都渲染为 `unknown`。
    expansions: usize,
    /// 属性注释是否包含约束 tag。
    tags: bool,
    /// 当前递归深度（TS 靠 JS 栈自身的上限，见 `MAX_TYPE_DEPTH`）。
    depth: usize,
}

/// `resolveRef(ref, root)`：解析 `#` 或 `#/…` 形式的本地引用。
fn resolve_ref<'a>(reference: &str, root: &'a CodemodeJsonSchema) -> Option<&'a Value> {
    if reference != "#" && !reference.starts_with("#/") {
        return None;
    }
    let mut current = RefNode::Value(root);
    // `ref.slice(2)`：只有 `"#"` 或 `"#/…"` 能到这里，两者都是 ASCII 前缀，切点必在码点边界。
    let rest = if reference == "#" {
        ""
    } else {
        &reference[2..]
    };
    // 空段被过滤：`#`、`#/` 以及 `#//a` 里多余的斜杠都不产生下钻段。
    for segment in rest.split('/').filter(|segment| !segment.is_empty()) {
        // TS 在类型检查**之前**就 `decodeURIComponent(segment)`，所以即使是「键不存在」或
        // 「上一段已不是对象」的段，畸形转义照样抛 `URIError`。
        // 先解码百分号转义、再做 JSON Pointer 的 `~1`/`~0` 反转义；顺序固定，所以 `~01`
        // 先变成 `~1`（不会被再次当成 `/`）。
        let key = decode_uri_component(segment)
            .replace("~1", "/")
            .replace("~0", "~");
        current = current.child(&key)?;
    }
    match current {
        RefNode::Value(value @ (Value::Bool(_) | Value::Object(_))) => Some(value),
        // 函数、`Object.prototype` 与 JS 的 `null` 在 TS 里同样渲染 `unknown`（`toType` 从它们
        // 身上读不到任何认得的键），所以统一按未命中处理。
        RefNode::Value(_) | RefNode::Function | RefNode::ObjectPrototype | RefNode::JsNull => None,
    }
}

/// `resolveRef` 逐段下钻时的节点。
///
/// TS 的 `key in current` 走**原型链**：`JSON.parse` 出来的对象都继承 `Object.prototype`，
/// 所以 `"toString" in obj` 为真、`obj["toString"]` 是引擎内置函数。函数不是普通对象
/// （`typeof` 为 `"function"`），任何后续段都会返回 `undefined`——但后续段仍会**先解码**，
/// 因此 `#/toString/%ZZ` 在 TS 里照样抛 `URIError`。Rust 若只用 `map.get` 就会在这里提前
/// 返回 `unknown`，漏掉那次抛错。
///
/// `Object.prototype.__proto__` 是访问器，读出来是 JS 的 `null`（而**不是** `undefined`）：
/// 所以 `#/__proto__/__proto__` 会走到 `null` 再停，而 `#/__proto__/__proto__/%ZZ` 里那个畸形段
/// 仍然先被解码并抛错。[`RefNode::JsNull`] 存在的唯一理由就是让下一次循环能把那一段解码。
enum RefNode<'a> {
    /// JSON 值（JSON.parse 出来的普通对象，或布尔/数组/原始值）。
    Value(&'a Value),
    /// `Object.prototype` 的成员函数。
    Function,
    /// `obj.__proto__` 取到的 `Object.prototype` 本身（一个普通对象）。
    ObjectPrototype,
    /// `Object.prototype.__proto__` 就是 JS 的 `null`。
    JsNull,
}

/// `Object.prototype` 的自有属性名（V8 的 12 个）。对普通对象 `key in obj` 恒为真。
const OBJECT_PROTOTYPE_MEMBERS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// `key in Object.prototype`。
fn is_object_prototype_member(key: &str) -> bool {
    OBJECT_PROTOTYPE_MEMBERS.contains(&key)
}

impl<'a> RefNode<'a> {
    /// 取下一段对应的节点；`None` 对应 TS 的 `return undefined`。
    ///
    /// 前提条件对应 TS 的守卫 `typeof current !== "object" || current === null ||
    /// Array.isArray(current) || !(key in current)`。
    fn child(&self, key: &str) -> Option<RefNode<'a>> {
        match self {
            RefNode::Value(Value::Object(map)) => {
                if let Some(value) = map.get(key) {
                    return Some(RefNode::Value(value));
                }
                // 自有键没有命中时，`key in current` 仍可能经原型链为真。
                if !is_object_prototype_member(key) {
                    return None;
                }
                // `obj.__proto__` 是 `Object.prototype`，其余成员是函数。
                Some(if key == "__proto__" {
                    RefNode::ObjectPrototype
                } else {
                    RefNode::Function
                })
            }
            // 非对象 / null / 数组：TS 的三个前置条件都不满足。
            RefNode::Value(_) => None,
            // 函数：`typeof` 是 `"function"`，不是普通对象。
            RefNode::Function => None,
            // `Object.prototype` 自身：成员同样是函数，但 `Object.prototype.__proto__` 是 JS 的
            // `null`（走 [`RefNode::JsNull`] 而不是未命中），其余与普通对象一致。
            RefNode::ObjectPrototype => {
                if key == "__proto__" {
                    return Some(RefNode::JsNull);
                }
                is_object_prototype_member(key).then_some(RefNode::Function)
            }
            // `null`：`typeof current !== "object"` 为真（`typeof null === "object"`，但守卫里
            // 紧接着的 `current === null` 命中）。
            RefNode::JsNull => None,
        }
    }
}

/// `decodeURIComponent(text)`：百分号转义按 UTF-8 严格解码，畸形时抛 `URIError: URI malformed`
/// （Rust 侧是 `panic!`，见模块文档的「已知差异」）。
fn decode_uri_component(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    // 逐字节扫描：`%` 后必须跟两位十六进制，否则 panic（对应 TS 的 URIError）。
    // 非转义片段原样复制——`decodeURIComponent` 不把 `+` 当作空格。
    while index < bytes.len() {
        if bytes[index] != b'%' {
            // 非转义部分原样复制（`from_utf8` 之前它本身是合法 UTF-8）。
            let start = index;
            while index < bytes.len() && bytes[index] != b'%' {
                index += 1;
            }
            decoded.extend_from_slice(&bytes[start..index]);
            continue;
        }
        let hex = bytes.get(index + 1..index + 3);
        let Some([high, low]) = hex.and_then(|pair| <[u8; 2]>::try_from(pair).ok()) else {
            panic!("URI malformed");
        };
        let (Some(high), Some(low)) = (hex_value(high), hex_value(low)) else {
            panic!("URI malformed");
        };
        decoded.push(high << 4 | low);
        index += 3;
    }
    // 整段解码结果必须是合法 UTF-8：超长编码、截断序列与代理项编码（如 `%ED%A0%80`）
    // 都会被 from_utf8 拒绝并 panic。`malformed_percent_escapes_panic_like_uri_error`
    // 锁定这些形态与 TS 的 `URIError` 对应。
    match String::from_utf8(decoded) {
        Ok(text) => text,
        Err(_) => panic!("URI malformed"),
    }
}

/// 单个十六进制字符的数值（`decodeURIComponent` 接受两种大小写）。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `toType(schema, context)`：分支顺序即行为，不要重排（spec §2.2.4 的第 1–10 步）。
///
/// 唯一的额外行为是深度上限 `MAX_TYPE_DEPTH`：TS 在超深 schema 上抛 `RangeError`，Rust 的
/// 递归则会 abort 进程，所以超限处渲染 `unknown`（见该常量的文档）。
fn to_type(schema: &CodemodeJsonSchema, context: &mut SchemaContext<'_>) -> String {
    to_type_with_type(schema, None, context)
}

/// `toType` 的通用形：`type_override` 非空时用它代替 `schema.type`。
///
/// TS 在 `type` 是数组时递归 `toType({ ...schema, type: entry }, context)`——一次**浅**展开。
/// Rust 若照抄成 `map.clone()` 再改 `type`，`serde_json::Map` 的 `Clone` 是**深**拷贝（递归
/// 遍历整棵子树），在深度上限之前就会把栈打爆（实测 1 MiB 栈上 700 层的兄弟子树即溢出）。
/// 而 `{ ...schema, type }` 与 `schema` 只差 `type` 一个键，且 `toType` 在 `type` 之前的分支
/// （`$ref`/`const`/`enum`/`anyOf`/`oneOf`/`allOf`）都只看别的键，所以传一个 `type` 覆盖值
/// 与克隆整棵树完全等价。
fn to_type_with_type(
    schema: &CodemodeJsonSchema,
    type_override: Option<&Value>,
    context: &mut SchemaContext<'_>,
) -> String {
    if context.depth >= MAX_TYPE_DEPTH {
        return "unknown".to_string();
    }
    // 深度是**当前递归路径**的深度：进入时检查并 +1，子分支返回后 -1；
    // 这里不提前 return，保证计数一定配对（提前返回发生在 to_type_unchecked 内部）。
    context.depth += 1;
    let rendered = to_type_unchecked(schema, type_override, context);
    context.depth -= 1;
    rendered
}

/// [`to_type`] 的本体：进入时已检查并递增深度。
fn to_type_unchecked(
    schema: &CodemodeJsonSchema,
    type_override: Option<&Value>,
    context: &mut SchemaContext<'_>,
) -> String {
    let map = match schema {
        // 布尔 schema 是 JSON Schema 的两个哨兵：true = 任意值，false = 不可能值。
        Value::Bool(true) => return "unknown".to_string(),
        Value::Bool(false) => return "never".to_string(),
        Value::Object(map) => map,
        // 非对象 / null / 数组 → unknown（字符串、数字也在内）。
        Value::Null | Value::Array(_) | Value::Number(_) | Value::String(_) => {
            return "unknown".to_string();
        }
    };

    // 分派顺序就是行为契约，不要重排：`$ref` 优先于同一 schema 上的其它关键字。
    if let Some(Value::String(reference)) = map.get("$ref") {
        // 直接环（当前展开路径上已有同一引用）与展开次数上限都退化为 unknown；
        // 远程或解析不到的引用同样如此。
        if context.resolving.iter().any(|active| active == reference)
            || context.expansions >= MAX_REF_EXPANSIONS
        {
            return "unknown".to_string();
        }
        let Some(target) = resolve_ref(reference, context.root) else {
            return "unknown".to_string();
        };
        context.expansions += 1;
        context.resolving.push(reference.clone());
        let rendered = to_type(target, context);
        context.resolving.pop();
        return rendered;
    }

    // 字面量分支：const 覆盖 enum；`stringify` 的文本就是 TS 里的类型字面量。
    if let Some(value) = map.get("const") {
        return kiana_json::js_object::stringify(value);
    }
    // enum 只在是数组时生效；非数组的 enum 被忽略，落到后面的 type 分支。
    if let Some(Value::Array(values)) = map.get("enum") {
        return union(
            values
                .iter()
                .map(kiana_json::js_object::stringify)
                .collect::<Vec<_>>(),
        );
    }

    // anyOf 优先；anyOf 不是数组时再看 oneOf。变体各自递归后按首次出现去重。
    let variants = match map.get("anyOf") {
        Some(Value::Array(variants)) => Some(variants),
        _ => match map.get("oneOf") {
            Some(Value::Array(variants)) => Some(variants),
            _ => None,
        },
    };
    if let Some(variants) = variants {
        return union(variants.iter().map(|v| to_type(v, context)).collect());
    }
    // allOf 是交集：unknown 分支被丢弃，全部为 unknown 时整体 unknown；
    // 含 `" | "` 的成员要加括号，避免 `A & (B | C)` 被读成 `(A & B) | C`。
    if let Some(Value::Array(parts)) = map.get("allOf") {
        let parts: Vec<String> = parts
            .iter()
            .map(|part| to_type(part, context))
            .filter(|part| part != "unknown")
            .collect();
        if parts.is_empty() {
            return "unknown".to_string();
        }
        return parts
            .iter()
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" & ");
    }

    // 到这里才看 type；type 数组的每个条目通过 type_override 做一次浅展开，
    // 条目本身仍是数组时会再次进入这个分支。
    let type_value = type_override.or_else(|| map.get("type"));
    if let Some(Value::Array(entries)) = type_value {
        return union(
            entries
                .iter()
                .map(|entry| to_type_with_type(schema, Some(entry), context))
                .collect(),
        );
    }
    // 单个 type 关键字；无法识别的类型名以及非字符串的 type 都渲染 unknown。
    match type_value {
        Some(Value::String(kind)) => match kind.as_str() {
            "string" => "string".to_string(),
            "number" | "integer" => "number".to_string(),
            "boolean" => "boolean".to_string(),
            "null" => "null".to_string(),
            "array" => array_type(map, context),
            "object" => object_type(map, context),
            _ => "unknown".to_string(),
        },
        Some(_) => "unknown".to_string(),
        None => {
            // 没有 type 时的结构推断：对象关键字优先于数组关键字，全部缺失才 unknown。
            if map.contains_key("properties")
                || map.contains_key("additionalProperties")
                || map.contains_key("required")
            {
                return object_type(map, context);
            }
            if map.contains_key("items") || map.contains_key("prefixItems") {
                return array_type(map, context);
            }
            "unknown".to_string()
        }
    }
}

/// `arrayType(schema, context)`：`Array<T>`、元组 `[A, B]`，否则 `unknown[]`。
fn array_type(schema: &Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    // `items` 的非数组形态（对象或布尔）是元素类型；它先于 prefixItems 判断，所以两者同时
    // 存在时元组被忽略。`items !== undefined && !Array.isArray(items)`（数组形态是元组）。
    if let Some(items) = schema.get("items").filter(|items| !items.is_array()) {
        return format!("Array<{}>", to_type(items, context));
    }
    // 元组：prefixItems 优先，旧式 `items: [...]` 次之；都没有则退化为 unknown[]。
    let tuple: &[Value] = match schema.get("prefixItems") {
        Some(Value::Array(items)) => items,
        _ => match schema.get("items") {
            Some(Value::Array(items)) => items,
            _ => &[],
        },
    };
    if tuple.is_empty() {
        return "unknown[]".to_string();
    }
    format!(
        "[{}]",
        tuple
            .iter()
            .map(|item| to_type(item, context))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// `descriptionOf(property)`：属性描述（非对象的属性读作空串）。
fn description_of(property: &Value) -> &str {
    match property {
        Value::Object(map) => map
            .get("description")
            .and_then(Value::as_str)
            .map(kiana_json::js_string::js_trim)
            .unwrap_or(""),
        _ => "",
    }
}

/// `constraintTags(schema)`：类型表达不了的约束，渲染成 JSDoc 风格的 tag 行。
///
/// 顺序固定：`@integer` → `TAG_KEYWORDS` 顺序 → `@uniqueItems` → `@default` → `@deprecated`。
/// 每个 tag 只有 UTF-16 长度 ≤ `MAX_TAG_CHARS` 时才保留（超长**整条省略**，不是截断）；
/// 最后把行终止符转成 `\uXXXX`。
fn constraint_tags(schema: &Value) -> String {
    // 非对象 schema（含布尔与 null）没有可读的约束。
    let Value::Object(property) = schema else {
        return String::new();
    };
    // 只从 anyOf/oneOf 里 `type !== "null"` 的分支取约束：`int | None` 的约束通常写在
    // 非 null 分支里。判断只排除 type 恰为字符串 "null" 的变体。
    // Pydantic 的 `int | None` 之类把约束放在非 null 分支里；分支的约束与属性自身的约束同时生效。
    let variants = match property.get("anyOf") {
        Some(Value::Array(variants)) => Some(variants),
        // `anyOf` 不是数组时 TS 取 `property.oneOf`；它也不是数组就没有变体。
        _ => match property.get("oneOf") {
            Some(Value::Array(variants)) => Some(variants),
            _ => None,
        },
    };
    let non_null: Vec<&Value> = match variants {
        Some(variants) => variants
            .iter()
            .filter(|variant| {
                let ty = match variant {
                    Value::Object(map) => map.get("type"),
                    _ => None,
                };
                !matches!(ty, Some(Value::String(kind)) if kind == "null")
            })
            .collect(),
        None => Vec::new(),
    };
    let branch = if non_null.len() == 1 {
        Some(non_null[0])
    } else {
        None
    };
    // 参与合并的键视图：恰好一个非 null 分支**且该分支是对象**时是 [属性, 分支]
    // （属性在前，分支在后）；否则（零个/多个分支，或唯一分支不是对象）只有属性本身。
    // 顺序影响去重与 `@format` 这类可出现多次的 tag。
    let branches: Vec<TagBranch<'_>> = match branch {
        Some(Value::Object(map)) => vec![TagBranch { map: property }, TagBranch { map }],
        _ => vec![TagBranch { map: property }],
    };

    let mut tags: Vec<String> = Vec::new();
    // `@integer`：任一参与分支的 type 是 "integer"，或 type 数组里含 "integer"。
    let is_integer = |branch: &TagBranch<'_>| match branch.map.get("type") {
        Some(Value::String(kind)) => kind == "integer",
        Some(Value::Array(kinds)) => kinds
            .iter()
            .any(|kind| matches!(kind, Value::String(text) if text == "integer")),
        _ => false,
    };
    if branches.iter().any(is_integer) {
        tags.push("@integer".to_string());
    }
    for keyword in TAG_KEYWORDS {
        let values: Vec<Option<&Value>> = branches
            .iter()
            .map(|branch| branch.value(keyword))
            .collect();
        if LOWER_BOUNDS.contains(&keyword) || UPPER_BOUNDS.contains(&keyword) {
            // 约束同时成立时只有最紧的一条有意义：下界取所有分支的最大值，上界取最小值。
            let numbers: Vec<f64> = values
                .iter()
                .filter_map(|value| value.and_then(Value::as_f64))
                .collect();
            if numbers.is_empty() {
                continue;
            }
            let bound = if LOWER_BOUNDS.contains(&keyword) {
                numbers
                    .into_iter()
                    .fold(f64::NEG_INFINITY, |acc, value| acc.max(value))
            } else {
                numbers
                    .into_iter()
                    .fold(f64::INFINITY, |acc, value| acc.min(value))
            };
            tags.push(format!(
                "@{keyword} {}",
                kiana_json::format_js_number(bound)
            ));
            continue;
        }
        let mut seen: Vec<TagValue<'_>> = Vec::new();
        // 非边界关键字：按 `new Set` 的插入序去重后逐个输出；数字按 f64 值比较。
        for value in values.into_iter().flatten() {
            let entry = tag_value(value);
            if seen.contains(&entry) {
                continue;
            }
            seen.push(entry.clone());
            match entry {
                TagValue::Number(number) => {
                    tags.push(format!(
                        "@{keyword} {}",
                        kiana_json::format_js_number(number)
                    ));
                }
                TagValue::Text(text) => tags.push(format!("@{keyword} {text}")),
                TagValue::Other => {}
            }
        }
    }
    // `uniqueItems` 仍看参与分支；`@default`/`@deprecated` 是纯注解，只从属性本身读：
    // `default: null` 被抑制，`deprecated` 只认布尔 true。
    if branches
        .iter()
        .any(|branch| matches!(branch.map.get("uniqueItems"), Some(Value::Bool(true))))
    {
        tags.push("@uniqueItems".to_string());
    }
    // 注解（与约束不同）只来自属性本身。
    match property.get("default") {
        Some(value) if !value.is_null() => tags.push(format!(
            "@default {}",
            kiana_json::js_object::stringify(value)
        )),
        _ => {}
    }
    if matches!(property.get("deprecated"), Some(Value::Bool(true))) {
        tags.push("@deprecated".to_string());
    }
    // 长度过滤发生在行终止符转义**之前**：转义只会让 tag 变长，
    // 因此最终输出的注释行可能超过 MAX_TAG_CHARS。
    tags.retain(|tag| js_length(tag) <= MAX_TAG_CHARS);
    tags.into_iter()
        .map(|tag| escape_line_terminators(&tag))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 一个分支里某个 tag 关键字的取值视图（含 draft-04 布尔排他边界的改写）。
struct TagBranch<'a> {
    map: &'a Map<String, Value>,
}

impl TagBranch<'_> {
    /// `schema.exclusiveMinimum === true`：draft-04 / OpenAPI 3.0 的布尔排他下界。
    fn exclusive_minimum(&self) -> bool {
        matches!(self.map.get("exclusiveMinimum"), Some(Value::Bool(true)))
    }

    /// `schema.exclusiveMaximum === true`。
    fn exclusive_maximum(&self) -> bool {
        matches!(self.map.get("exclusiveMaximum"), Some(Value::Bool(true)))
    }

    /// 改写后的关键字取值：布尔排他边界会把 `minimum`/`maximum` 搬到
    /// `exclusiveMinimum`/`exclusiveMaximum` 上（并把原键置为 `undefined`）。
    fn value(&self, keyword: &str) -> Option<&Value> {
        match keyword {
            "minimum" => {
                if self.exclusive_minimum() {
                    None
                } else {
                    self.map.get("minimum")
                }
            }
            "exclusiveMinimum" => {
                if self.exclusive_minimum() {
                    self.map.get("minimum")
                } else {
                    self.map.get("exclusiveMinimum")
                }
            }
            "maximum" => {
                if self.exclusive_maximum() {
                    None
                } else {
                    self.map.get("maximum")
                }
            }
            "exclusiveMaximum" => {
                if self.exclusive_maximum() {
                    self.map.get("maximum")
                } else {
                    self.map.get("exclusiveMaximum")
                }
            }
            _ => self.map.get(keyword),
        }
    }
}

/// `new Set(values)` 里的一个值：数字按 f64（SameValueZero），字符串按内容，其余不产生 tag。
#[derive(Clone, PartialEq)]
enum TagValue<'a> {
    /// JSON 数字。
    Number(f64),
    /// JSON 字符串。
    Text(&'a str),
    /// `true`/`false`/`null`/对象/数组：不产生 tag，但仍参与去重。
    Other,
}

/// 把一个 JSON 值映射成 `new Set` 里的元素（只区分数字、字符串与其余）。
fn tag_value(value: &Value) -> TagValue<'_> {
    match value {
        Value::Number(number) => TagValue::Number(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(text) => TagValue::Text(text),
        _ => TagValue::Other,
    }
}

/// 把行终止符（CR、LF、U+2028、U+2029）转成小写 4 位十六进制的 `\uXXXX`。
///
/// 为什么必须转义：tag 最终落在 `// …` 注释行里，值里的裸换行会撕开注释、破坏生成的
/// TypeScript；U+2028/U+2029 在 JS 里同样是行终止符。
/// `leaves_out_long_tags_and_escapes_line_terminators` 锁定这一点。
fn escape_line_terminators(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    for ch in tag.chars() {
        if is_line_terminator(ch) {
            out.push_str(&format!("\\u{:04x}", ch as u32));
        } else {
            out.push(ch);
        }
    }
    out
}

/// 是否是 JS 的行终止符（`LINE_TERMINATORS = /[\r\n  ]/g`）。
fn is_line_terminator(ch: char) -> bool {
    matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

/// 按 `LINE_TERMINATORS` 切分（每个行终止符都是分隔符）。
fn split_line_terminators(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if is_line_terminator(ch) {
            parts.push(&text[start..index]);
            start = index + ch.len_utf8();
        }
    }
    parts.push(&text[start..]);
    parts
}

/// `objectType(schema, context)`：对象的单行/多行渲染。
fn object_type(schema: &Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    let empty = Map::new();
    // properties / required 缺失或类型不对时按空处理；required 里的非字符串条目被忽略。
    let properties = match schema.get("properties") {
        Some(Value::Object(properties)) => properties,
        _ => &empty,
    };
    let required: Vec<&str> = match schema.get("required") {
        Some(Value::Array(required)) => required.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    // 属性名按 UTF-16 码元排序（`Array.prototype.sort()` 的默认序），不是 Rust `str` 的
    // UTF-8 字节序；非 BMP 字符会因此得到与 `str::cmp` 不同的顺序。
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort_by(|a, b| cmp_utf16(a, b));

    // 不在 required 里的属性都带 `?`；required 用线性 contains，重复项没有副作用。
    let mut members: Vec<String> = names
        .iter()
        .map(|name| {
            let optional = if required.contains(&name.as_str()) {
                ""
            } else {
                "?"
            };
            format!(
                "{}{optional}: {};",
                property_key(name),
                to_type(&properties[*name], context)
            )
        })
        .collect();

    // additionalProperties 四态：false 不放开索引；true 放开为 unknown；其它值（含 null）
    // 按 schema 渲染索引类型；缺省时只有在没有具名属性时才放开 unknown 索引。
    match schema.get("additionalProperties") {
        Some(Value::Bool(false)) => {}
        Some(Value::Bool(true)) => members.push("[key: string]: unknown;".to_string()),
        Some(additional) => {
            members.push(format!("[key: string]: {};", to_type(additional, context)))
        }
        None => {
            if names.is_empty() {
                members.push("[key: string]: unknown;".to_string());
            }
        }
    }
    // 只有「无具名属性 + additionalProperties:false」才会走到这里。
    if members.is_empty() {
        return "{}".to_string();
    }

    // 注释只来自具名属性：描述按行终止符切分、逐行 JS trim、丢弃空行；tag 追加成最后一行。
    // 超预算的第二遍（context.tags == false）只保留描述。
    let comments: Vec<Vec<String>> = names
        .iter()
        .map(|name| {
            let property = &properties[*name];
            let mut lines: Vec<String> = split_line_terminators(description_of(property))
                .into_iter()
                .map(|line| kiana_json::js_string::js_trim(line).to_string())
                .filter(|line| !line.is_empty())
                .collect();
            let tags = if context.tags {
                constraint_tags(property)
            } else {
                String::new()
            };
            if !tags.is_empty() {
                lines.push(tags);
            }
            lines
        })
        .collect();

    // 没有任何注释时用单行对象；只要有一条注释，就改成“注释 + 缩进成员”的块状布局。
    if !comments.iter().any(|comment| !comment.is_empty()) {
        return format!("{{ {} }}", members.join(" "));
    }

    let mut lines = String::from("{");
    // names 与 members 一一对应；additionalProperties 产生的索引签名追加在 members 末尾，
    // 所以在遍历完具名属性后再原样补上剩下的成员。
    for (index, comment) in comments.iter().enumerate() {
        for line in comment {
            lines.push('\n');
            lines.push_str(INDENT);
            lines.push_str("// ");
            lines.push_str(line);
        }
        lines.push('\n');
        lines.push_str(INDENT);
        // 嵌套对象渲染出的多行成员要再缩进一层，保证内层成员与注释对齐。
        lines.push_str(&members[index].replace('\n', &format!("\n{INDENT}")));
    }
    for member in &members[names.len()..] {
        lines.push('\n');
        lines.push_str(INDENT);
        lines.push_str(member);
    }
    lines.push('\n');
    lines.push('}');
    lines
}

/// 按 UTF-16 码元比较两个字符串（`Array.prototype.sort()` 的默认比较）。
fn cmp_utf16(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
