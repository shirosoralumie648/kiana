//! latex.ts：LaTeX 子集 → Unicode 终端排版。
//!
//! 对应 `packages/tui/src/latex.ts`（1506 行）。TS 导出的 API 只有 [`render_latex`] 与
//! [`RenderLatexOptions`]；Rust 侧额外把符号表 [`SYMBOLS`] 以 `pub static` 暴露（TS 未导出）。
//! `None` 对应 TS 的 `undefined`（源串含不支持或格式错误的语法）。
//!
//! ## 与 TS 的表示差异
//!
//! 除"孤立代理的返回串表示"与"递归深度"的 **V8 边界**两条（见各自条目，属于无法在冻结
//! 签名下消除的已知差异）外，其余都不改变可观察输出。
//!
//! - **索引单位 / 孤立代理**。TS 的 `source[i]`/`slice`/`indexOf` 按 **UTF-16 码元**；本文件
//!   把源码展开成 `Vec<char>` 后按**码点**索引。普通文本两边一致；裸的必需参数只读一个码元
//!   时（`x^🙂`、`\hat🙂`、`\frac🙂2`、`\vec🙂`、`x_🙂`），TS 取到高代理、低代理留在外层，
//!   可能重新相邻、也可能被组合符/换行分开。
//!
//!   本文件用 BMP 私用区的**占位字符**表示两半（`SURROGATE_HIGH_BASE` 文档）：占位符在
//!   UTF-16 里各占 1 个码元（位置/长度语义与 TS 相同），`latex_visible_width` 按 JS 的
//!   字素规则计量（相邻两半 = 正常字符，落单 = 宽度 0），最后 `decode_output` 把相邻的
//!   两半合回原字符、把落单的一半写成 `U+FFFD`。
//!
//!   **唯一残余**：TS 返回的 `string` 里落单代理是**真实存在**的 UTF-16 码元，Rust 的
//!   `String` 无法表示。若调用方直接比较 `renderLatex()` 的返回值，含落单代理的输出会
//!   不同（Rust 是 `U+FFFD`）；但**终端字节完全一致**——Node 把落单代理写成 UTF-8 时正是
//!   `EF BF BD`（`U+FFFD`），且布局宽度/缩进也按孤立代理的 0 宽计算。相邻两半（如内联
//!   `x^😀`、matrix 单元格）在 Rust 输出里就是完整字符。
//!
//!   为保证占位符不与输入文本混淆，源码里**字面**出现的 `U+E000..=U+E800` 会被写成
//!   `U+E800 + 字符`（解码时还原）。含这类 BMP 私用区字符的数学表达式可能得到不同的
//!   **格式化决策**（例如单字符脚本被当作两字符而加括号），文本内容仍然还原；正常数学/
//!   文本输入不含这些码点，不受影响。
//! - **正则**。`regex` crate 的 `\p{L}` / `\p{N}` 是**较旧的 Unicode 版本**，与 Node 26
//!   （Unicode 17.0 / ICU 78.3）相差 4,644 / 13 个码点，因此这两类改用 `JS_LETTER_RANGES` /
//!   `JS_NUMBER_RANGES`（由 Node 逐码点枚举生成）。JS 的 `\s` 换成 `is_js_whitespace`
//!   （25 个码点，比 `White_Space` 多 `U+FEFF`、少 `U+0085`）；`normalizeScriptValue` 的
//!   `\s*([=+-])\s*` 因此手写单遍扫描。两条 lookbehind/lookahead 命名算子空格模式分别改写为
//!   `space_named_operator_left` / `space_named_operator_right`（匹配互不重叠，结果逐字相同）。
//! - **字符串宽度**。用 `latex_visible_width`（内部对普通文本直接走 [`visible_width`]），
//!   与 TS 的 `visibleWidth` 同一实现。
//! - **JS 原型链**。`SYMBOLS` / `ACCENTS` / `NEGATED_SYMBOLS` 在 TS 里是对象字面量，所以
//!   `\constructor` 会命中原型链上的 native 函数；`object_literal_lookup` 复刻这个回退
//!   （见 `OBJECT_PROTOTYPE_MEMBERS`），并用 `CommandValue::prototype` 把"函数值"原样
//!   传给参数位置（`\hat\constructor`、`\text\constructor` 的传播语义与 TS 逐值一致）。
//! - **异常**。TS 在函数值上调用 `.trim()` / `.replace()` 会抛 `TypeError` 并冒泡给调用方；
//!   本文件的 `render_latex` 签名（骨架冻结）没有异常通道，也不允许为模型可控输入而 panic，
//!   所以这些输入（以及 `renderLayout` 因环递归导致的 `RangeError`）返回 `None`——调用方同样
//!   拿不到渲染结果，回退到原文。
//! - **递归深度**。`MAX_PARSE_DEPTH` / `MAX_LAYOUT_DEPTH` 与 `with_large_stack`：TS 没有深度
//!   上限，靠 JS 栈抛**可捕获的** `RangeError`；Rust 的栈溢出是不可捕获的进程 abort。因此
//!   深度超过 `INLINE_RECURSION_BUDGET` 的输入改在 64 MiB 栈的线程上解析/展开，并在 8,192 层
//!   主动放弃（`None`）；`renderLayout` 还会先对布局节点图做环检测（字面量布局标记可让节点
//!   引用自身或互相引用）。**已知差异**：V8 的栈上限随构造与栈状态浮动（实测 Node 26.12：
//!   `\sqrt{` 约 1.3k 层、嵌套 `x^{...}` 约 1.4k 层、`\frac{1}{` 约 1.7k 层、花括号约 5k 层
//!   即抛 `RangeError`，±数百层），本文件在 8192 层以内一律正常渲染，所以约 1.3k–8192 层
//!   之间会出现"TS 抛异常、Rust 有输出"。这类极端输入的布局展开是超线性的（TS 同量级：
//!   `x^{x^{...}}` 1.2k 层在 TS 里也要约 6.5 s），无法用单一深度阈值精确复刻 V8 的边界。
//!
//! ## 必须保留的怪异行为（逐条源自 TS 源）
//!
//! - 私有区标记字符（`U+F0000`…`U+F0005`、`U+0000` 负间距、`U+F0002` 受保护空格）**逐字**照抄。
//! - `parseScripts` 的 `canUseLayout` 判定：多字符且不含 `/`、不含 `A-Z*∗` 的脚本值**禁止**二维布局
//!   （所以 `x_{ab}` 在 display 下仍是 `x_ab`）——这是反直觉的，但它是可观察行为。
//! - `\frac{}{}` 在 display 下的 `"\n─"`（全空行 → `Math.min(...[])` 的 `Infinity` 语义）。
//! - 旧式字体切换（`{\bf ...}`）、`cases` 的偶数行居中、必需的参数内的换行、跨行的控制空格。
//!
//! ## 执行流程
//!
//! 一次 [`render_latex`] 调用依次经过四步：
//!
//! 1. **编码**：`encode_source` 把源码里**字面**出现的占位区码点（`U+E000..=U+E800`）
//!    转义成 `U+E800 + 字符`，使解析器可以无歧义地用这些码点表示被拆开的代理对。
//! 2. **解析**：`LatexParser::render` 从 `parse_sequence(None)` 进入，`parse_sequence_body`
//!    是主循环（`{`/`}` 分组、`\命令`、`^`/`_`、空白、关系符、`&`、`~`、矩阵后的 `.`），
//!    命令由 `parse_command_body` 的 if 链分派（分派顺序本身就是行为，见该函数文档）。
//!    display 模式下真正进入堆叠的 `\frac`/`\dfrac`（`\tfrac`，以及外层已堆叠时其分子/
//!    分母参数内的嵌套 `\frac`——`stack_fractions` 会收窄——除外）、带上下标且未被
//!    `\nolimits` 关闭的算子（`DISPLAY_LIMIT_SYMBOLS` 符号、`LIMIT_OPERATORS` 命名算子与
//!    `\operatorname*`）、需要二维布局的 `Script`，以及产生多行（行数 > 1）的矩阵/`cases`
//!    （任何模式），都会通过 `push_node` 写入一个 `LayoutNode`，并在文本里留下
//!    `U+F0000{下标}U+F0001` 形式的布局标记；其余内容直接拼成可见文本。
//! 3. **布局**：只要产生过布局节点，`render_latex_encoded` 先用 `layout_expansion_depth`
//!    检查节点引用图是否有环并估算展开深度，再由 `render_layout` 把标记就地展开成多行
//!    文本（`join_layouts` 按基线并排、`pad_layout_line` 按显示宽度补齐）。
//! 4. **解码与收尾**：`finish_layout` 去掉所有行共有的缩进并合并受保护空格（没有布局节点
//!    时不经过它，直接替换受保护空格）；`decode_output` 还原转义字符、把相邻的代理
//!    占位符重新合成原字符，落单的一半写成 `U+FFFD`。
//!
//! ## 阅读顺序
//!
//! 先看 [`RenderLatexOptions`] 与 [`render_latex`] 了解入口和返回值；再读
//! `parse_sequence_body` → `parse_command_body`（命令分派链）→ `parse_scripts` /
//! `render_cases` / `render_matrix` 这些结构命令；最后读 `render_layout` 与 `join_layouts`
//! 理解二维排版。符号/命令表集中在文件中部（从 `SYMBOLS` 开始），表结构与 TS 源一致；
//! `Pi` 条目按标准 LaTeX 拼写解析为 `Π`（不要改回旧的 `Kiana` 写法）。
//! `JS_LETTER_RANGES` / `JS_NUMBER_RANGES` 由 Node 逐码点枚举生成，这些表不要手改。
//!
//! ## 调用方契约
//!
//! 生产代码中唯一的调用方是 markdown 组件（`crate::components::markdown::Markdown`）：
//! 行内公式（`Token::Latex`）用 `RenderLatexOptions::default()`，公式块（`Token::LatexBlock`）
//! 用 `RenderLatexOptions { display: true }`。它对 `None` 的处理是回退输出原始 LaTeX 文本，
//! 所以 `None` 只能表示"拿不到可信的渲染结果"，不能用来表示空结果——空输入返回 `Some("")`。
//! 行为回归测试在 `crates/kiana-tui/tests/latex_review_regression.rs`（逐条对照 TS 输出）。
//!
//! ## 注意事项
//!
//! - 解析器是**单遍、无回溯**的：每个分支只前进不后退，任何"先试再退"的改动都会改变
//!   分派与消费位置，从而破坏既有输出契约（回归测试逐条锁定输出）。
//! - 递归深度有两条相互独立的限制：解析深度上限 `MAX_PARSE_DEPTH`、布局展开上限
//!   `MAX_LAYOUT_DEPTH`；超限返回 `None` 而不是 panic/abort（理由见各自文档）。
//! - 输出必须经过 `normalize_output`（命名算子空格）与 `decode_output`（占位符/转义）；
//!   新增返回文本的分支时不要绕开它们。

use std::sync::OnceLock;

use regex::Regex;

use crate::text::{is_js_whitespace, trim_end_js, trim_js, visible_width};

/// `RenderLatexOptions`：`display` 为真才启用二维布局。
///
/// `display` 影响三处：`\frac` 默认纵向堆叠（`\tfrac` 例外）；`\sum`/`\int` 等
/// `DISPLAY_LIMIT_SYMBOLS` 把上下标堆在符号上方/下方；`^`/`_` 在 Unicode 上下标无法表示、
/// 或脚本本身嵌套在另一段脚本里时改走二维布局（前提是脚本值通过了 `canUseLayout` 判定）。
///
/// 行内公式传 `false`（不主动堆叠分数/算子；矩阵和 `\\` 换行仍可能产生多行），公式块传
/// `true`；markdown 组件的两种 token 分别使用这两个值（见文件级文档"调用方契约"）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderLatexOptions {
    /// Stack fractions and operator limits vertically for display math (default: false).
    pub display: bool,
}

// ============================================================================
// 私有标记字符
// ============================================================================

/// 布局节点占位符前缀。
const LAYOUT_MARKER_START: char = '\u{f0000}';
/// 布局节点占位符后缀。
const LAYOUT_MARKER_END: char = '\u{f0001}';
/// 对齐用"受保护空格"，最终替换为普通空格。
const PROTECTED_SPACE: char = '\u{f0002}';
/// 命名算子输出的左标记。
const NAMED_OPERATOR_START: char = '\u{f0004}';
/// 命名算子输出的右标记。
const NAMED_OPERATOR_END: char = '\u{f0005}';
/// `\!` 等负间距命令的返回值。
const NEGATIVE_SPACE: char = '\u{0}';

/// 非 BMP 字符被按 UTF-16 码元拆开时，**高代理**码元的占位字符基址（BMP 私用区）。
///
/// TS 的 `source[position]` 取一个 UTF-16 码元：增补平面字符被拆成高/低两个孤立代理，
/// 高代理进入参数值、低代理留在外层。Rust 的 `String` 表示不了孤立代理，这里用 BMP 私用区
/// 的占位字符代替：`U+E000..=U+E3FF` ↔ 高代理 `U+D800..=U+DBFF`，
/// `U+E400..=U+E7FF` ↔ 低代理 `U+DC00..=U+DFFF`。
///
/// 占位字符在 UTF-16 里正好是 1 个码元（与孤立代理相同），所以解析器的位置/长度语义不变；
/// 宽度按孤立代理的 0 计（[`latex_visible_width`]）；最终输出时相邻的高+低占位符**重新合成**
/// 原来的字符，落单的占位符写成 `U+FFFD`（Node 把孤立代理编码成 UTF-8 时的字节）。
const SURROGATE_HIGH_BASE: u32 = 0xE000;
/// 低代理占位字符基址。
const SURROGATE_LOW_BASE: u32 = 0xE400;
/// 占位区上界（不含）。
const SURROGATE_PLACEHOLDER_END: u32 = 0xE800;
/// 输入里字面出现占位区字符时的转义前缀（`ESCAPE + 字符`，解码时还原）。
const LATEX_ESCAPE: char = '\u{E800}';

/// 字符是否落在高/低代理占位区（`U+E000..=U+E7FF`）。
fn is_surrogate_placeholder(character: char) -> bool {
    let code = character as u32;
    (SURROGATE_HIGH_BASE..SURROGATE_PLACEHOLDER_END).contains(&code)
}

/// 源码编码：把**字面**出现的占位区字符写成 `ESCAPE + 字符`，避免与拆分产生的占位符混淆。
///
/// 绝大多数输入不含这些码点，直接原样返回。编码只在顶层 [`render_latex_inner`] 做一次；
/// 嵌套 parser 的输入都是同一编码空间的切片，不会二次转义。
fn encode_source(source: &str) -> String {
    if !source
        .chars()
        .any(|character| character == LATEX_ESCAPE || is_surrogate_placeholder(character))
    {
        return source.to_string();
    }
    let mut encoded = String::with_capacity(source.len() + 8);
    for character in source.chars() {
        if character == LATEX_ESCAPE || is_surrogate_placeholder(character) {
            encoded.push(LATEX_ESCAPE);
        }
        encoded.push(character);
    }
    encoded
}

/// 输出解码：`ESCAPE + 字符` → 字面字符；相邻的 高+低 占位符 → 原字符；
/// 落单占位符 → `U+FFFD`（终端字节层面与孤立代理的 UTF-8 编码一致）。
fn decode_output(value: &str) -> String {
    if !value
        .chars()
        .any(|character| character == LATEX_ESCAPE || is_surrogate_placeholder(character))
    {
        return value.to_string();
    }
    let characters: Vec<char> = value.chars().collect();
    let mut decoded = String::with_capacity(value.len());
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        let code = character as u32;
        if character == LATEX_ESCAPE {
            if let Some(escaped) = characters.get(index + 1) {
                decoded.push(*escaped);
            } else {
                // 编码器不会产生落单的转义符；万一被切片截断，按 Node 的代理编码处理。
                decoded.push('\u{FFFD}');
            }
            index += 2;
            continue;
        }
        if (SURROGATE_HIGH_BASE..SURROGATE_LOW_BASE).contains(&code) {
            if let Some(low) = characters.get(index + 1) {
                let low_code = *low as u32;
                if (SURROGATE_LOW_BASE..SURROGATE_PLACEHOLDER_END).contains(&low_code) {
                    let high = 0xD800 + (code - SURROGATE_HIGH_BASE);
                    let low = 0xDC00 + (low_code - SURROGATE_LOW_BASE);
                    let scalar = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                    if let Some(recombined) = char::from_u32(scalar) {
                        decoded.push(recombined);
                        index += 2;
                        continue;
                    }
                }
            }
            decoded.push('\u{FFFD}');
            index += 1;
            continue;
        }
        if (SURROGATE_LOW_BASE..SURROGATE_PLACEHOLDER_END).contains(&code) {
            decoded.push('\u{FFFD}');
            index += 1;
            continue;
        }
        decoded.push(character);
        index += 1;
    }
    decoded
}

/// `visibleWidth` 的等价物，额外处理 UTF-16 拆分占位符。
///
/// TS 的宽度按 `Intl.Segmenter` 的字素切分：**相邻的高+低代理在 JS 字符串里就是一个正常
/// 字符**（宽度按该字符的表值，如 emoji = 2），落单的代理才是宽度 0 的独立字素。
/// 因此这里把相邻的高+低占位符合成原字符、留在当前字素段里（保持与后续 ZWJ/组合符的
/// 聚类），只有落单的一半按 0 计并断开字素段。`ESCAPE + 字符` 按被转义字符的宽度计。
fn latex_visible_width(text: &str) -> usize {
    if !text
        .chars()
        .any(|character| character == LATEX_ESCAPE || is_surrogate_placeholder(character))
    {
        return visible_width(text);
    }
    let mut width = 0usize;
    let mut segment = String::new();
    let characters: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character == LATEX_ESCAPE {
            if let Some(escaped) = characters.get(index + 1) {
                segment.push(*escaped);
            }
            index += 2;
            continue;
        }
        let code = character as u32;
        if (SURROGATE_HIGH_BASE..SURROGATE_LOW_BASE).contains(&code) {
            if let Some(low) = characters.get(index + 1) {
                let low_code = *low as u32;
                if (SURROGATE_LOW_BASE..SURROGATE_PLACEHOLDER_END).contains(&low_code) {
                    let high = 0xD800 + (code - SURROGATE_HIGH_BASE);
                    let low = 0xDC00 + (low_code - SURROGATE_LOW_BASE);
                    let scalar = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                    if let Some(recombined) = char::from_u32(scalar) {
                        segment.push(recombined);
                        index += 2;
                        continue;
                    }
                }
            }
            // 落单的高代理：独立字素、宽度 0。
            width += visible_width(&segment);
            segment.clear();
            index += 1;
            continue;
        }
        if (SURROGATE_LOW_BASE..SURROGATE_PLACEHOLDER_END).contains(&code) {
            // 落单的低代理：独立字素、宽度 0。
            width += visible_width(&segment);
            segment.clear();
            index += 1;
            continue;
        }
        segment.push(character);
        index += 1;
    }
    width + visible_width(&segment)
}

/// 解析递归深度上限（栈保护；超过即 `None`，对应 TS 在 V8 栈耗尽时抛 `RangeError`）。
///
/// TS 自身没有深度限制，靠 JS 栈抛**可捕获的** `RangeError`。实测（Node 26.12.1，独立进程；
/// 阈值随构造与栈状态浮动数百层）：嵌套花括号约 5,000 层、嵌套 `\sqrt{` 约 1,300 层、
/// 嵌套 `\frac{1}{` 约 1,700 层、嵌套 `x^{...}` 约 1,400 层。Rust 的栈溢出是**进程 abort**
/// （`fatal runtime error: stack overflow, aborting`，不可捕获，会杀掉整个 TUI/agent），
/// 因此本文件改用两条措施：
///
/// 1. 递归深度超过 [`INLINE_RECURSION_BUDGET`] 的输入放到 64 MiB 栈的线程上执行
///    （见 [`with_large_stack`]），让深嵌套照常解析而不是 abort；
/// 2. 仍在 [`MAX_PARSE_DEPTH`] 处主动放弃并返回 `None`（与"TS 抛 `RangeError`"等价：
///    两种情况下调用方都拿不到渲染结果，回退到原文）。
///
/// 上限取 8192 的取舍见模块文档：它高于 TS 能渲染的任何构造，因此约 1.3k–8192 层之间会出现
/// "TS 抛异常、Rust 有输出"的已知差异；精确复刻 V8 的边界是不可能的（同一构造在不同栈状态
/// 下的阈值不同）。布局阶段的深度上限见 [`MAX_LAYOUT_DEPTH`]。
const MAX_PARSE_DEPTH: usize = 8192;

/// `renderLayout` 展开布局节点的深度上限（不含环的情况；环会直接返回 `None`）。
///
/// `render_layout` 的递归在解析完成后进行，深度可以与解析深度无关：用户输入里的字面量
/// 布局标记（U+F0000/U+F0001）能把多个互不相邻的节点串成一条长链。这里以 8192 作为
/// 展开深度上界（超过即返回 `None`，调用方回退到原文）；深度超过
/// [`INLINE_RECURSION_BUDGET`] 时会在 64 MiB 栈的线程上展开。
const MAX_LAYOUT_DEPTH: usize = MAX_PARSE_DEPTH;

// ============================================================================
// 表查询
// ============================================================================

/// 在 `&[(键, 值)]` 数组里按**顺序**线性查找键；找不到返回 `None`。
///
/// 各表的元素顺序与 TS 对象字面量一致（见文件中部各表文档），因此这里的"第一个匹配"
/// 与 JS 属性查找的覆盖规则一致（重复键由表本身避免）。
fn lookup(map: &[(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    map.iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

fn in_set(set: &[&str], value: &str) -> bool {
    set.contains(&value)
}

/// `SYMBOLS[command]`，走 [`object_literal_lookup`] 以复刻 JS 原型链回退。
fn symbol(command: &str) -> Option<JsValue> {
    object_literal_lookup(SYMBOLS, command)
}

/// `ACCENTS[command]`，同样带原型链回退。
fn accent(command: &str) -> Option<JsValue> {
    object_literal_lookup(ACCENTS, command)
}

/// `NEGATED_SYMBOLS[value]`，同样带原型链回退（`\not{constructor}` 会命中函数值）。
fn negated_symbol(value: &str) -> Option<JsValue> {
    object_literal_lookup(NEGATED_SYMBOLS, value)
}

/// `BLACKBOARD[character]`：只有 `C/H/N/P/Q/R/Z` 有映射，其余字符返回 `None`。
fn blackboard(character: char) -> Option<&'static str> {
    let mut buffer = [0u8; 4];
    lookup(BLACKBOARD, character.encode_utf8(&mut buffer))
}

/// 单个字符查替换表：把 `char` 编码进 4 字节栈缓冲区后走 [`lookup`]。
fn script_char(
    replacements: &[(&'static str, &'static str)],
    character: char,
) -> Option<&'static str> {
    let mut buffer = [0u8; 4];
    lookup(replacements, character.encode_utf8(&mut buffer))
}

// ---------------------------------------------------------------------------
// JS 对象的**原型链**属性（`SYMBOLS` / `NEGATED_SYMBOLS` / `ACCENTS` 都是普通对象字面量）
// ---------------------------------------------------------------------------
//
// TS 用 `SYMBOLS[command]`、`NEGATED_SYMBOLS[value]`、`ACCENTS[command]` 直接查对象字面量，
// 因此 `\constructor`、`\toString` 这类名字会命中原型链上的 `Object.prototype` 成员而不是
// "未找到"：`renderLatex("\\constructor")` 是 `"function Object() { [native code] }"`。
// 这是原型链查找带来的意外行为，但已有输出契约要求保持，而且普通文档也可能触发
// （`\constructor` 不是 LaTeX 命令，作者写错时会把这段 native-code 文本渲染出来）。
//
// 复刻范围：值被**当作字符串**使用的位置（`parseCommand` 的返回值、`\not` 的结果、
// accent 的 `${value}${accent}` 拼接）。JS 在 `+` / 模板字符串里会自动把函数转成
// `String(fn)`，所以这些路径只会得到上表里的文本。
//
// 值**原样传给参数**的路径也要复刻：`parseRequiredArgumentValue` 的 `\command` 分支返回
// `parseCommand()` 的结果本身，`text*`/`mbox` 又把它原样返回，所以 `\hat\constructor` 拿到的是
// 函数（`Array.from(fn).length` = arity），`\frac\constructor2` 拿到函数后 `.trim()` 抛
// `TypeError`。前者由 [`CommandValue::prototype`] 携带 arity 复刻；后者在 Rust 里映射成
// `renderLatex` 的 `None`（见模块文档"异常"）。带花括号的参数经 `parseSequence` 的
// `result += command` 后永远是字符串（`\frac{\text\constructor}{2}`），`parseRequiredArgumentValue`
// 会清掉内层留下的标记。

/// `Object.prototype` 上可按属性名直接查到的函数成员。
///
/// 第二列是 `String(fn)`（JS 把函数转成字符串的结果），第三列是 `fn.length`——`\hat` 之类
/// 的 accent 分支用 `Array.from(value).length === 1` 判断"单字符"，而 `Array.from(函数)`
/// 的长度就是该函数的 `length`（arity），所以 arity 会影响输出。
/// 顺序同 `Object.getOwnPropertyNames(Object.prototype)`。
const OBJECT_PROTOTYPE_MEMBERS: &[(&str, &str, u8)] = &[
    ("constructor", "function Object() { [native code] }", 1),
    (
        "__defineGetter__",
        "function __defineGetter__() { [native code] }",
        2,
    ),
    (
        "__defineSetter__",
        "function __defineSetter__() { [native code] }",
        2,
    ),
    (
        "hasOwnProperty",
        "function hasOwnProperty() { [native code] }",
        1,
    ),
    (
        "__lookupGetter__",
        "function __lookupGetter__() { [native code] }",
        1,
    ),
    (
        "__lookupSetter__",
        "function __lookupSetter__() { [native code] }",
        1,
    ),
    (
        "isPrototypeOf",
        "function isPrototypeOf() { [native code] }",
        1,
    ),
    (
        "propertyIsEnumerable",
        "function propertyIsEnumerable() { [native code] }",
        1,
    ),
    ("toString", "function toString() { [native code] }", 0),
    ("valueOf", "function valueOf() { [native code] }", 0),
    (
        "toLocaleString",
        "function toLocaleString() { [native code] }",
        0,
    ),
];

/// `Object.prototype.__proto__`：访问器属性，值是对象，`String(value)` = `"[object Object]"`，
/// 且 `Array.from(value)` 的长度是 0（对象没有 `length`）。
const OBJECT_PROTOTYPE_PROTO_TEXT: &str = "[object Object]";

/// 原型链上的值：JS 函数（带 arity）或 `__proto__` 对象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrototypeValue {
    /// 原型链上的函数：`Array.from(fn).length` = arity。
    Function(usize),
    /// `__proto__`（对象）：`Array.from(obj).length` = 0。
    Object,
}

/// 对象字面量属性查询的结果：`String(value)` 与它是否是原型链上的非字符串值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JsValue {
    text: &'static str,
    /// 命中原型链时是函数/对象（此时 `text` 是 `String(value)`）；自有属性是 `None`。
    prototype: Option<PrototypeValue>,
}

impl JsValue {
    /// 来自自有属性（字符串值）。
    fn string(text: &'static str) -> Self {
        Self {
            text,
            prototype: None,
        }
    }
}

/// `LatexParser::parseCommand` 的返回值。
///
/// TS 的 `parseCommand` 大多返回字符串，但 `SYMBOLS[command]` 命中原型链、以及
/// `text*`/`mbox` 原样返回参数时，返回值可能是 `Object.prototype` 上的**函数/对象**。
/// `parseRequiredArgumentValue` 的 `\command` 分支会把这个值原样交给调用方，所以必须把
/// "它是函数值"与文本一起传出去（`\hat\constructor` 的 `Array.from(value).length`、
/// `\frac\constructor2` 的 `value.trim()` 抛错都依赖它）。
#[derive(Debug, Clone)]
struct CommandValue {
    /// `String(value)`。
    text: String,
    /// 非 `None` 表示返回值是原型链上的函数/对象（未被模板字符串强制转成文本）。
    prototype: Option<PrototypeValue>,
}

impl CommandValue {
    fn string(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            prototype: None,
        }
    }

    fn with_prototype(text: impl Into<String>, prototype: Option<PrototypeValue>) -> Self {
        Self {
            text: text.into(),
            prototype,
        }
    }
}

/// `map[key]`（含 `Object.prototype` 的原型链回退）。
fn object_literal_lookup(map: &[(&'static str, &'static str)], key: &str) -> Option<JsValue> {
    if let Some(value) = lookup(map, key) {
        return Some(JsValue::string(value));
    }
    if key == "__proto__" {
        return Some(JsValue {
            text: OBJECT_PROTOTYPE_PROTO_TEXT,
            prototype: Some(PrototypeValue::Object),
        });
    }
    OBJECT_PROTOTYPE_MEMBERS
        .iter()
        .find(|(name, _, _)| *name == key)
        .map(|(_, text, arity)| JsValue {
            text,
            prototype: Some(PrototypeValue::Function(usize::from(*arity))),
        })
}

// ---------------------------------------------------------------------------
// JS 的 `\p{L}` / `\p{N}`（General_Category=L / N，Unicode 17.0）
// ---------------------------------------------------------------------------
//
// `regex` crate 自带的是**较旧的 Unicode 版本**，与 Node 26（Unicode 17.0 / ICU 78.3）相比
// `\p{L}` 少 4,644 个码点、`\p{N}` 少 13 个（例：U+088F、U+0C5C、U+0CDC、U+A7CE、
// U+11DE0..U+11DE9、U+16FF4..U+16FF6），会让 `formatFraction` / `formatRoot` /
// `formatScript` 以及命名算子空格产生不同输出。因此下表由 Node 逐码点枚举生成
// （`^\p{L}$` / `^\p{N}$`，2026-10，node v26.12.1 / ICU 78.3），是唯一事实来源，
// 本文件不再使用 `regex` 的 `\p{L}` / `\p{N}`。
//
// 生成脚本：对 0..=0x10FFFF 逐码点执行 `new RegExp("^\\p{L}$", "u").test(String.fromCodePoint(cp))`，
// 把连续码点合并为闭区间（跳过代理区）。

/// `\p{L}`：684 个区间。
static JS_LETTER_RANGES: &[(u32, u32)] = &[
    (0x41, 0x5A),
    (0x61, 0x7A),
    (0xAA, 0xAA),
    (0xB5, 0xB5),
    (0xBA, 0xBA),
    (0xC0, 0xD6),
    (0xD8, 0xF6),
    (0xF8, 0x2C1),
    (0x2C6, 0x2D1),
    (0x2E0, 0x2E4),
    (0x2EC, 0x2EC),
    (0x2EE, 0x2EE),
    (0x370, 0x374),
    (0x376, 0x377),
    (0x37A, 0x37D),
    (0x37F, 0x37F),
    (0x386, 0x386),
    (0x388, 0x38A),
    (0x38C, 0x38C),
    (0x38E, 0x3A1),
    (0x3A3, 0x3F5),
    (0x3F7, 0x481),
    (0x48A, 0x52F),
    (0x531, 0x556),
    (0x559, 0x559),
    (0x560, 0x588),
    (0x5D0, 0x5EA),
    (0x5EF, 0x5F2),
    (0x620, 0x64A),
    (0x66E, 0x66F),
    (0x671, 0x6D3),
    (0x6D5, 0x6D5),
    (0x6E5, 0x6E6),
    (0x6EE, 0x6EF),
    (0x6FA, 0x6FC),
    (0x6FF, 0x6FF),
    (0x710, 0x710),
    (0x712, 0x72F),
    (0x74D, 0x7A5),
    (0x7B1, 0x7B1),
    (0x7CA, 0x7EA),
    (0x7F4, 0x7F5),
    (0x7FA, 0x7FA),
    (0x800, 0x815),
    (0x81A, 0x81A),
    (0x824, 0x824),
    (0x828, 0x828),
    (0x840, 0x858),
    (0x860, 0x86A),
    (0x870, 0x887),
    (0x889, 0x88F),
    (0x8A0, 0x8C9),
    (0x904, 0x939),
    (0x93D, 0x93D),
    (0x950, 0x950),
    (0x958, 0x961),
    (0x971, 0x980),
    (0x985, 0x98C),
    (0x98F, 0x990),
    (0x993, 0x9A8),
    (0x9AA, 0x9B0),
    (0x9B2, 0x9B2),
    (0x9B6, 0x9B9),
    (0x9BD, 0x9BD),
    (0x9CE, 0x9CE),
    (0x9DC, 0x9DD),
    (0x9DF, 0x9E1),
    (0x9F0, 0x9F1),
    (0x9FC, 0x9FC),
    (0xA05, 0xA0A),
    (0xA0F, 0xA10),
    (0xA13, 0xA28),
    (0xA2A, 0xA30),
    (0xA32, 0xA33),
    (0xA35, 0xA36),
    (0xA38, 0xA39),
    (0xA59, 0xA5C),
    (0xA5E, 0xA5E),
    (0xA72, 0xA74),
    (0xA85, 0xA8D),
    (0xA8F, 0xA91),
    (0xA93, 0xAA8),
    (0xAAA, 0xAB0),
    (0xAB2, 0xAB3),
    (0xAB5, 0xAB9),
    (0xABD, 0xABD),
    (0xAD0, 0xAD0),
    (0xAE0, 0xAE1),
    (0xAF9, 0xAF9),
    (0xB05, 0xB0C),
    (0xB0F, 0xB10),
    (0xB13, 0xB28),
    (0xB2A, 0xB30),
    (0xB32, 0xB33),
    (0xB35, 0xB39),
    (0xB3D, 0xB3D),
    (0xB5C, 0xB5D),
    (0xB5F, 0xB61),
    (0xB71, 0xB71),
    (0xB83, 0xB83),
    (0xB85, 0xB8A),
    (0xB8E, 0xB90),
    (0xB92, 0xB95),
    (0xB99, 0xB9A),
    (0xB9C, 0xB9C),
    (0xB9E, 0xB9F),
    (0xBA3, 0xBA4),
    (0xBA8, 0xBAA),
    (0xBAE, 0xBB9),
    (0xBD0, 0xBD0),
    (0xC05, 0xC0C),
    (0xC0E, 0xC10),
    (0xC12, 0xC28),
    (0xC2A, 0xC39),
    (0xC3D, 0xC3D),
    (0xC58, 0xC5A),
    (0xC5C, 0xC5D),
    (0xC60, 0xC61),
    (0xC80, 0xC80),
    (0xC85, 0xC8C),
    (0xC8E, 0xC90),
    (0xC92, 0xCA8),
    (0xCAA, 0xCB3),
    (0xCB5, 0xCB9),
    (0xCBD, 0xCBD),
    (0xCDC, 0xCDE),
    (0xCE0, 0xCE1),
    (0xCF1, 0xCF2),
    (0xD04, 0xD0C),
    (0xD0E, 0xD10),
    (0xD12, 0xD3A),
    (0xD3D, 0xD3D),
    (0xD4E, 0xD4E),
    (0xD54, 0xD56),
    (0xD5F, 0xD61),
    (0xD7A, 0xD7F),
    (0xD85, 0xD96),
    (0xD9A, 0xDB1),
    (0xDB3, 0xDBB),
    (0xDBD, 0xDBD),
    (0xDC0, 0xDC6),
    (0xE01, 0xE30),
    (0xE32, 0xE33),
    (0xE40, 0xE46),
    (0xE81, 0xE82),
    (0xE84, 0xE84),
    (0xE86, 0xE8A),
    (0xE8C, 0xEA3),
    (0xEA5, 0xEA5),
    (0xEA7, 0xEB0),
    (0xEB2, 0xEB3),
    (0xEBD, 0xEBD),
    (0xEC0, 0xEC4),
    (0xEC6, 0xEC6),
    (0xEDC, 0xEDF),
    (0xF00, 0xF00),
    (0xF40, 0xF47),
    (0xF49, 0xF6C),
    (0xF88, 0xF8C),
    (0x1000, 0x102A),
    (0x103F, 0x103F),
    (0x1050, 0x1055),
    (0x105A, 0x105D),
    (0x1061, 0x1061),
    (0x1065, 0x1066),
    (0x106E, 0x1070),
    (0x1075, 0x1081),
    (0x108E, 0x108E),
    (0x10A0, 0x10C5),
    (0x10C7, 0x10C7),
    (0x10CD, 0x10CD),
    (0x10D0, 0x10FA),
    (0x10FC, 0x1248),
    (0x124A, 0x124D),
    (0x1250, 0x1256),
    (0x1258, 0x1258),
    (0x125A, 0x125D),
    (0x1260, 0x1288),
    (0x128A, 0x128D),
    (0x1290, 0x12B0),
    (0x12B2, 0x12B5),
    (0x12B8, 0x12BE),
    (0x12C0, 0x12C0),
    (0x12C2, 0x12C5),
    (0x12C8, 0x12D6),
    (0x12D8, 0x1310),
    (0x1312, 0x1315),
    (0x1318, 0x135A),
    (0x1380, 0x138F),
    (0x13A0, 0x13F5),
    (0x13F8, 0x13FD),
    (0x1401, 0x166C),
    (0x166F, 0x167F),
    (0x1681, 0x169A),
    (0x16A0, 0x16EA),
    (0x16F1, 0x16F8),
    (0x1700, 0x1711),
    (0x171F, 0x1731),
    (0x1740, 0x1751),
    (0x1760, 0x176C),
    (0x176E, 0x1770),
    (0x1780, 0x17B3),
    (0x17D7, 0x17D7),
    (0x17DC, 0x17DC),
    (0x1820, 0x1878),
    (0x1880, 0x1884),
    (0x1887, 0x18A8),
    (0x18AA, 0x18AA),
    (0x18B0, 0x18F5),
    (0x1900, 0x191E),
    (0x1950, 0x196D),
    (0x1970, 0x1974),
    (0x1980, 0x19AB),
    (0x19B0, 0x19C9),
    (0x1A00, 0x1A16),
    (0x1A20, 0x1A54),
    (0x1AA7, 0x1AA7),
    (0x1B05, 0x1B33),
    (0x1B45, 0x1B4C),
    (0x1B83, 0x1BA0),
    (0x1BAE, 0x1BAF),
    (0x1BBA, 0x1BE5),
    (0x1C00, 0x1C23),
    (0x1C4D, 0x1C4F),
    (0x1C5A, 0x1C7D),
    (0x1C80, 0x1C8A),
    (0x1C90, 0x1CBA),
    (0x1CBD, 0x1CBF),
    (0x1CE9, 0x1CEC),
    (0x1CEE, 0x1CF3),
    (0x1CF5, 0x1CF6),
    (0x1CFA, 0x1CFA),
    (0x1D00, 0x1DBF),
    (0x1E00, 0x1F15),
    (0x1F18, 0x1F1D),
    (0x1F20, 0x1F45),
    (0x1F48, 0x1F4D),
    (0x1F50, 0x1F57),
    (0x1F59, 0x1F59),
    (0x1F5B, 0x1F5B),
    (0x1F5D, 0x1F5D),
    (0x1F5F, 0x1F7D),
    (0x1F80, 0x1FB4),
    (0x1FB6, 0x1FBC),
    (0x1FBE, 0x1FBE),
    (0x1FC2, 0x1FC4),
    (0x1FC6, 0x1FCC),
    (0x1FD0, 0x1FD3),
    (0x1FD6, 0x1FDB),
    (0x1FE0, 0x1FEC),
    (0x1FF2, 0x1FF4),
    (0x1FF6, 0x1FFC),
    (0x2071, 0x2071),
    (0x207F, 0x207F),
    (0x2090, 0x209C),
    (0x2102, 0x2102),
    (0x2107, 0x2107),
    (0x210A, 0x2113),
    (0x2115, 0x2115),
    (0x2119, 0x211D),
    (0x2124, 0x2124),
    (0x2126, 0x2126),
    (0x2128, 0x2128),
    (0x212A, 0x212D),
    (0x212F, 0x2139),
    (0x213C, 0x213F),
    (0x2145, 0x2149),
    (0x214E, 0x214E),
    (0x2183, 0x2184),
    (0x2C00, 0x2CE4),
    (0x2CEB, 0x2CEE),
    (0x2CF2, 0x2CF3),
    (0x2D00, 0x2D25),
    (0x2D27, 0x2D27),
    (0x2D2D, 0x2D2D),
    (0x2D30, 0x2D67),
    (0x2D6F, 0x2D6F),
    (0x2D80, 0x2D96),
    (0x2DA0, 0x2DA6),
    (0x2DA8, 0x2DAE),
    (0x2DB0, 0x2DB6),
    (0x2DB8, 0x2DBE),
    (0x2DC0, 0x2DC6),
    (0x2DC8, 0x2DCE),
    (0x2DD0, 0x2DD6),
    (0x2DD8, 0x2DDE),
    (0x2E2F, 0x2E2F),
    (0x3005, 0x3006),
    (0x3031, 0x3035),
    (0x303B, 0x303C),
    (0x3041, 0x3096),
    (0x309D, 0x309F),
    (0x30A1, 0x30FA),
    (0x30FC, 0x30FF),
    (0x3105, 0x312F),
    (0x3131, 0x318E),
    (0x31A0, 0x31BF),
    (0x31F0, 0x31FF),
    (0x3400, 0x4DBF),
    (0x4E00, 0xA48C),
    (0xA4D0, 0xA4FD),
    (0xA500, 0xA60C),
    (0xA610, 0xA61F),
    (0xA62A, 0xA62B),
    (0xA640, 0xA66E),
    (0xA67F, 0xA69D),
    (0xA6A0, 0xA6E5),
    (0xA717, 0xA71F),
    (0xA722, 0xA788),
    (0xA78B, 0xA7DC),
    (0xA7F1, 0xA801),
    (0xA803, 0xA805),
    (0xA807, 0xA80A),
    (0xA80C, 0xA822),
    (0xA840, 0xA873),
    (0xA882, 0xA8B3),
    (0xA8F2, 0xA8F7),
    (0xA8FB, 0xA8FB),
    (0xA8FD, 0xA8FE),
    (0xA90A, 0xA925),
    (0xA930, 0xA946),
    (0xA960, 0xA97C),
    (0xA984, 0xA9B2),
    (0xA9CF, 0xA9CF),
    (0xA9E0, 0xA9E4),
    (0xA9E6, 0xA9EF),
    (0xA9FA, 0xA9FE),
    (0xAA00, 0xAA28),
    (0xAA40, 0xAA42),
    (0xAA44, 0xAA4B),
    (0xAA60, 0xAA76),
    (0xAA7A, 0xAA7A),
    (0xAA7E, 0xAAAF),
    (0xAAB1, 0xAAB1),
    (0xAAB5, 0xAAB6),
    (0xAAB9, 0xAABD),
    (0xAAC0, 0xAAC0),
    (0xAAC2, 0xAAC2),
    (0xAADB, 0xAADD),
    (0xAAE0, 0xAAEA),
    (0xAAF2, 0xAAF4),
    (0xAB01, 0xAB06),
    (0xAB09, 0xAB0E),
    (0xAB11, 0xAB16),
    (0xAB20, 0xAB26),
    (0xAB28, 0xAB2E),
    (0xAB30, 0xAB5A),
    (0xAB5C, 0xAB69),
    (0xAB70, 0xABE2),
    (0xAC00, 0xD7A3),
    (0xD7B0, 0xD7C6),
    (0xD7CB, 0xD7FB),
    (0xF900, 0xFA6D),
    (0xFA70, 0xFAD9),
    (0xFB00, 0xFB06),
    (0xFB13, 0xFB17),
    (0xFB1D, 0xFB1D),
    (0xFB1F, 0xFB28),
    (0xFB2A, 0xFB36),
    (0xFB38, 0xFB3C),
    (0xFB3E, 0xFB3E),
    (0xFB40, 0xFB41),
    (0xFB43, 0xFB44),
    (0xFB46, 0xFBB1),
    (0xFBD3, 0xFD3D),
    (0xFD50, 0xFD8F),
    (0xFD92, 0xFDC7),
    (0xFDF0, 0xFDFB),
    (0xFE70, 0xFE74),
    (0xFE76, 0xFEFC),
    (0xFF21, 0xFF3A),
    (0xFF41, 0xFF5A),
    (0xFF66, 0xFFBE),
    (0xFFC2, 0xFFC7),
    (0xFFCA, 0xFFCF),
    (0xFFD2, 0xFFD7),
    (0xFFDA, 0xFFDC),
    (0x10000, 0x1000B),
    (0x1000D, 0x10026),
    (0x10028, 0x1003A),
    (0x1003C, 0x1003D),
    (0x1003F, 0x1004D),
    (0x10050, 0x1005D),
    (0x10080, 0x100FA),
    (0x10280, 0x1029C),
    (0x102A0, 0x102D0),
    (0x10300, 0x1031F),
    (0x1032D, 0x10340),
    (0x10342, 0x10349),
    (0x10350, 0x10375),
    (0x10380, 0x1039D),
    (0x103A0, 0x103C3),
    (0x103C8, 0x103CF),
    (0x10400, 0x1049D),
    (0x104B0, 0x104D3),
    (0x104D8, 0x104FB),
    (0x10500, 0x10527),
    (0x10530, 0x10563),
    (0x10570, 0x1057A),
    (0x1057C, 0x1058A),
    (0x1058C, 0x10592),
    (0x10594, 0x10595),
    (0x10597, 0x105A1),
    (0x105A3, 0x105B1),
    (0x105B3, 0x105B9),
    (0x105BB, 0x105BC),
    (0x105C0, 0x105F3),
    (0x10600, 0x10736),
    (0x10740, 0x10755),
    (0x10760, 0x10767),
    (0x10780, 0x10785),
    (0x10787, 0x107B0),
    (0x107B2, 0x107BA),
    (0x10800, 0x10805),
    (0x10808, 0x10808),
    (0x1080A, 0x10835),
    (0x10837, 0x10838),
    (0x1083C, 0x1083C),
    (0x1083F, 0x10855),
    (0x10860, 0x10876),
    (0x10880, 0x1089E),
    (0x108E0, 0x108F2),
    (0x108F4, 0x108F5),
    (0x10900, 0x10915),
    (0x10920, 0x10939),
    (0x10940, 0x10959),
    (0x10980, 0x109B7),
    (0x109BE, 0x109BF),
    (0x10A00, 0x10A00),
    (0x10A10, 0x10A13),
    (0x10A15, 0x10A17),
    (0x10A19, 0x10A35),
    (0x10A60, 0x10A7C),
    (0x10A80, 0x10A9C),
    (0x10AC0, 0x10AC7),
    (0x10AC9, 0x10AE4),
    (0x10B00, 0x10B35),
    (0x10B40, 0x10B55),
    (0x10B60, 0x10B72),
    (0x10B80, 0x10B91),
    (0x10C00, 0x10C48),
    (0x10C80, 0x10CB2),
    (0x10CC0, 0x10CF2),
    (0x10D00, 0x10D23),
    (0x10D4A, 0x10D65),
    (0x10D6F, 0x10D85),
    (0x10E80, 0x10EA9),
    (0x10EB0, 0x10EB1),
    (0x10EC2, 0x10EC7),
    (0x10F00, 0x10F1C),
    (0x10F27, 0x10F27),
    (0x10F30, 0x10F45),
    (0x10F70, 0x10F81),
    (0x10FB0, 0x10FC4),
    (0x10FE0, 0x10FF6),
    (0x11003, 0x11037),
    (0x11071, 0x11072),
    (0x11075, 0x11075),
    (0x11083, 0x110AF),
    (0x110D0, 0x110E8),
    (0x11103, 0x11126),
    (0x11144, 0x11144),
    (0x11147, 0x11147),
    (0x11150, 0x11172),
    (0x11176, 0x11176),
    (0x11183, 0x111B2),
    (0x111C1, 0x111C4),
    (0x111DA, 0x111DA),
    (0x111DC, 0x111DC),
    (0x11200, 0x11211),
    (0x11213, 0x1122B),
    (0x1123F, 0x11240),
    (0x11280, 0x11286),
    (0x11288, 0x11288),
    (0x1128A, 0x1128D),
    (0x1128F, 0x1129D),
    (0x1129F, 0x112A8),
    (0x112B0, 0x112DE),
    (0x11305, 0x1130C),
    (0x1130F, 0x11310),
    (0x11313, 0x11328),
    (0x1132A, 0x11330),
    (0x11332, 0x11333),
    (0x11335, 0x11339),
    (0x1133D, 0x1133D),
    (0x11350, 0x11350),
    (0x1135D, 0x11361),
    (0x11380, 0x11389),
    (0x1138B, 0x1138B),
    (0x1138E, 0x1138E),
    (0x11390, 0x113B5),
    (0x113B7, 0x113B7),
    (0x113D1, 0x113D1),
    (0x113D3, 0x113D3),
    (0x11400, 0x11434),
    (0x11447, 0x1144A),
    (0x1145F, 0x11461),
    (0x11480, 0x114AF),
    (0x114C4, 0x114C5),
    (0x114C7, 0x114C7),
    (0x11580, 0x115AE),
    (0x115D8, 0x115DB),
    (0x11600, 0x1162F),
    (0x11644, 0x11644),
    (0x11680, 0x116AA),
    (0x116B8, 0x116B8),
    (0x11700, 0x1171A),
    (0x11740, 0x11746),
    (0x11800, 0x1182B),
    (0x118A0, 0x118DF),
    (0x118FF, 0x11906),
    (0x11909, 0x11909),
    (0x1190C, 0x11913),
    (0x11915, 0x11916),
    (0x11918, 0x1192F),
    (0x1193F, 0x1193F),
    (0x11941, 0x11941),
    (0x119A0, 0x119A7),
    (0x119AA, 0x119D0),
    (0x119E1, 0x119E1),
    (0x119E3, 0x119E3),
    (0x11A00, 0x11A00),
    (0x11A0B, 0x11A32),
    (0x11A3A, 0x11A3A),
    (0x11A50, 0x11A50),
    (0x11A5C, 0x11A89),
    (0x11A9D, 0x11A9D),
    (0x11AB0, 0x11AF8),
    (0x11BC0, 0x11BE0),
    (0x11C00, 0x11C08),
    (0x11C0A, 0x11C2E),
    (0x11C40, 0x11C40),
    (0x11C72, 0x11C8F),
    (0x11D00, 0x11D06),
    (0x11D08, 0x11D09),
    (0x11D0B, 0x11D30),
    (0x11D46, 0x11D46),
    (0x11D60, 0x11D65),
    (0x11D67, 0x11D68),
    (0x11D6A, 0x11D89),
    (0x11D98, 0x11D98),
    (0x11DB0, 0x11DDB),
    (0x11EE0, 0x11EF2),
    (0x11F02, 0x11F02),
    (0x11F04, 0x11F10),
    (0x11F12, 0x11F33),
    (0x11FB0, 0x11FB0),
    (0x12000, 0x12399),
    (0x12480, 0x12543),
    (0x12F90, 0x12FF0),
    (0x13000, 0x1342F),
    (0x13441, 0x13446),
    (0x13460, 0x143FA),
    (0x14400, 0x14646),
    (0x16100, 0x1611D),
    (0x16800, 0x16A38),
    (0x16A40, 0x16A5E),
    (0x16A70, 0x16ABE),
    (0x16AD0, 0x16AED),
    (0x16B00, 0x16B2F),
    (0x16B40, 0x16B43),
    (0x16B63, 0x16B77),
    (0x16B7D, 0x16B8F),
    (0x16D40, 0x16D6C),
    (0x16E40, 0x16E7F),
    (0x16EA0, 0x16EB8),
    (0x16EBB, 0x16ED3),
    (0x16F00, 0x16F4A),
    (0x16F50, 0x16F50),
    (0x16F93, 0x16F9F),
    (0x16FE0, 0x16FE1),
    (0x16FE3, 0x16FE3),
    (0x16FF2, 0x16FF3),
    (0x17000, 0x18CD5),
    (0x18CFF, 0x18D1E),
    (0x18D80, 0x18DF2),
    (0x1AFF0, 0x1AFF3),
    (0x1AFF5, 0x1AFFB),
    (0x1AFFD, 0x1AFFE),
    (0x1B000, 0x1B122),
    (0x1B132, 0x1B132),
    (0x1B150, 0x1B152),
    (0x1B155, 0x1B155),
    (0x1B164, 0x1B167),
    (0x1B170, 0x1B2FB),
    (0x1BC00, 0x1BC6A),
    (0x1BC70, 0x1BC7C),
    (0x1BC80, 0x1BC88),
    (0x1BC90, 0x1BC99),
    (0x1D400, 0x1D454),
    (0x1D456, 0x1D49C),
    (0x1D49E, 0x1D49F),
    (0x1D4A2, 0x1D4A2),
    (0x1D4A5, 0x1D4A6),
    (0x1D4A9, 0x1D4AC),
    (0x1D4AE, 0x1D4B9),
    (0x1D4BB, 0x1D4BB),
    (0x1D4BD, 0x1D4C3),
    (0x1D4C5, 0x1D505),
    (0x1D507, 0x1D50A),
    (0x1D50D, 0x1D514),
    (0x1D516, 0x1D51C),
    (0x1D51E, 0x1D539),
    (0x1D53B, 0x1D53E),
    (0x1D540, 0x1D544),
    (0x1D546, 0x1D546),
    (0x1D54A, 0x1D550),
    (0x1D552, 0x1D6A5),
    (0x1D6A8, 0x1D6C0),
    (0x1D6C2, 0x1D6DA),
    (0x1D6DC, 0x1D6FA),
    (0x1D6FC, 0x1D714),
    (0x1D716, 0x1D734),
    (0x1D736, 0x1D74E),
    (0x1D750, 0x1D76E),
    (0x1D770, 0x1D788),
    (0x1D78A, 0x1D7A8),
    (0x1D7AA, 0x1D7C2),
    (0x1D7C4, 0x1D7CB),
    (0x1DF00, 0x1DF1E),
    (0x1DF25, 0x1DF2A),
    (0x1E030, 0x1E06D),
    (0x1E100, 0x1E12C),
    (0x1E137, 0x1E13D),
    (0x1E14E, 0x1E14E),
    (0x1E290, 0x1E2AD),
    (0x1E2C0, 0x1E2EB),
    (0x1E4D0, 0x1E4EB),
    (0x1E5D0, 0x1E5ED),
    (0x1E5F0, 0x1E5F0),
    (0x1E6C0, 0x1E6DE),
    (0x1E6E0, 0x1E6E2),
    (0x1E6E4, 0x1E6E5),
    (0x1E6E7, 0x1E6ED),
    (0x1E6F0, 0x1E6F4),
    (0x1E6FE, 0x1E6FF),
    (0x1E7E0, 0x1E7E6),
    (0x1E7E8, 0x1E7EB),
    (0x1E7ED, 0x1E7EE),
    (0x1E7F0, 0x1E7FE),
    (0x1E800, 0x1E8C4),
    (0x1E900, 0x1E943),
    (0x1E94B, 0x1E94B),
    (0x1EE00, 0x1EE03),
    (0x1EE05, 0x1EE1F),
    (0x1EE21, 0x1EE22),
    (0x1EE24, 0x1EE24),
    (0x1EE27, 0x1EE27),
    (0x1EE29, 0x1EE32),
    (0x1EE34, 0x1EE37),
    (0x1EE39, 0x1EE39),
    (0x1EE3B, 0x1EE3B),
    (0x1EE42, 0x1EE42),
    (0x1EE47, 0x1EE47),
    (0x1EE49, 0x1EE49),
    (0x1EE4B, 0x1EE4B),
    (0x1EE4D, 0x1EE4F),
    (0x1EE51, 0x1EE52),
    (0x1EE54, 0x1EE54),
    (0x1EE57, 0x1EE57),
    (0x1EE59, 0x1EE59),
    (0x1EE5B, 0x1EE5B),
    (0x1EE5D, 0x1EE5D),
    (0x1EE5F, 0x1EE5F),
    (0x1EE61, 0x1EE62),
    (0x1EE64, 0x1EE64),
    (0x1EE67, 0x1EE6A),
    (0x1EE6C, 0x1EE72),
    (0x1EE74, 0x1EE77),
    (0x1EE79, 0x1EE7C),
    (0x1EE7E, 0x1EE7E),
    (0x1EE80, 0x1EE89),
    (0x1EE8B, 0x1EE9B),
    (0x1EEA1, 0x1EEA3),
    (0x1EEA5, 0x1EEA9),
    (0x1EEAB, 0x1EEBB),
    (0x20000, 0x2A6DF),
    (0x2A700, 0x2B81D),
    (0x2B820, 0x2CEAD),
    (0x2CEB0, 0x2EBE0),
    (0x2EBF0, 0x2EE5D),
    (0x2F800, 0x2FA1D),
    (0x30000, 0x3134A),
    (0x31350, 0x33479),
];

/// `\p{N}`：146 个区间。
static JS_NUMBER_RANGES: &[(u32, u32)] = &[
    (0x30, 0x39),
    (0xB2, 0xB3),
    (0xB9, 0xB9),
    (0xBC, 0xBE),
    (0x660, 0x669),
    (0x6F0, 0x6F9),
    (0x7C0, 0x7C9),
    (0x966, 0x96F),
    (0x9E6, 0x9EF),
    (0x9F4, 0x9F9),
    (0xA66, 0xA6F),
    (0xAE6, 0xAEF),
    (0xB66, 0xB6F),
    (0xB72, 0xB77),
    (0xBE6, 0xBF2),
    (0xC66, 0xC6F),
    (0xC78, 0xC7E),
    (0xCE6, 0xCEF),
    (0xD58, 0xD5E),
    (0xD66, 0xD78),
    (0xDE6, 0xDEF),
    (0xE50, 0xE59),
    (0xED0, 0xED9),
    (0xF20, 0xF33),
    (0x1040, 0x1049),
    (0x1090, 0x1099),
    (0x1369, 0x137C),
    (0x16EE, 0x16F0),
    (0x17E0, 0x17E9),
    (0x17F0, 0x17F9),
    (0x1810, 0x1819),
    (0x1946, 0x194F),
    (0x19D0, 0x19DA),
    (0x1A80, 0x1A89),
    (0x1A90, 0x1A99),
    (0x1B50, 0x1B59),
    (0x1BB0, 0x1BB9),
    (0x1C40, 0x1C49),
    (0x1C50, 0x1C59),
    (0x2070, 0x2070),
    (0x2074, 0x2079),
    (0x2080, 0x2089),
    (0x2150, 0x2182),
    (0x2185, 0x2189),
    (0x2460, 0x249B),
    (0x24EA, 0x24FF),
    (0x2776, 0x2793),
    (0x2CFD, 0x2CFD),
    (0x3007, 0x3007),
    (0x3021, 0x3029),
    (0x3038, 0x303A),
    (0x3192, 0x3195),
    (0x3220, 0x3229),
    (0x3248, 0x324F),
    (0x3251, 0x325F),
    (0x3280, 0x3289),
    (0x32B1, 0x32BF),
    (0xA620, 0xA629),
    (0xA6E6, 0xA6EF),
    (0xA830, 0xA835),
    (0xA8D0, 0xA8D9),
    (0xA900, 0xA909),
    (0xA9D0, 0xA9D9),
    (0xA9F0, 0xA9F9),
    (0xAA50, 0xAA59),
    (0xABF0, 0xABF9),
    (0xFF10, 0xFF19),
    (0x10107, 0x10133),
    (0x10140, 0x10178),
    (0x1018A, 0x1018B),
    (0x102E1, 0x102FB),
    (0x10320, 0x10323),
    (0x10341, 0x10341),
    (0x1034A, 0x1034A),
    (0x103D1, 0x103D5),
    (0x104A0, 0x104A9),
    (0x10858, 0x1085F),
    (0x10879, 0x1087F),
    (0x108A7, 0x108AF),
    (0x108FB, 0x108FF),
    (0x10916, 0x1091B),
    (0x109BC, 0x109BD),
    (0x109C0, 0x109CF),
    (0x109D2, 0x109FF),
    (0x10A40, 0x10A48),
    (0x10A7D, 0x10A7E),
    (0x10A9D, 0x10A9F),
    (0x10AEB, 0x10AEF),
    (0x10B58, 0x10B5F),
    (0x10B78, 0x10B7F),
    (0x10BA9, 0x10BAF),
    (0x10CFA, 0x10CFF),
    (0x10D30, 0x10D39),
    (0x10D40, 0x10D49),
    (0x10E60, 0x10E7E),
    (0x10F1D, 0x10F26),
    (0x10F51, 0x10F54),
    (0x10FC5, 0x10FCB),
    (0x11052, 0x1106F),
    (0x110F0, 0x110F9),
    (0x11136, 0x1113F),
    (0x111D0, 0x111D9),
    (0x111E1, 0x111F4),
    (0x112F0, 0x112F9),
    (0x11450, 0x11459),
    (0x114D0, 0x114D9),
    (0x11650, 0x11659),
    (0x116C0, 0x116C9),
    (0x116D0, 0x116E3),
    (0x11730, 0x1173B),
    (0x118E0, 0x118F2),
    (0x11950, 0x11959),
    (0x11BF0, 0x11BF9),
    (0x11C50, 0x11C6C),
    (0x11D50, 0x11D59),
    (0x11DA0, 0x11DA9),
    (0x11DE0, 0x11DE9),
    (0x11F50, 0x11F59),
    (0x11FC0, 0x11FD4),
    (0x12400, 0x1246E),
    (0x16130, 0x16139),
    (0x16A60, 0x16A69),
    (0x16AC0, 0x16AC9),
    (0x16B50, 0x16B59),
    (0x16B5B, 0x16B61),
    (0x16D70, 0x16D79),
    (0x16E80, 0x16E96),
    (0x16FF4, 0x16FF6),
    (0x1CCF0, 0x1CCF9),
    (0x1D2C0, 0x1D2D3),
    (0x1D2E0, 0x1D2F3),
    (0x1D360, 0x1D378),
    (0x1D7CE, 0x1D7FF),
    (0x1E140, 0x1E149),
    (0x1E2F0, 0x1E2F9),
    (0x1E4F0, 0x1E4F9),
    (0x1E5F1, 0x1E5FA),
    (0x1E8C7, 0x1E8CF),
    (0x1E950, 0x1E959),
    (0x1EC71, 0x1ECAB),
    (0x1ECAD, 0x1ECAF),
    (0x1ECB1, 0x1ECB4),
    (0x1ED01, 0x1ED2D),
    (0x1ED2F, 0x1ED3D),
    (0x1F100, 0x1F10C),
    (0x1FBF0, 0x1FBF9),
];

/// 有序区间表的二分查找。
/// 在**升序且互不重叠**的闭区间表上二分查找 `value`。
///
/// 前提：`ranges` 按起点升序排列（由生成脚本合并相邻码点得到，见 [`JS_LETTER_RANGES`] /
/// [`JS_NUMBER_RANGES`] 的注释）。表不满足该前提时返回值无意义。
fn range_contains(ranges: &[(u32, u32)], value: u32) -> bool {
    let mut low = 0usize;
    let mut high = ranges.len();
    while low < high {
        let middle = (low + high) / 2;
        let (start, end) = ranges[middle];
        if value < start {
            high = middle;
        } else if value > end {
            low = middle + 1;
        } else {
            return true;
        }
    }
    false
}

/// JS `\p{L}`（`Character.isLetter` 的对照物，Unicode 17.0）。
fn is_js_letter(character: char) -> bool {
    range_contains(JS_LETTER_RANGES, character as u32)
}

/// JS `\p{N}`（Unicode 17.0）。
fn is_js_number(character: char) -> bool {
    range_contains(JS_NUMBER_RANGES, character as u32)
}

/// `^[\p{L}\p{N}.]+$`（`formatFraction` / `formatRoot` 的"简单值"判定）。
fn is_simple_letters_numbers(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character == '.' || is_js_letter(character) || is_js_number(character))
}

/// `^[\p{N}.]+$`。
fn is_simple_numbers(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character == '.' || is_js_number(character))
}

// ============================================================================
// 符号 / 命令表（表结构与 TS 源一致、顺序与 JS 对象字面量一致）
// ============================================================================

/// `SYMBOLS`（224 条）：`\name` → 字形。
pub static SYMBOLS: &[(&str, &str)] = &[
    ("alpha", "\u{3b1}"),
    ("beta", "\u{3b2}"),
    ("gamma", "\u{3b3}"),
    ("delta", "\u{3b4}"),
    ("epsilon", "\u{3f5}"),
    ("varepsilon", "\u{3b5}"),
    ("zeta", "\u{3b6}"),
    ("eta", "\u{3b7}"),
    ("theta", "\u{3b8}"),
    ("vartheta", "\u{3d1}"),
    ("iota", "\u{3b9}"),
    ("kappa", "\u{3ba}"),
    ("varkappa", "\u{3f0}"),
    ("lambda", "\u{3bb}"),
    ("mu", "\u{3bc}"),
    ("nu", "\u{3bd}"),
    ("xi", "\u{3be}"),
    ("pi", "\u{3c0}"),
    ("varpi", "\u{3d6}"),
    ("rho", "\u{3c1}"),
    ("varrho", "\u{3f1}"),
    ("sigma", "\u{3c3}"),
    ("varsigma", "\u{3c2}"),
    ("tau", "\u{3c4}"),
    ("upsilon", "\u{3c5}"),
    ("phi", "\u{3d5}"),
    ("varphi", "\u{3c6}"),
    ("chi", "\u{3c7}"),
    ("psi", "\u{3c8}"),
    ("omega", "\u{3c9}"),
    ("Gamma", "\u{393}"),
    ("Delta", "\u{394}"),
    ("Theta", "\u{398}"),
    ("Lambda", "\u{39b}"),
    ("Xi", "\u{39e}"),
    ("Pi", "\u{3a0}"),
    ("Sigma", "\u{3a3}"),
    ("Upsilon", "\u{3a5}"),
    ("Phi", "\u{3a6}"),
    ("Psi", "\u{3a8}"),
    ("Omega", "\u{3a9}"),
    ("pm", "\u{b1}"),
    ("mp", "\u{2213}"),
    ("times", "\u{d7}"),
    ("div", "\u{f7}"),
    ("cdot", "\u{b7}"),
    ("ast", "\u{2217}"),
    ("star", "\u{22c6}"),
    ("circ", "\u{2218}"),
    ("bullet", "\u{2022}"),
    ("oplus", "\u{2295}"),
    ("ominus", "\u{2296}"),
    ("otimes", "\u{2297}"),
    ("oslash", "\u{2298}"),
    ("odot", "\u{2299}"),
    ("bigcirc", "\u{25cb}"),
    ("dagger", "\u{2020}"),
    ("ddagger", "\u{2021}"),
    ("amalg", "\u{2a3f}"),
    ("uplus", "\u{228e}"),
    ("sqcap", "\u{2293}"),
    ("sqcup", "\u{2294}"),
    ("bowtie", "\u{22c8}"),
    ("Join", "\u{22c8}"),
    ("ltimes", "\u{22c9}"),
    ("rtimes", "\u{22ca}"),
    ("leftouterjoin", "\u{27d5}"),
    ("rightouterjoin", "\u{27d6}"),
    ("fullouterjoin", "\u{27d7}"),
    ("triangleleft", "\u{25c1}"),
    ("triangleright", "\u{25b7}"),
    ("wr", "\u{2240}"),
    ("cap", "\u{2229}"),
    ("cup", "\u{222a}"),
    ("bigcap", "\u{22c2}"),
    ("bigcup", "\u{22c3}"),
    ("bigwedge", "\u{22c0}"),
    ("bigvee", "\u{22c1}"),
    ("bigsqcup", "\u{2a06}"),
    ("biguplus", "\u{2a04}"),
    ("bigoplus", "\u{2a01}"),
    ("bigotimes", "\u{2a02}"),
    ("bigodot", "\u{2a00}"),
    ("setminus", "\u{2216}"),
    ("in", "\u{2208}"),
    ("notin", "\u{2209}"),
    ("ni", "\u{220b}"),
    ("subset", "\u{2282}"),
    ("supset", "\u{2283}"),
    ("subseteq", "\u{2286}"),
    ("supseteq", "\u{2287}"),
    ("sqsubset", "\u{228f}"),
    ("sqsupset", "\u{2290}"),
    ("sqsubseteq", "\u{2291}"),
    ("sqsupseteq", "\u{2292}"),
    ("prec", "\u{227a}"),
    ("preceq", "\u{227c}"),
    ("succ", "\u{227b}"),
    ("succeq", "\u{227d}"),
    ("ll", "\u{226a}"),
    ("gg", "\u{226b}"),
    ("le", "\u{2264}"),
    ("leq", "\u{2264}"),
    ("leqslant", "\u{2264}"),
    ("ge", "\u{2265}"),
    ("geq", "\u{2265}"),
    ("geqslant", "\u{2265}"),
    ("ne", "\u{2260}"),
    ("neq", "\u{2260}"),
    ("equiv", "\u{2261}"),
    ("approx", "\u{2248}"),
    ("sim", "\u{223c}"),
    ("simeq", "\u{2243}"),
    ("cong", "\u{2245}"),
    ("asymp", "\u{224d}"),
    ("doteq", "\u{2250}"),
    ("propto", "\u{221d}"),
    ("parallel", "\u{2225}"),
    ("perp", "\u{22a5}"),
    ("mid", "\u{2223}"),
    ("vdash", "\u{22a2}"),
    ("dashv", "\u{22a3}"),
    ("models", "\u{22a8}"),
    ("Vdash", "\u{22a9}"),
    ("Vvdash", "\u{22aa}"),
    ("nvdash", "\u{22ac}"),
    ("nvDash", "\u{22ad}"),
    ("forall", "\u{2200}"),
    ("exists", "\u{2203}"),
    ("nexists", "\u{2204}"),
    ("neg", "\u{ac}"),
    ("land", "\u{2227}"),
    ("wedge", "\u{2227}"),
    ("lor", "\u{2228}"),
    ("vee", "\u{2228}"),
    ("to", "\u{2192}"),
    ("rightarrow", "\u{2192}"),
    ("longrightarrow", "\u{2192}"),
    ("leftarrow", "\u{2190}"),
    ("longleftarrow", "\u{2190}"),
    ("gets", "\u{2190}"),
    ("leftrightarrow", "\u{2194}"),
    ("longleftrightarrow", "\u{2194}"),
    ("hookleftarrow", "\u{21a9}"),
    ("hookrightarrow", "\u{21aa}"),
    ("twoheadleftarrow", "\u{219e}"),
    ("twoheadrightarrow", "\u{21a0}"),
    ("leftharpoonup", "\u{21bc}"),
    ("leftharpoondown", "\u{21bd}"),
    ("rightharpoonup", "\u{21c0}"),
    ("rightharpoondown", "\u{21c1}"),
    ("rightleftharpoons", "\u{21cc}"),
    ("leftrightharpoons", "\u{21cb}"),
    ("nearrow", "\u{2197}"),
    ("searrow", "\u{2198}"),
    ("swarrow", "\u{2199}"),
    ("nwarrow", "\u{2196}"),
    ("rightsquigarrow", "\u{21dd}"),
    ("leadsto", "\u{21dd}"),
    ("Rightarrow", "\u{21d2}"),
    ("Longrightarrow", "\u{21d2}"),
    ("Leftarrow", "\u{21d0}"),
    ("Longleftarrow", "\u{21d0}"),
    ("Leftrightarrow", "\u{21d4}"),
    ("Longleftrightarrow", "\u{21d4}"),
    ("implies", "\u{21d2}"),
    ("iff", "\u{21d4}"),
    ("mapsto", "\u{21a6}"),
    ("longmapsto", "\u{21a6}"),
    ("uparrow", "\u{2191}"),
    ("downarrow", "\u{2193}"),
    ("partial", "\u{2202}"),
    ("nabla", "\u{2207}"),
    ("int", "\u{222b}"),
    ("iint", "\u{222c}"),
    ("iiint", "\u{222d}"),
    ("oint", "\u{222e}"),
    ("sum", "\u{2211}"),
    ("prod", "\u{220f}"),
    ("coprod", "\u{2210}"),
    ("infty", "\u{221e}"),
    ("emptyset", "\u{2205}"),
    ("varnothing", "\u{2205}"),
    ("angle", "\u{2220}"),
    ("therefore", "\u{2234}"),
    ("because", "\u{2235}"),
    ("aleph", "\u{2135}"),
    ("beth", "\u{2136}"),
    ("gimel", "\u{2137}"),
    ("daleth", "\u{2138}"),
    ("top", "\u{22a4}"),
    ("bot", "\u{22a5}"),
    ("triangle", "\u{25b3}"),
    ("square", "\u{25a1}"),
    ("lozenge", "\u{25ca}"),
    ("checkmark", "\u{2713}"),
    ("complement", "\u{2201}"),
    ("wp", "\u{2118}"),
    ("prime", "\u{2032}"),
    ("ldots", "\u{2026}"),
    ("dots", "\u{2026}"),
    ("cdots", "\u{22ef}"),
    ("vdots", "\u{22ee}"),
    ("ddots", "\u{22f1}"),
    ("ell", "\u{2113}"),
    ("hbar", "\u{210f}"),
    ("Im", "\u{2111}"),
    ("Re", "\u{211c}"),
    ("langle", "\u{27e8}"),
    ("rangle", "\u{27e9}"),
    ("vert", "|"),
    ("lvert", "|"),
    ("rvert", "|"),
    ("Vert", "\u{2016}"),
    ("lVert", "\u{2016}"),
    ("rVert", "\u{2016}"),
    ("lbrace", "{"),
    ("rbrace", "}"),
    ("backslash", "\\"),
    ("lfloor", "\u{230a}"),
    ("rfloor", "\u{230b}"),
    ("lceil", "\u{2308}"),
    ("rceil", "\u{2309}"),
    ("colon", ":"),
];

/// `NAMED_OPERATORS`（32）：按文本输出的算子名（`sin`、`lim` …）。
static NAMED_OPERATORS: &[&str] = &[
    "arccos", "arcsin", "arctan", "arg", "cos", "cosh", "cot", "coth", "csc", "deg", "det", "dim",
    "exp", "gcd", "hom", "inf", "ker", "lg", "lim", "liminf", "limsup", "ln", "log", "max", "min",
    "Pr", "sec", "sin", "sinh", "sup", "tan", "tanh",
];

/// `LIMIT_OPERATORS`（11）：`\lim` 等可带上/下标的算子。
static LIMIT_OPERATORS: &[&str] = &[
    "argmax", "argmin", "inf", "injlim", "lim", "liminf", "limsup", "max", "min", "projlim", "sup",
];

/// `DISPLAY_LIMIT_SYMBOLS`（16）：display 模式下上/下标纵向堆叠的符号。
static DISPLAY_LIMIT_SYMBOLS: &[&str] = &[
    "bigcap",
    "bigcup",
    "bigodot",
    "bigoplus",
    "bigotimes",
    "bigsqcup",
    "biguplus",
    "bigvee",
    "bigwedge",
    "coprod",
    "int",
    "iint",
    "iiint",
    "oint",
    "prod",
    "sum",
];

/// `RELATION_COMMANDS`（88）：两侧加空格的关系符。
static RELATION_COMMANDS: &[&str] = &[
    "Leftarrow",
    "Leftrightarrow",
    "Longleftarrow",
    "Longleftrightarrow",
    "Longrightarrow",
    "Rightarrow",
    "Join",
    "Vdash",
    "Vvdash",
    "approx",
    "asymp",
    "bowtie",
    "cong",
    "dashv",
    "fullouterjoin",
    "doteq",
    "downarrow",
    "equiv",
    "ge",
    "geq",
    "geqslant",
    "gets",
    "gg",
    "hookleftarrow",
    "hookrightarrow",
    "iff",
    "implies",
    "in",
    "leadsto",
    "le",
    "leftarrow",
    "leftharpoondown",
    "leftharpoonup",
    "leftrightarrow",
    "leftrightharpoons",
    "leftouterjoin",
    "leq",
    "leqslant",
    "ll",
    "longleftarrow",
    "longleftrightarrow",
    "longmapsto",
    "longrightarrow",
    "ltimes",
    "mapsto",
    "mid",
    "models",
    "ne",
    "nearrow",
    "neq",
    "ni",
    "notin",
    "nvdash",
    "nvDash",
    "nwarrow",
    "parallel",
    "perp",
    "prec",
    "preceq",
    "propto",
    "rightharpoondown",
    "rightharpoonup",
    "rightleftharpoons",
    "rightouterjoin",
    "rightarrow",
    "rightsquigarrow",
    "rtimes",
    "searrow",
    "sim",
    "simeq",
    "sqsubset",
    "sqsubseteq",
    "sqsupset",
    "sqsupseteq",
    "subset",
    "subseteq",
    "succ",
    "succeq",
    "supset",
    "supseteq",
    "swarrow",
    "to",
    "triangleleft",
    "triangleright",
    "twoheadleftarrow",
    "twoheadrightarrow",
    "uparrow",
    "vdash",
];

/// `NEGATED_SYMBOLS`（30）：`\not X` 的映射。
static NEGATED_SYMBOLS: &[(&str, &str)] = &[
    ("<", "\u{226e}"),
    (">", "\u{226f}"),
    ("=", "\u{2260}"),
    ("\u{2208}", "\u{2209}"),
    ("\u{220b}", "\u{220c}"),
    ("\u{2223}", "\u{2224}"),
    ("\u{2225}", "\u{2226}"),
    ("\u{223c}", "\u{2241}"),
    ("\u{2243}", "\u{2244}"),
    ("\u{2245}", "\u{2247}"),
    ("\u{2248}", "\u{2249}"),
    ("\u{2261}", "\u{2262}"),
    ("\u{2264}", "\u{2270}"),
    ("\u{2265}", "\u{2271}"),
    ("\u{227a}", "\u{2280}"),
    ("\u{227b}", "\u{2281}"),
    ("\u{2282}", "\u{2284}"),
    ("\u{2283}", "\u{2285}"),
    ("\u{2286}", "\u{2288}"),
    ("\u{2287}", "\u{2289}"),
    ("\u{22a2}", "\u{22ac}"),
    ("\u{22a8}", "\u{22ad}"),
    ("\u{2194}", "\u{21ae}"),
    ("\u{2190}", "\u{219a}"),
    ("\u{2192}", "\u{219b}"),
    ("\u{21d2}", "\u{21cf}"),
    ("\u{21d0}", "\u{21cd}"),
    ("\u{21d4}", "\u{21ce}"),
    ("\u{227c}", "\u{22e0}"),
    ("\u{227d}", "\u{22e1}"),
];

/// `BLACKBOARD`（7）：`\mathbb` 的字母映射。
static BLACKBOARD: &[(&str, &str)] = &[
    ("C", "\u{2102}"),
    ("H", "\u{210d}"),
    ("N", "\u{2115}"),
    ("P", "\u{2119}"),
    ("Q", "\u{211a}"),
    ("R", "\u{211d}"),
    ("Z", "\u{2124}"),
];

/// `SUPERSCRIPTS`（40）。
static SUPERSCRIPTS: &[(&str, &str)] = &[
    ("0", "\u{2070}"),
    ("1", "\u{b9}"),
    ("2", "\u{b2}"),
    ("3", "\u{b3}"),
    ("4", "\u{2074}"),
    ("5", "\u{2075}"),
    ("6", "\u{2076}"),
    ("7", "\u{2077}"),
    ("8", "\u{2078}"),
    ("9", "\u{2079}"),
    ("+", "\u{207a}"),
    ("-", "\u{207b}"),
    ("=", "\u{207c}"),
    ("(", "\u{207d}"),
    (")", "\u{207e}"),
    ("a", "\u{1d43}"),
    ("b", "\u{1d47}"),
    ("c", "\u{1d9c}"),
    ("d", "\u{1d48}"),
    ("e", "\u{1d49}"),
    ("f", "\u{1da0}"),
    ("g", "\u{1d4d}"),
    ("h", "\u{2b0}"),
    ("i", "\u{2071}"),
    ("j", "\u{2b2}"),
    ("k", "\u{1d4f}"),
    ("l", "\u{2e1}"),
    ("m", "\u{1d50}"),
    ("n", "\u{207f}"),
    ("o", "\u{1d52}"),
    ("p", "\u{1d56}"),
    ("r", "\u{2b3}"),
    ("s", "\u{2e2}"),
    ("t", "\u{1d57}"),
    ("u", "\u{1d58}"),
    ("v", "\u{1d5b}"),
    ("w", "\u{2b7}"),
    ("x", "\u{2e3}"),
    ("y", "\u{2b8}"),
    ("z", "\u{1dbb}"),
];

/// `SUBSCRIPTS`（32）。
static SUBSCRIPTS: &[(&str, &str)] = &[
    ("0", "\u{2080}"),
    ("1", "\u{2081}"),
    ("2", "\u{2082}"),
    ("3", "\u{2083}"),
    ("4", "\u{2084}"),
    ("5", "\u{2085}"),
    ("6", "\u{2086}"),
    ("7", "\u{2087}"),
    ("8", "\u{2088}"),
    ("9", "\u{2089}"),
    ("+", "\u{208a}"),
    ("-", "\u{208b}"),
    ("=", "\u{208c}"),
    ("(", "\u{208d}"),
    (")", "\u{208e}"),
    ("a", "\u{2090}"),
    ("e", "\u{2091}"),
    ("h", "\u{2095}"),
    ("i", "\u{1d62}"),
    ("j", "\u{2c7c}"),
    ("k", "\u{2096}"),
    ("l", "\u{2097}"),
    ("m", "\u{2098}"),
    ("n", "\u{2099}"),
    ("o", "\u{2092}"),
    ("p", "\u{209a}"),
    ("r", "\u{1d63}"),
    ("s", "\u{209b}"),
    ("t", "\u{209c}"),
    ("u", "\u{1d64}"),
    ("v", "\u{1d65}"),
    ("x", "\u{2093}"),
];

/// `SPACING_COMMANDS`（12）。
static SPACING_COMMANDS: &[&str] = &[
    ",",
    ":",
    ";",
    " ",
    ">",
    "enspace",
    "enskip",
    "medspace",
    "quad",
    "qquad",
    "thickspace",
    "thinspace",
];

/// `NEGATIVE_SPACING_COMMANDS`（4）。
static NEGATIVE_SPACING_COMMANDS: &[&str] = &["!", "negmedspace", "negthickspace", "negthinspace"];

/// `FONT_SWITCH_COMMANDS`（7）：吞掉后续空白的旧式字体切换。
static FONT_SWITCH_COMMANDS: &[&str] = &["bf", "cal", "it", "rm", "sf", "sl", "tt"];

/// `IGNORED_COMMANDS`（6）。
static IGNORED_COMMANDS: &[&str] = &[
    "displaystyle",
    "limits",
    "nolimits",
    "scriptstyle",
    "scriptscriptstyle",
    "textstyle",
];

/// `SIZE_COMMANDS`（12）。
static SIZE_COMMANDS: &[&str] = &[
    "big", "Big", "bigg", "Bigg", "bigl", "Bigl", "biggl", "Biggl", "bigr", "Bigr", "biggr",
    "Biggr",
];

/// `PLAIN_WRAPPERS`（30）：只取参数文本的包装命令。
static PLAIN_WRAPPERS: &[&str] = &[
    "emph",
    "mathcal",
    "mathbf",
    "mathfrak",
    "mathit",
    "mathrm",
    "mathnormal",
    "mathscr",
    "mathsf",
    "mathtt",
    "mathup",
    "mbox",
    "overbrace",
    "pmb",
    "smash",
    "substack",
    "text",
    "textbf",
    "textit",
    "textmd",
    "textnormal",
    "textrm",
    "textsc",
    "textsf",
    "textsl",
    "texttt",
    "textup",
    "underbrace",
    "bm",
    "boldsymbol",
];

/// `ACCENTS`（18）：组合附加符号。
static ACCENTS: &[(&str, &str)] = &[
    ("acute", "\u{301}"),
    ("bar", "\u{305}"),
    ("breve", "\u{306}"),
    ("check", "\u{30c}"),
    ("ddot", "\u{308}"),
    ("dot", "\u{307}"),
    ("grave", "\u{300}"),
    ("hat", "\u{302}"),
    ("mathring", "\u{30a}"),
    ("overleftarrow", "\u{20d6}"),
    ("overleftrightarrow", "\u{20e1}"),
    ("overline", "\u{305}"),
    ("overrightarrow", "\u{20d7}"),
    ("tilde", "\u{303}"),
    ("underline", "\u{332}"),
    ("vec", "\u{20d7}"),
    ("widehat", "\u{302}"),
    ("widetilde", "\u{303}"),
];

// ============================================================================
// 小型格式化函数
// ============================================================================

/// `replaceCharacters`：任一字符无映射 → `None`。
fn replace_characters(
    value: &str,
    replacements: &[(&'static str, &'static str)],
) -> Option<String> {
    let mut result = String::new();
    for character in value.chars() {
        result.push_str(script_char(replacements, character)?);
    }
    Some(result)
}

/// `normalizeScriptValue`：`value.trim().replace(/\s*([=+-])\s*/g, "$1")`。
///
/// JS 的 `\s` 是 [`is_js_whitespace`] 的 25 个码点（**不含** U+0085、**含** U+FEFF），
/// 与 Rust `regex` 的 `\s`（Unicode `White_Space`）恰好在这两个字符上相反，因此这里手写
/// 单遍替换，不用正则。语义与 `replace` 逐字相同：匹配 `\s* 运算符 \s*` 并把整段换成运算符，
/// 扫描从匹配末尾继续（因此 `a = = b` 里两个 `=` 之间的空白只归前一个匹配）。
fn normalize_script_value(value: &str) -> String {
    let characters: Vec<char> = trim_js(value).chars().collect();
    let mut result = String::with_capacity(characters.len());
    let mut index = 0usize;
    while index < characters.len() {
        let character = characters[index];
        if character == '=' || character == '+' || character == '-' {
            while result.chars().next_back().is_some_and(is_js_whitespace) {
                result.pop();
            }
            result.push(character);
            index += 1;
            while index < characters.len() && is_js_whitespace(characters[index]) {
                index += 1;
            }
            continue;
        }
        result.push(character);
        index += 1;
    }
    result
}

/// `formatUnicodeScript`：把规范化后的值逐字符映射成 Unicode 上下标。
///
/// 任一字符没有映射（[`SUPERSCRIPTS`] / [`SUBSCRIPTS`]）就返回 `None`，由调用方退回
/// `^x`/`_x` 形式。
fn format_unicode_script(value: &str, kind: ScriptKind) -> Option<String> {
    let normalized = normalize_script_value(value);
    replace_characters(
        &normalized,
        match kind {
            ScriptKind::Sub => SUBSCRIPTS,
            ScriptKind::Sup => SUPERSCRIPTS,
        },
    )
}

/// `formatScript`：Unicode 上下标不可用时的单行退路。
///
/// 单个字符，或（仅下标）由纯 ASCII 字母组成的值，写成 `^x` / `_x`；其余加括号，写成
/// `^(...)` / `_(...)`。这个"什么时候加括号"的判定是可观察行为。
fn format_script(value: &str, kind: ScriptKind) -> String {
    let normalized = normalize_script_value(value);
    if let Some(unicode) = format_unicode_script(&normalized, kind) {
        return unicode;
    }
    let prefix = match kind {
        ScriptKind::Sub => "_",
        ScriptKind::Sup => "^",
    };
    let single = normalized.chars().count() == 1;
    let all_ascii_letters = kind == ScriptKind::Sub
        && !normalized.is_empty()
        && normalized.chars().all(|c| c.is_ascii_alphabetic());
    if single || all_ascii_letters {
        format!("{prefix}{normalized}")
    } else {
        format!("{prefix}({normalized})")
    }
}

/// `formatFraction`：行内分数 `n/d`。
///
/// 分子只允许 `\p{L}`/`\p{N}`/`.` 组成，分母只允许 `\p{N}`/`.` 组成，或**恰好**一个
/// 任意字符；不满足的一侧加圆括号。判定用的是 JS 的 `\p{L}`/`\p{N}`（见
/// [`is_simple_letters_numbers`] / [`is_simple_numbers`]），不是 Rust `regex` 的字符类。
fn format_fraction(numerator: &str, denominator: &str) -> String {
    let numerator = trim_js(numerator);
    let denominator = trim_js(denominator);
    let simple_numerator = is_simple_letters_numbers(numerator);
    let simple_denominator = is_simple_numbers(denominator) || denominator.chars().count() == 1;
    let n = if simple_numerator {
        numerator.to_string()
    } else {
        format!("({numerator})")
    };
    let d = if simple_denominator {
        denominator.to_string()
    } else {
        format!("({denominator})")
    };
    format!("{n}/{d}")
}

/// `formatRoot`：`\sqrt` 的单行形式。
///
/// 值与 `formatFraction` 的分子用同一"简单值"判定：简单时直接接在根号后（`√x`），否则
/// 加括号（`√(x+1)`）。
fn format_root(value: &str, symbol: &str) -> String {
    let value = trim_js(value);
    if is_simple_letters_numbers(value) {
        format!("{symbol}{value}")
    } else {
        format!("{symbol}({value})")
    }
}

/// `NAMED_OPERATOR_LEFT_SPACING_PATTERN` = `/(?<=[\p{L}\p{N}\)\]}LAYOUT_MARKER_END])LAYOUT_MARKER_START/gu`
/// 的等价实现（`\p{L}`/`\p{N}` 见上面的区间表）：
/// 上一个字符属于 `[\p{L}\p{N})\]}LAYOUT_MARKER_END]` 时，把 `NAMED_OPERATOR_START` 换成空格。
///
/// 原模式是**零宽后顾**，匹配只含 START 一个字符且互不重叠，所以单遍扫描逐字等价；
/// 替换结果不含 START，不会产生新的可匹配位置。
fn space_named_operator_left(value: &str) -> String {
    let mut result = String::with_capacity(value.len() + 4);
    let mut previous: Option<char> = None;
    for character in value.chars() {
        if character == NAMED_OPERATOR_START
            && previous.is_some_and(|previous| {
                is_js_letter(previous)
                    || is_js_number(previous)
                    || previous == ')'
                    || previous == ']'
                    || previous == '}'
                    || previous == LAYOUT_MARKER_END
            })
        {
            result.push(' ');
        }
        result.push(character);
        previous = Some(character);
    }
    result
}

/// `NAMED_OPERATOR_RIGHT_SPACING_PATTERN` 的等价实现：下一个字符属于
/// `[\p{L}\p{N}√LAYOUT_MARKER_START]` 时，把 `NAMED_OPERATOR_END` 换成空格。
fn space_named_operator_right(value: &str) -> String {
    let characters: Vec<char> = value.chars().collect();
    let mut result = String::with_capacity(value.len() + 4);
    for (index, character) in characters.iter().enumerate() {
        if *character == NAMED_OPERATOR_END
            && characters.get(index + 1).is_some_and(|next| {
                is_js_letter(*next)
                    || is_js_number(*next)
                    || *next == '√'
                    || *next == LAYOUT_MARKER_START
            })
        {
            result.push(' ');
        } else {
            result.push(*character);
        }
    }
    result
}

/// `normalizeOutput`（所有输出都必须经过）。
///
/// 顺序敏感：先加左空格再删 START、再加右空格再删 END，然后按行压空白并过滤空行。
fn normalize_output(value: &str) -> String {
    let value = space_named_operator_left(value);
    let value = value.replace(NAMED_OPERATOR_START, "");
    let value = space_named_operator_right(&value);
    let value = value.replace(NAMED_OPERATOR_END, "");

    static SPACES: OnceLock<Regex> = OnceLock::new();
    let spaces = SPACES.get_or_init(|| Regex::new(r"[ \t]+").expect("valid regex"));
    let lines: Vec<&str> = value.split('\n').collect();
    let count = lines.len();
    let mut out: Vec<String> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let collapsed = spaces.replace_all(line, " ").into_owned();
        let trimmed = trim_js(&collapsed);
        if !trimmed.is_empty() || (index > 0 && index < count - 1) {
            out.push(trimmed.to_string());
        }
    }
    trim_js(&out.join("\n")).to_string()
}

// ============================================================================
// 布局引擎
// ============================================================================

/// 脚本类型：下标 / 上标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptKind {
    Sub,
    Sup,
}

impl ScriptKind {
    /// `_` 是下标，其余记号（调用方只会传 `^`）按上标处理。
    fn from_marker(marker: char) -> Self {
        if marker == '_' {
            ScriptKind::Sub
        } else {
            ScriptKind::Sup
        }
    }
}

/// `LayoutNode`（四类节点的联合）。
///
/// 节点在解析阶段写入共享数组，输出串里只留下 `U+F0000{下标}U+F0001` 标记；布局阶段
/// [`render_layout`] 才把标记展开成实际的多行文本。节点内的字段文本可能仍含嵌套标记：
/// 只有 `Fraction`/`Script` 会对字段再递归调用 `render_layout`；`Matrix` 与 `Operator`
/// 都不会展开其中的标记——`Operator` 的 `lower`/`upper` 原样拼进结果（当前输出契约要求如此），
/// 其中若含布局标记，标记会原样留在最终输出里。
#[derive(Debug, Clone)]
enum LayoutNode {
    /// `\frac`（display 且允许堆叠时）。
    Fraction {
        /// 分子：已经 `normalize_output` 的文本，可含嵌套布局标记。
        numerator: String,
        /// 分母：同上。
        denominator: String,
    },
    /// 带上下标的算子（`\sum`、`\lim`、`\operatorname*` 等）。
    Operator {
        /// 算子字形本身（如 `∑`）。
        operator: String,
        /// 下标文本（已归一化并去掉空格）。
        lower: Option<String>,
        /// 上标文本（已归一化并去掉空格）。
        upper: Option<String>,
    },
    /// 普通上下标（display 下 Unicode 上下标不可用、或脚本嵌套，且脚本值通过了
    /// `canUseLayout` 判定时）。
    Script {
        /// 下标：已 `normalize_output` 的文本。
        lower: Option<String>,
        /// 上标：已 `normalize_output` 的文本。
        upper: Option<String>,
    },
    /// 矩阵/`cases`：内容已经是最终的多行文本（`render_layout` 不再展开其中的标记）。
    Matrix {
        /// 每一行的文本。
        lines: Vec<String>,
        /// 基线所在行号，与相邻块并排时按它对齐。
        baseline: usize,
    },
}

/// `Layout`：一块二维文本及其在父布局里的对齐信息。
#[derive(Debug, Clone)]
struct Layout {
    /// 各行文本（不含换行符）。各块生成时会按自身 `width` 补齐；但 [`join_layouts`] 拼好
    /// 一行后会去掉行尾空白（`trim_end_js`），顶层 [`render_layout`] 直接复用这些行，因此
    /// `lines` 中每行的实际宽度可能小于 `width`（`width` 取的是最大行宽）。
    lines: Vec<String>,
    /// 显示宽度（按 [`latex_visible_width`] 计），并排时按此宽度占位。
    width: usize,
    /// 基线在 `lines` 中的下标：与相邻块并排时这一行互相对齐。
    baseline: usize,
}

/// `padLayoutLine(line, width, centered = false)`。
///
/// 按**显示宽度**（[`latex_visible_width`]，不是字节数）补齐到 `width` 列。`centered` 时
/// 左边放 `padding / 2`（整数除法，向下取整）、余数放右边——分数与算子块的居中位置依赖
/// 这个取整方向（`render_layout` 的 `Fraction`/`Operator` 分支传入 `centered = true`；
/// `cases` 的居中不经过这里）。`width` 小于实际宽度时不截断，只是不补空格（`saturating_sub`）。
fn pad_layout_line(line: &str, width: usize, centered: bool) -> String {
    let padding = width.saturating_sub(latex_visible_width(line));
    let left = if centered { padding / 2 } else { 0 };
    format!("{}{}{}", " ".repeat(left), line, " ".repeat(padding - left))
}

/// `joinLayouts`：把同一行的若干块按**基线**并排拼接。
///
/// 每个子布局的 `baseline` 是"与相邻块对齐的那一行"的行号。结果行数 = 各子布局基线的
/// **最大值** + 各子布局"基线以下行数"的**最大值** + 1；结果第 `row` 行取自每个子布局的
/// `row - 结果基线 + 子布局基线` 行，行号越界（含负数）时该子布局用等宽空格填充。拼接后
/// 每行去尾部空白（所以只有行尾空格会被丢掉，内部对齐空格保留），整体宽度取各子布局宽度
/// 之和。
///
/// 空输入返回单行、0 宽、基线 0 的布局（对应 TS 的 `lines: [""]`）。
fn join_layouts(layouts: &[Layout]) -> Layout {
    if layouts.is_empty() {
        return Layout {
            lines: vec![String::new()],
            width: 0,
            baseline: 0,
        };
    }
    let baseline = layouts
        .iter()
        .map(|layout| layout.baseline)
        .max()
        .unwrap_or(0);
    let below = layouts
        .iter()
        .map(|layout| layout.lines.len().saturating_sub(layout.baseline + 1))
        .max()
        .unwrap_or(0);
    let mut lines = Vec::with_capacity(baseline + below + 1);
    for row in 0..=baseline + below {
        let mut line = String::new();
        for layout in layouts {
            let source_row = row as i64 - baseline as i64 + layout.baseline as i64;
            if source_row >= 0 && (source_row as usize) < layout.lines.len() {
                let content = &layout.lines[source_row as usize];
                line.push_str(&pad_layout_line(content, layout.width, false));
            } else {
                line.push_str(&" ".repeat(layout.width));
            }
        }
        lines.push(trim_end_js(&line).to_string());
    }
    Layout {
        lines,
        width: layouts.iter().map(|layout| layout.width).sum(),
        baseline,
    }
}

/// `renderLayout(source, nodes)`：把 `source` 里的布局标记展开成多行文本。
///
/// 逐行处理 `source`（按 `\n` 切分）：
///
/// 1. [`find_layout_markers`] 找出该行的全部 `U+F0000{下标}U+F0001` 标记。**越界的下标会被
///    跳过且 `position` 不前移**，于是那段标记文本会留到下一个文本段里原样输出。
/// 2. 标记之间的文本按规则裁剪：前面已有节点时去掉行首空白，否则保留；行尾空白一律去掉
///    （`trim_end_js`）。此外，若**前一个**节点是 `Matrix` 且这段文本以空白开头，保留
///    一个前导空格；若**后一个**节点是 `Matrix` 且这段文本以空白结尾，保留一个尾随空格。
///    整段只剩空白时，仅当上述前导/尾随保留条件成立（相邻节点是 `Matrix`）才输出一个
///    空格，否则整段丢弃。这是 TS 的排版规则，会直接影响输出宽度，避免矩阵与文字粘连。
/// 3. 遇到节点时按类型生成子布局：`Fraction` = 分子行 + `─` 横线 + 分母行（居中，内容宽度
///    取分子/分母最大者、至少 1，再 +2）；`Operator` = 上/算子/下三行（居中，宽度 +1）；
///    `Script` = 上布局、一空白行、下布局；`Matrix` = 现成行，基线由节点给出。
/// 4. 一行内所有块交给 [`join_layouts`] 按基线并排；各行依次累加，**第一行**的基线作为
///    整个布局的基线（后续行不参与基线计算）。
///
/// 递归只发生在 `Fraction` 的分子/分母和 `Script` 的上/下标上；环与过深输入在进入本函数
/// 之前已由 [`layout_expansion_depth`] 拦截，所以这里的递归一定终止。
fn render_layout(source: &str, nodes: &[LayoutNode]) -> Layout {
    let mut rendered_lines: Vec<String> = Vec::new();
    let mut first_baseline = 0usize;
    for source_line in source.split('\n') {
        let mut layouts: Vec<Layout> = Vec::new();
        let mut position = 0usize;
        let mut previous_node: Option<&LayoutNode> = None;
        for index in find_layout_markers(source_line) {
            let (marker_index, marker_length, node_index) = index;
            let Some(node) = nodes.get(node_index) else {
                continue;
            };
            if marker_index > position {
                let sliced = &source_line[position..marker_index];
                let trimmed = if previous_node.is_some() {
                    trim_start_js(sliced)
                } else {
                    sliced
                };
                let trimmed = trim_end_js(trimmed);
                let preserve_leading_space =
                    matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                        && starts_with_whitespace(sliced);
                let preserve_trailing_space =
                    matches!(node, LayoutNode::Matrix { .. }) && ends_with_whitespace(sliced);
                let text = if !trimmed.is_empty() {
                    format!(
                        "{}{}{}",
                        if preserve_leading_space { " " } else { "" },
                        trimmed,
                        if preserve_trailing_space { " " } else { "" }
                    )
                } else if preserve_leading_space || preserve_trailing_space {
                    " ".to_string()
                } else {
                    String::new()
                };
                layouts.push(Layout {
                    width: latex_visible_width(&text),
                    lines: vec![text],
                    baseline: 0,
                });
            }
            match node {
                LayoutNode::Fraction {
                    numerator,
                    denominator,
                } => {
                    let numerator = render_layout(numerator, nodes);
                    let denominator = render_layout(denominator, nodes);
                    let content_width = numerator.width.max(denominator.width).max(1);
                    let width = content_width + 2;
                    let mut lines: Vec<String> = numerator
                        .lines
                        .iter()
                        .map(|line| pad_layout_line(line, width, true))
                        .collect();
                    lines.push(format!(" {} ", "─".repeat(content_width)));
                    lines.extend(
                        denominator
                            .lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, true)),
                    );
                    layouts.push(Layout {
                        lines,
                        width,
                        baseline: numerator.lines.len(),
                    });
                }
                LayoutNode::Operator {
                    operator,
                    lower,
                    upper,
                } => {
                    let content_width = latex_visible_width(operator)
                        .max(lower.as_ref().map_or(0, |value| latex_visible_width(value)))
                        .max(upper.as_ref().map_or(0, |value| latex_visible_width(value)));
                    let mut lines: Vec<String> = Vec::new();
                    if let Some(upper) = upper {
                        lines.push(format!("{} ", pad_layout_line(upper, content_width, true)));
                    }
                    lines.push(format!(
                        "{} ",
                        pad_layout_line(operator, content_width, true)
                    ));
                    if let Some(lower) = lower {
                        lines.push(format!("{} ", pad_layout_line(lower, content_width, true)));
                    }
                    layouts.push(Layout {
                        lines,
                        width: content_width + 1,
                        baseline: if upper.is_none() { 0 } else { 1 },
                    });
                }
                LayoutNode::Script { lower, upper } => {
                    let upper_layout = upper.as_ref().map(|value| render_layout(value, nodes));
                    let lower_layout = lower.as_ref().map(|value| render_layout(value, nodes));
                    let width = upper_layout
                        .as_ref()
                        .map_or(0, |layout| layout.width)
                        .max(lower_layout.as_ref().map_or(0, |layout| layout.width));
                    let mut lines: Vec<String> = Vec::new();
                    if let Some(upper_layout) = &upper_layout {
                        lines.extend(
                            upper_layout
                                .lines
                                .iter()
                                .map(|line| pad_layout_line(line, width, false)),
                        );
                    }
                    lines.push(" ".repeat(width));
                    if let Some(lower_layout) = &lower_layout {
                        lines.extend(
                            lower_layout
                                .lines
                                .iter()
                                .map(|line| pad_layout_line(line, width, false)),
                        );
                    }
                    layouts.push(Layout {
                        lines,
                        width,
                        baseline: upper_layout.as_ref().map_or(0, |layout| layout.lines.len()),
                    });
                }
                LayoutNode::Matrix { lines, baseline } => {
                    let width = lines
                        .iter()
                        .map(|line| latex_visible_width(line))
                        .max()
                        .unwrap_or(0);
                    layouts.push(Layout {
                        lines: lines
                            .iter()
                            .map(|line| pad_layout_line(line, width, false))
                            .collect(),
                        width,
                        baseline: *baseline,
                    });
                }
            }
            position = marker_index + marker_length;
            previous_node = Some(node);
        }
        if position < source_line.len() {
            let sliced = &source_line[position..];
            let trimmed = if previous_node.is_some() {
                trim_start_js(sliced)
            } else {
                sliced
            };
            let text = if matches!(previous_node, Some(LayoutNode::Matrix { .. }))
                && starts_with_whitespace(sliced)
            {
                format!(" {trimmed}")
            } else {
                trimmed.to_string()
            };
            layouts.push(Layout {
                width: latex_visible_width(&text),
                lines: vec![text],
                baseline: 0,
            });
        }
        let line_layout = join_layouts(&layouts);
        if rendered_lines.is_empty() {
            first_baseline = line_layout.baseline;
        }
        rendered_lines.extend(line_layout.lines);
    }
    Layout {
        width: rendered_lines
            .iter()
            .map(|line| latex_visible_width(line))
            .max()
            .unwrap_or(0),
        lines: rendered_lines,
        baseline: first_baseline,
    }
}

/// JS `String.prototype.trimStart()`：按 [`is_js_whitespace`] 的码点集合裁剪，而不是 Rust
/// `str::trim_start` 的 `char::is_whitespace`（两者对 U+0085/U+FEFF 的判断相反）。
fn trim_start_js(value: &str) -> &str {
    value.trim_start_matches(is_js_whitespace)
}

/// 首个字符是否是 JS 空白（`render_layout` 用它在矩阵边界保留一个空格）。
fn starts_with_whitespace(value: &str) -> bool {
    value.chars().next().is_some_and(is_js_whitespace)
}

/// 最后一个字符是否是 JS 空白。
fn ends_with_whitespace(value: &str) -> bool {
    value.chars().next_back().is_some_and(is_js_whitespace)
}

/// `sourceLine.matchAll(LAYOUT_MARKER_PATTERN)`：返回 `(起始字节, 长度, 节点下标)`。
///
/// 标记 = `LAYOUT_MARKER_START` + 十进制下标 + `LAYOUT_MARKER_END`；下标不允许为空或含非
/// ASCII 数字。找不到 END 时停止扫描；START 后内容畸形时只前进一个 START 继续找，所以畸形
/// 标记不会吞掉后面合法的标记。返回的是**字节**偏移（供字符串切片），下标则是十进制数值。
fn find_layout_markers(line: &str) -> Vec<(usize, usize, usize)> {
    let mut result = Vec::new();
    let mut cursor = 0usize;
    while let Some(offset) = line[cursor..].find(LAYOUT_MARKER_START) {
        let start = cursor + offset;
        let after_start = start + LAYOUT_MARKER_START.len_utf8();
        let Some(end_offset) = line[after_start..].find(LAYOUT_MARKER_END) else {
            break;
        };
        let digits = &line[after_start..after_start + end_offset];
        let end = after_start + end_offset + LAYOUT_MARKER_END.len_utf8();
        if !digits.is_empty()
            && digits.bytes().all(|byte| byte.is_ascii_digit())
            && let Ok(index) = digits.parse::<usize>()
        {
            result.push((start, end - start, index));
            cursor = end;
            continue;
        }
        cursor = start + LAYOUT_MARKER_START.len_utf8();
    }
    result
}

/// `renderLayout` 会递归展开的节点字段：Fraction 的分子/分母、Script 的上/下标。
///
/// Operator 与 Matrix 的字段只被当作字符串/行使用（`visibleWidth(node.operator)`、`node.lines`），
/// 不再递归，因此不产生边。
fn layout_node_children(node_count: usize, node: &LayoutNode) -> Vec<usize> {
    let fields: [Option<&str>; 2] = match node {
        LayoutNode::Fraction {
            numerator,
            denominator,
        } => [Some(numerator), Some(denominator)],
        LayoutNode::Script { lower, upper } => [lower.as_deref(), upper.as_deref()],
        LayoutNode::Operator { .. } | LayoutNode::Matrix { .. } => [None, None],
    };
    let mut children = Vec::new();
    for field in fields.into_iter().flatten() {
        for line in field.split('\n') {
            for (_, _, index) in find_layout_markers(line) {
                if index < node_count {
                    children.push(index);
                }
            }
        }
    }
    children
}

/// 布局展开的最大递归深度；`None` 表示节点内容互相引用形成环。
///
/// TS 的 `renderLayout` 没有任何深度/环保护：字面量布局标记（U+F0000/U+F0001）出现在用户
/// 输入里时，`nodes[i]` 的内容可以引用它自己（或与另一个节点互相引用），V8 会在递归到栈
/// 上限时抛**可捕获的** `RangeError`。Rust 的栈溢出是不可捕获的进程 abort，所以这里先把
/// 布局节点图（只含 render_layout 真正递归的字段）跑一遍迭代 DFS：有环就直接返回 `None`
/// （调用方回退到原文），不会进入 `render_layout`。
///
/// 返回的深度包含最外层 `renderLayout(rendered, nodes)` 调用（`1 + 最长链`），用于决定
/// 是否需要大栈线程。
fn layout_expansion_depth(rendered: &str, nodes: &[LayoutNode]) -> Option<usize> {
    let node_count = nodes.len();
    let recursive: Vec<bool> = nodes
        .iter()
        .map(|node| {
            matches!(
                node,
                LayoutNode::Fraction { .. } | LayoutNode::Script { .. }
            )
        })
        .collect();
    let children: Vec<Vec<usize>> = nodes
        .iter()
        .map(|node| layout_node_children(node_count, node))
        .collect();
    let mut roots: Vec<usize> = Vec::new();
    for line in rendered.split('\n') {
        for (_, _, index) in find_layout_markers(line) {
            if index < node_count {
                roots.push(index);
            }
        }
    }
    // 0 = 未访问，1 = 在栈上（灰），2 = 已完成（黑）。
    let mut state = vec![0u8; node_count];
    let mut depths = vec![0usize; node_count];
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for &root in &roots {
        if state[root] == 2 {
            continue;
        }
        if state[root] == 1 {
            return None;
        }
        state[root] = 1;
        stack.push((root, 0));
        while let Some(&(node, next)) = stack.last() {
            if let Some(&child) = children[node].get(next) {
                if let Some(top) = stack.last_mut() {
                    top.1 += 1;
                }
                match state[child] {
                    0 => {
                        state[child] = 1;
                        stack.push((child, 0));
                    }
                    1 => return None,
                    _ => {}
                }
            } else {
                // Operator / Matrix 的标记在同一帧内就地展开，不新增递归帧，因此不计深度。
                let deepest = children[node]
                    .iter()
                    .filter(|&&child| recursive[child])
                    .map(|&child| depths[child])
                    .max()
                    .unwrap_or(0);
                depths[node] = deepest + 1;
                state[node] = 2;
                stack.pop();
            }
        }
    }
    let deepest = roots
        .iter()
        .filter(|&&root| recursive[root])
        .map(|&root| depths[root])
        .max()
        .unwrap_or(0);
    Some(deepest + 1)
}

// ============================================================================
// LatexParser
// ============================================================================

/// `LatexParser`。`layout_nodes` 是**共享引用**：嵌套 parser 会向同一数组 push。
struct LatexParser<'a> {
    /// 源码按**码点**展开。注意 [`Self::take_utf16_unit`] 会把一个增补平面字符就地替换成
    /// 两个占位字符，所以这个数组的长度会随解析增长（每拆一次 +1）。`position` 是 `source`
    /// 的元素（码点）下标；只有被 `take_utf16_unit` 拆开之后，该字符才以两个元素占位，
    /// 它之后的元素位置才与 TS 的 UTF-16 下标对齐（未拆分的增补字符在这里仍只占一个元素）。
    source: Vec<char>,
    /// 本轮渲染产生的布局节点，所有嵌套 parser 共享（下标因此全局唯一）。
    layout_nodes: &'a mut Vec<LayoutNode>,
    /// 是否 display 模式；嵌套 parser 的值还会与 `stack_fractions` 相与
    /// （见 [`Self::render_nested`]）。
    display: bool,
    /// 下一个待读元素（码点）的下标（TS 的 `this.position`；拆分后的对齐关系见 `source` 字段）。
    position: usize,
    /// 置 `false` 表示遇到不支持的语法或 TS 会抛异常的输入；[`Self::render`] 见到它就返回
    /// `None`。置假后通常仍会继续消费输入，以保证调用方循环能前进。
    supported: bool,
    /// 是否允许把分数纵向堆叠。进入 `\frac` 的分子/分母时按 AND 收窄、离开时恢复，
    /// 所以堆叠模式下嵌套的分数不会再堆叠。
    stack_fractions: bool,
    /// 当前脚本嵌套层数（`parse_scripts` 解析参数期间 +1）。大于 0 表示这段 `^`/`_` 位于
    /// 另一段脚本内部；只有 display 且脚本值通过 `canUseLayout` 判定时，这个条件才会让它
    /// 走二维布局（`needs_layout` 的其余前提见 `parse_scripts`）。
    script_depth: usize,
    /// 递归深度（组/环境/嵌套 parser 共享同一计数，见 [`MAX_PARSE_DEPTH`]）。
    depth: usize,
    /// 上一次 `parse_required_argument` 返回的**参数值**是否是 `Object.prototype` 上的
    /// 函数/对象（见 [`OBJECT_PROTOTYPE_MEMBERS`]）。它来自 [`CommandValue::prototype`]：
    /// 裸 `\command` 参数、以及 `text*`/`mbox` 的原样返回会传播；带花括号的参数永远是字符串。
    /// accent 与 `\mathbb` 会读它（TS 在那两个分支对值做 `Array.from(value)`）；其余命令用它
    /// 识别"TS 会在 `.trim()`/`.replace()` 上抛 `TypeError`"的输入。
    argument_prototype: Option<PrototypeValue>,
}

/// `parseOperator` 的 `inlineLowerStyle`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LowerStyle {
    /// 行内下标写成 `[x]`（`\lim`/`\operatorname` 用）。
    Bracket,
    /// 行内下标写成 Unicode 下标或 `_x`（`\sum` 等符号用）。
    Script,
}

impl<'a> LatexParser<'a> {
    /// 新建 parser：`supported`/`stack_fractions` 为真、位置与深度为 0。
    ///
    /// 嵌套 parser 由 [`Self::render_nested`] 创建后会覆盖 `depth`，使总深度限制跨层累计。
    fn new(source: &str, layout_nodes: &'a mut Vec<LayoutNode>, display: bool) -> Self {
        Self {
            source: source.chars().collect(),
            layout_nodes,
            display,
            position: 0,
            supported: true,
            stack_fractions: true,
            script_depth: 0,
            depth: 0,
            argument_prototype: None,
        }
    }

    fn length(&self) -> usize {
        self.source.len()
    }

    fn char_at(&self, index: usize) -> Option<char> {
        self.source.get(index).copied()
    }

    /// 取**一个 UTF-16 码元**（TS 的 `this.source[this.position]`，`position++` 后只剩
    /// 低位的场景）。
    ///
    /// 增补平面字符在 TS 里占两个码元：`parseRequiredArgumentValue` 的裸参数、
    /// `parseCommandBody` 的非字母分支、`readRawGroup` 的 `\X` 跳过都只消费**高位**，
    /// 低位随后作为单独字符继续解析。`Vec<char>` 表示不了孤立代理，这里把该码点就地
    /// 拆成高/低两个**占位字符**（见 [`SURROGATE_HIGH_BASE`]）：各自在 UTF-16 里是一个
    /// 码元（位置/长度语义与 TS 相同）、宽度按 0 计；最终输出时相邻的两半重新合成原字符，
    /// 落单的一半写成 `U+FFFD`（[`decode_output`]）。
    fn take_utf16_unit(&mut self) -> char {
        let character = self.source[self.position];
        if character.len_utf16() == 2 {
            let code = character as u32 - 0x10000;
            let high = 0xD800 + (code >> 10);
            let low = 0xDC00 + (code & 0x3FF);
            let high_placeholder =
                char::from_u32(SURROGATE_HIGH_BASE + (high - 0xD800)).expect("BMP private use");
            let low_placeholder =
                char::from_u32(SURROGATE_LOW_BASE + (low - 0xDC00)).expect("BMP private use");
            self.source.splice(
                self.position..=self.position,
                [high_placeholder, low_placeholder],
            );
            self.position += 1;
            high_placeholder
        } else {
            self.position += 1;
            character
        }
    }

    /// TS 的 `source.slice(start, end)`：下标是**码点**下标，两端都会截断到 `[0, len]`。
    /// 调用方需保证 `start <= end`；否则 Rust 的范围切片会 panic（JS 则返回空串）。
    fn slice(&self, start: usize, end: usize) -> String {
        self.source[start.min(self.length())..end.min(self.length())]
            .iter()
            .collect()
    }

    /// TS 的 `source.slice(start)`：从 `start` 到末尾的副本（两端截断规则同 [`Self::slice`]）。
    fn rest_from(&self, start: usize) -> String {
        self.slice(start, self.length())
    }

    /// TS 的 `source.indexOf(needle, from)`：按码点匹配，返回码点下标。
    ///
    /// 空 `needle` 返回 `from` 截到长度（JS 的 `indexOf("")` 语义）；非空时找不到返回
    /// `None`（JS 的 -1）。不处理重叠匹配，也不需要：调用方只关心第一次出现。
    fn index_of(&self, needle: &str, from: usize) -> Option<usize> {
        let needle: Vec<char> = needle.chars().collect();
        if needle.is_empty() {
            return Some(from.min(self.length()));
        }
        if from >= self.length() || needle.len() > self.length() - from {
            return None;
        }
        (from..=self.length() - needle.len())
            .find(|&index| self.source[index..index + needle.len()] == needle[..])
    }

    /// 解析整个 `source` 并归一化输出；`None` 表示失败。
    ///
    /// 成功条件：没有置 `supported = false`，且 `position` 恰好停在末尾（输入全部被消费）。
    /// 嵌套 parser 也走这里，它返回 `None` 会让外层把整条公式判为不可渲染
    /// （见 [`Self::render_nested`]）。
    fn render(&mut self) -> Option<String> {
        let rendered = self.parse_sequence(None);
        if !self.supported || self.position != self.length() {
            return None;
        }
        Some(normalize_output(&rendered))
    }

    /// 追加一个布局节点，返回它在输出串里的占位标记 `U+F0000{下标}U+F0001`。
    ///
    /// 下标是十进制、不补零；[`find_layout_markers`] 依赖这个格式。节点数组由本次渲染的
    /// 所有嵌套 parser 共享，所以下标在整个输出里唯一。
    fn push_node(&mut self, node: LayoutNode) -> String {
        let index = self.layout_nodes.len();
        self.layout_nodes.push(node);
        format!("{LAYOUT_MARKER_START}{index}{LAYOUT_MARKER_END}")
    }

    /// 序列解析的统一入口：先做深度保护，再交给 [`Self::parse_sequence_body`]。
    ///
    /// 超过 [`MAX_PARSE_DEPTH`] 时**消费掉剩余输入**并置 `supported = false`。必须前进
    /// `position`：调用方（`parse_sequence_body` 的 `while`）只在 position 变化时才可能退出，
    /// 原地返回会死循环（TS 没有这条路径，它靠 JS 栈抛可捕获的 `RangeError`）。
    fn parse_sequence(&mut self, end_character: Option<char>) -> String {
        if self.depth >= MAX_PARSE_DEPTH {
            // 放弃时必须**消费掉剩余输入**：调用方（`parse_sequence_body` 的 `while`）只在
            // position 前进时才退出，原地返回会变成死循环（TS 没有这条路径，它靠 JS 栈抛异常）。
            self.supported = false;
            self.position = self.length();
            return String::new();
        }
        self.depth += 1;
        let result = self.parse_sequence_body(end_character);
        self.depth -= 1;
        result
    }

    /// 解析一段序列，直到遇到 `end_character`（消费它）或输入结束，返回可见文本。
    ///
    /// 分支顺序就是匹配优先级（调整顺序会改变输出，回归测试逐条锁定）：
    ///
    /// 1. 当前字符 == `end_character`：消费并正常返回；
    /// 2. `}`：没有对应的 `{` → `supported = false`，**不消费**直接返回；
    /// 3. `{`：消费后递归 [`Self::parse_sequence`]（以 `}` 结束）；
    /// 4. `\`：交给 [`Self::parse_command`]。若返回的 `text` 是 `U+0000`（负间距命令），
    ///    改为去掉结果末尾空白，并连带撤销紧邻的命名算子右标记（负间距要作用在算子后）；
    ///    否则把 `text` 追加到结果。
    /// 5. `^` / `_`：先去掉结果末尾空白，再交给 [`Self::parse_scripts`]。若结果以命名算子
    ///    右标记结尾，脚本要插在标记**内部**：先摘掉标记、追加脚本、再补回标记。
    /// 6. JS 空白：整段折叠成一个空格（[`Self::parse_whitespace`]）。
    /// 7. `=` / `<` / `>`：输出 `" op "`（两侧各一个空格）。
    /// 8. `&`：对齐符，丢弃；`~`：输出一个空格。
    /// 9. `.` 且结果末尾是 `Matrix` 节点的标记：把点追加进该矩阵最后一行的文本，而不是
    ///    输出到标记外面（矩阵后的句号要贴住内容）。
    /// 10. 其余字符原样输出。
    ///
    /// 失败路径：指定了 `end_character` 却读到输入末尾时置 `supported = false`（缺右括号）。
    fn parse_sequence_body(&mut self, end_character: Option<char>) -> String {
        let mut result = String::new();
        while self.position < self.length() {
            let character = self.source[self.position];
            if Some(character) == end_character {
                self.position += 1;
                return result;
            }

            if character == '}' {
                self.supported = false;
                return result;
            }

            if character == '{' {
                self.position += 1;
                result.push_str(&self.parse_sequence(Some('}')));
                continue;
            }

            if character == '\\' {
                let command = self.parse_command();
                if command.text == "\u{0}" {
                    result = trim_end_js(&result).to_string();
                    if result.ends_with(NAMED_OPERATOR_END) {
                        result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                    }
                } else {
                    result.push_str(&command.text);
                }
                continue;
            }

            if character == '^' || character == '_' {
                self.position += 1;
                result = trim_end_js(&result).to_string();
                let script = self.parse_scripts(character);
                if result.ends_with(NAMED_OPERATOR_END) {
                    result.truncate(result.len() - NAMED_OPERATOR_END.len_utf8());
                    result.push_str(&script);
                    result.push(NAMED_OPERATOR_END);
                } else {
                    result.push_str(&script);
                }
                continue;
            }

            if is_js_whitespace(character) {
                result.push_str(&self.parse_whitespace());
                continue;
            }

            if character == '=' || character == '<' || character == '>' {
                result = format!("{} {} ", trim_end_js(&result), character);
                self.position += 1;
                continue;
            }

            if character == '&' {
                self.position += 1;
                continue;
            }

            if character == '~' {
                self.position += 1;
                result.push(' ');
                continue;
            }

            if character == '.'
                && let Some(node_index) = trailing_layout_marker(&result)
                && let Some(LayoutNode::Matrix { lines, .. }) =
                    self.layout_nodes.get_mut(node_index)
                && let Some(last_line) = lines.last_mut()
            {
                last_line.push(character);
                self.position += 1;
                continue;
            }

            result.push(character);
            self.position += 1;
        }

        if end_character.is_some() {
            self.supported = false;
        }
        result
    }

    /// 解析 `^`/`_`（`initial_marker` 已被主循环消费）。分五个阶段：
    ///
    /// 1. 解析一个必需参数（参数内禁止堆叠分数）。参数若是原型链函数值（TS 会在
    ///    `normalizeScriptValue` 的 `.trim()` 上抛 `TypeError`），直接返回空串——此时
    ///    `supported` 已被 [`Self::has_prototype_argument`] 置假。
    /// 2. 跳过 JS 空白后，若下一个记号是**另一个方向**的 `^`/`_`，再解析第二个参数。
    ///    同方向重复（`x^a^b`）不在这里处理，留给外层主循环。
    /// 3. 分别尝试把两侧值转成 Unicode 上下标（[`format_unicode_script`]）。
    /// 4. 判断能否用二维布局：含 `/`、或不含布局标记且长度 > 1 且不含 `A-Z`/`*`/`∗`
    ///    的脚本值禁止布局（见 [`is_layout_allowed_marker`]；`x_{ab}` 在 display 下仍是
    ///    `x_ab`，这是 TS 的可观察行为）。
    ///    `needs_layout` = display 且允许布局，且（本脚本嵌套在另一段脚本内，或至少一侧
    ///    没有 Unicode 形式）。
    /// 5. 不需要布局：按 `order`（值的出现顺序）拼接，优先用 Unicode 形式，否则
    ///    [`format_script`]。需要布局：两侧先 `normalize_output`，再 `push_node(Script)`
    ///    并返回标记。
    fn parse_scripts(&mut self, initial_marker: char) -> String {
        let mut sub: Option<String> = None;
        let mut sup: Option<String> = None;
        let mut order: Vec<ScriptKind> = Vec::new();

        let kind = ScriptKind::from_marker(initial_marker);
        self.script_depth += 1;
        let value = self.parse_required_argument(false);
        self.script_depth -= 1;
        if self.has_prototype_argument() {
            // TS：`normalizeScriptValue(value)` 的 `value.trim()`（`x^\constructor`）。
            return String::new();
        }
        order.push(kind);
        match kind {
            ScriptKind::Sub => sub = Some(value),
            ScriptKind::Sup => sup = Some(value),
        }

        let mut next_position = self.position;
        while next_position < self.length() && is_js_whitespace(self.source[next_position]) {
            next_position += 1;
        }
        let next_marker = self.char_at(next_position);
        if matches!(next_marker, Some('^') | Some('_')) && next_marker != Some(initial_marker) {
            self.position = next_position + 1;
            let marker = next_marker.unwrap_or('^');
            let kind = ScriptKind::from_marker(marker);
            self.script_depth += 1;
            let value = self.parse_required_argument(false);
            self.script_depth -= 1;
            if self.has_prototype_argument() {
                return String::new();
            }
            order.push(kind);
            match kind {
                ScriptKind::Sub => sub = Some(value),
                ScriptKind::Sup => sup = Some(value),
            }
        }

        let sub_unicode = sub
            .as_ref()
            .and_then(|value| format_unicode_script(value, ScriptKind::Sub));
        let sup_unicode = sup
            .as_ref()
            .and_then(|value| format_unicode_script(value, ScriptKind::Sup));

        let can_use_layout = [sub.as_deref(), sup.as_deref()]
            .into_iter()
            .flatten()
            .all(|value| {
                !(value.contains('/')
                    || (!value.contains(LAYOUT_MARKER_START)
                        && value.chars().count() > 1
                        && !value.chars().any(is_layout_allowed_marker)))
            });
        let needs_layout = self.display
            && can_use_layout
            && (self.script_depth > 0
                || (sub.is_some() && sub_unicode.is_none())
                || (sup.is_some() && sup_unicode.is_none()));
        if !needs_layout {
            return order
                .iter()
                .map(|kind| match kind {
                    ScriptKind::Sub => sub_unicode.clone().unwrap_or_else(|| {
                        format_script(sub.as_deref().unwrap_or(""), ScriptKind::Sub)
                    }),
                    ScriptKind::Sup => sup_unicode.clone().unwrap_or_else(|| {
                        format_script(sup.as_deref().unwrap_or(""), ScriptKind::Sup)
                    }),
                })
                .collect();
        }

        self.push_node(LayoutNode::Script {
            lower: sub.as_deref().map(normalize_output),
            upper: sup.as_deref().map(normalize_output),
        })
    }

    /// 消耗一整段 JS 空白，返回单个空格（TS 的 `parseWhitespace`：任意长度的空白都折叠成
    /// 一个空格，换行也不例外）。
    fn parse_whitespace(&mut self) -> String {
        while self.position < self.length() && is_js_whitespace(self.source[self.position]) {
            self.position += 1;
        }
        " ".to_string()
    }

    /// `\命令` 的深度保护层：超过 [`MAX_PARSE_DEPTH`] 时消费掉剩余输入、置
    /// `supported = false`，返回空 `CommandValue`。
    ///
    /// 与 [`Self::parse_sequence`] 同理，必须前进 `position`，否则主循环会原地重试。
    fn parse_command(&mut self) -> CommandValue {
        if self.depth >= MAX_PARSE_DEPTH {
            // 同 [`Self::parse_sequence`]：必须前进 position，否则调用方会原地重试。
            self.supported = false;
            self.position = self.length();
            return CommandValue::string(String::new());
        }
        self.depth += 1;
        let command = self.parse_command_body();
        self.depth -= 1;
        command
    }

    /// 解析 `\` 之后的命令，返回输出文本以及"是否是原型链函数值"的标记。
    ///
    /// 分派顺序本身就是行为，顺序影响结果——同一命令名同时出现在
    /// 多个集合/分支时，先命中的那个决定输出：例如 [`LIMIT_OPERATORS`]（`\lim`/`\max`…）
    /// 排在 [`NAMED_OPERATORS`] 之前，[`DISPLAY_LIMIT_SYMBOLS`] 的算子布局判断排在
    /// [`SYMBOLS`] 的通用输出之前。不要按"更合理"的顺序重排：
    ///
    /// 1. `\` 后直接换行：跨行命令 → 一个空格（`\r\n` 只算一次）。
    /// 2. 命令名：ASCII 字母一直读到非字母（`\alpha`）；否则只取**一个 UTF-16 码元**
    ///    （增补平面字符由 [`Self::take_utf16_unit`] 拆成两个占位字符）。
    /// 3. 固定集合：`\\` → 换行符 `\n`；空白命令 → `" "`；负间距 → `U+0000`（由主循环
    ///    特殊处理）；旧式字体切换 → 吞掉后续空白后输出空串；`IGNORED_COMMANDS` → 空串；
    ///    `\{ \} \$ \% \# \_ \&` → 相应字符；`\|` → `‖`。
    /// 4. `\not`：解析一个参数、查 [`NEGATED_SYMBOLS`]；命中则输出 `" 符号 "`；未命中则把
    ///    组合反斜线 `U+0338` 插在参数第一个字符之后（参数为空 → `supported = false`）。
    /// 5. [`LIMIT_OPERATORS`]（`\lim`/`\max`…）→ [`Self::parse_operator`]（行内下标用
    ///    方括号，输出两侧加空格）。
    /// 6. [`SYMBOLS`]：`\cdot`/`\times`/关系符两侧加空格；[`DISPLAY_LIMIT_SYMBOLS`] 走
    ///    算子布局；其余原样输出，并把"命中了原型链"记入 [`CommandValue::prototype`]
    ///    （加了空格的关系符分支不传播该标记）。
    /// 7. [`NAMED_OPERATORS`] → `U+F0004名字U+F0005`，两侧空格由 [`normalize_output`] 按
    ///    相邻字符补。
    /// 8. `\big` 系列（[`SIZE_COMMANDS`]）→ 空串；`\left`/`\middle`/`\right` 吞掉紧跟的
    ///    `.` 后 → 空串。
    /// 9. 结构命令：`\frac`/`\dfrac`/`\tfrac`、`\sqrt`、`\boxed`/`\fbox`、`\binom` 系、
    ///    [`ACCENTS`]、`\mathbb`、`\operatorname`、`\mod`/`\bmod`、`\pmod`/`\pod`、
    ///    `\overset`/`\stackrel`、`\underset`、[`PLAIN_WRAPPERS`]（其中 `text*`/`mbox`
    ///    原样透传函数值）、`\begin`；`\end` 视为错误。
    /// 10. 其余未知命令：置 `supported = false`，但仍返回 `\命令名` 文本（最终结果会被
    ///     丢弃，返回文本只是为了让上层继续消费输入）。
    ///
    /// 失败约定：TS 会调用 `.trim()`/`.replace()` 的位置（`\frac`、`\sqrt`、
    /// `\operatorname`、`\pmod` 等），若参数是原型链函数值，就调用
    /// [`Self::has_prototype_argument`] 置假并返回空串，使 [`render_latex`] 最终返回
    /// `None`（调用方拿不到渲染结果，回退到原文）。
    fn parse_command_body(&mut self) -> CommandValue {
        self.position += 1;
        if self.position >= self.length() {
            self.supported = false;
            return CommandValue::string(String::new());
        }

        let first = self.source[self.position];
        if first == '\n' || first == '\r' {
            self.position += 1;
            if first == '\r' && self.char_at(self.position) == Some('\n') {
                self.position += 1;
            }
            return CommandValue::string(" ");
        }

        let command: String = if first.is_ascii_alphabetic() {
            let start = self.position;
            while self.position < self.length() && self.source[self.position].is_ascii_alphabetic()
            {
                self.position += 1;
            }
            self.slice(start, self.position)
        } else {
            // TS 的 `command = first; this.position++`：只吃掉一个 UTF-16 码元。
            self.take_utf16_unit().to_string()
        };

        if command == "\\" {
            return CommandValue::string("\n");
        }
        if in_set(SPACING_COMMANDS, &command) {
            return CommandValue::string(" ");
        }
        if in_set(NEGATIVE_SPACING_COMMANDS, &command) {
            return CommandValue::string(NEGATIVE_SPACE.to_string());
        }
        if in_set(FONT_SWITCH_COMMANDS, &command) {
            while self.position < self.length() && is_js_whitespace(self.source[self.position]) {
                self.position += 1;
            }
            return CommandValue::string(String::new());
        }
        if in_set(IGNORED_COMMANDS, &command) {
            return CommandValue::string(String::new());
        }
        if matches!(command.as_str(), "{" | "}" | "$" | "%" | "#" | "_" | "&") {
            return CommandValue::string(command);
        }
        if command == "|" {
            return CommandValue::string("‖");
        }
        if command == "not" {
            let argument = self.parse_required_argument(false);
            // TS 在 `parseRequiredArgument(...).trim()` 上对函数值抛 TypeError（`\not\constructor`）。
            if self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            let value = trim_js(&argument).to_string();
            if let Some(negated) = negated_symbol(&value) {
                return CommandValue::string(format!(" {} ", negated.text));
            }
            // `NEGATED_SYMBOLS[value]` 命中原型链（`\not{constructor}` 等）：TS 返回的是函数/
            // 对象，`\`${negated}\`` 会把它转成 `String(fn)` 文本，与 `{}` 分支不同——
            // 命中原型链时 `.trim()` 和 `Array.from` 用的是**函数**而不是文本，但这里
            // `value` 是字符串 `"constructor"`，所以模板字符串直接给出 `String(fn)`。
            // 已在上面 `negated_symbol` 命中，因此这条分支不会被执行到。
            let characters: Vec<char> = value.chars().collect();
            if characters.is_empty() {
                self.supported = false;
                return CommandValue::string(String::new());
            }
            let first = characters[0];
            let rest: String = characters[1..].iter().collect();
            return CommandValue::string(format!(" {first}\u{338}{rest} "));
        }
        if in_set(LIMIT_OPERATORS, &command) {
            return CommandValue::string(self.parse_operator(
                &command,
                LowerStyle::Bracket,
                true,
                true,
            ));
        }

        if let Some(symbol) = symbol(&command) {
            // 命中原型链时 TS 拿到的是函数/对象，后续 `+` 会把它转成字符串；这里用 `text`
            // 复刻该文本，并把"这是函数值"记在 [`CommandValue::prototype`] 里，供
            // `parseRequiredArgumentValue` 原样向上传播（`\hat\constructor`）。
            let text = symbol.text;
            if in_set(DISPLAY_LIMIT_SYMBOLS, &command) {
                return CommandValue::string(self.parse_operator(
                    text,
                    LowerStyle::Script,
                    true,
                    false,
                ));
            }
            return if command == "cdot" || command == "times" || in_set(RELATION_COMMANDS, &command)
            {
                CommandValue::string(format!(" {text} "))
            } else {
                CommandValue::with_prototype(text, symbol.prototype)
            };
        }
        if in_set(NAMED_OPERATORS, &command) {
            return CommandValue::string(format!(
                "{NAMED_OPERATOR_START}{command}{NAMED_OPERATOR_END}"
            ));
        }
        if in_set(SIZE_COMMANDS, &command) {
            return CommandValue::string(String::new());
        }
        if command == "left" || command == "middle" || command == "right" {
            if self.char_at(self.position) == Some('.') {
                self.position += 1;
            }
            return CommandValue::string(String::new());
        }
        if command == "frac" || command == "dfrac" || command == "tfrac" {
            let should_stack = self.display && self.stack_fractions && command != "tfrac";
            let numerator = self.parse_required_argument(!should_stack);
            let numerator_threw = self.has_prototype_argument();
            let denominator = self.parse_required_argument(!should_stack);
            let denominator_threw = self.has_prototype_argument();
            if numerator_threw || denominator_threw {
                // TS：`numerator.trim()` / `denominator.trim()`（或堆叠时的 `normalizeOutput`）。
                return CommandValue::string(String::new());
            }
            if should_stack {
                return CommandValue::string(self.push_node(LayoutNode::Fraction {
                    numerator: normalize_output(&numerator),
                    denominator: normalize_output(&denominator),
                }));
            }
            return CommandValue::string(format_fraction(&numerator, &denominator));
        }
        if command == "sqrt" {
            let degree = self
                .parse_optional_argument()
                .map(|value| trim_js(&value).to_string());
            let value = self.parse_required_argument(true);
            // TS：`formatRoot(value)` 里的 `value.trim()`。
            if self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            return CommandValue::string(match degree.as_deref() {
                None | Some("2") => format_root(&value, "√"),
                Some("3") => format_root(&value, "∛"),
                Some("4") => format_root(&value, "∜"),
                Some(degree) => format!(
                    "{}{}",
                    format_script(degree, ScriptKind::Sup),
                    format_root(&value, "√")
                ),
            });
        }
        if command == "boxed" || command == "fbox" {
            let value = self.parse_required_argument(true);
            if self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            return CommandValue::string(format!("[{}]", trim_js(&value)));
        }
        if command == "binom" || command == "dbinom" || command == "tbinom" {
            // 模板字符串会把函数值强制转成文本，所以这里**不**检查原型标记。
            let first = self.parse_required_argument(true);
            let second = self.parse_required_argument(true);
            return CommandValue::string(format!("({first} choose {second})"));
        }
        if let Some(accent) = accent(&command) {
            let value = self.parse_required_argument(true);
            // TS 用 `Array.from(value).length === 1`：字符串按码点计，原型链函数按其 arity，
            // `__proto__`（对象）为 0。`value` 本身已是 `String(value)` 的文本。
            let length = match self.argument_prototype {
                Some(PrototypeValue::Function(arity)) => arity,
                Some(PrototypeValue::Object) => 0,
                None => value.chars().count(),
            };
            return CommandValue::string(if length == 1 {
                format!("{value}{}", accent.text)
            } else {
                format!("{command}({value})")
            });
        }
        if command == "mathbb" {
            let value = self.parse_required_argument(true);
            // TS：`Array.from(value, ch => BLACKBOARD[ch] ?? ch).join("")`。函数值没有可迭代
            // 元素（`Array.from` 走类数组分支，按 `fn.length` 生成那么多个 `undefined`），
            // `BLACKBOARD[undefined]` 查不到 → 回调返回 `undefined`，而 `Array.prototype.join`
            // 把 `null`/`undefined` 转成空串，所以结果**永远是空串**（arity 为 0 时也一样）。
            if self.argument_prototype.is_some() {
                return CommandValue::string(String::new());
            }
            let mut result = String::new();
            for character in value.chars() {
                match blackboard(character) {
                    Some(mapped) => result.push_str(mapped),
                    None => result.push(character),
                }
            }
            return CommandValue::string(result);
        }
        if command == "operatorname" {
            let starred = self.char_at(self.position) == Some('*');
            if starred {
                self.position += 1;
            }
            let argument = self.parse_required_argument(true);
            if self.has_prototype_argument() {
                // TS：`normalizeOutput(value)` 里的 `value.replace(...)`。
                return CommandValue::string(String::new());
            }
            let operator = trim_js(&normalize_output(&argument)).to_string();
            return CommandValue::string(self.parse_operator(
                &operator,
                LowerStyle::Bracket,
                starred,
                true,
            ));
        }
        if command == "mod" || command == "bmod" {
            return CommandValue::string(" mod ");
        }
        if command == "pmod" || command == "pod" {
            let argument = self.parse_required_argument(true);
            if self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            let value = trim_js(&argument).to_string();
            return CommandValue::string(if command == "pmod" {
                format!(" (mod {value})")
            } else {
                format!(" ({value})")
            });
        }
        if command == "overset" || command == "stackrel" {
            let upper = self.parse_required_argument(true);
            let upper_threw = self.has_prototype_argument();
            let second = self.parse_required_argument(true);
            if upper_threw || self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            let value = trim_js(&second).to_string();
            return CommandValue::string(format!(
                "{value}{}",
                format_script(&upper, ScriptKind::Sup)
            ));
        }
        if command == "underset" {
            let lower = self.parse_required_argument(true);
            let lower_threw = self.has_prototype_argument();
            let second = self.parse_required_argument(true);
            if lower_threw || self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            let value = trim_js(&second).to_string();
            return CommandValue::string(format!(
                "{value}{}",
                format_script(&lower, ScriptKind::Sub)
            ));
        }
        if in_set(PLAIN_WRAPPERS, &command) {
            let value = self.parse_required_argument(true);
            if command.starts_with("text") || command == "mbox" {
                // 这两个分支不调用 `.trim()`，函数值被原样返回：必须继续向上传播，
                // 否则 `\hat\text\constructor` 会按字符串长度走 `hat(...)` 而不是组合符。
                let prototype = self.argument_prototype;
                return CommandValue::with_prototype(value, prototype);
            }
            if self.has_prototype_argument() {
                return CommandValue::string(String::new());
            }
            return CommandValue::string(trim_js(&value));
        }
        if command == "begin" {
            return CommandValue::string(self.parse_environment());
        }
        if command == "end" {
            self.supported = false;
            return CommandValue::string(String::new());
        }

        self.supported = false;
        CommandValue::string(format!("\\{command}"))
    }
    /// `parseOperator(name, inlineLowerStyle, displayLimits, spaced)`：算子字形 + 上下标。
    ///
    /// 1. 先跳过空格/制表符；若紧跟 `\limits` / `\nolimits` 且后面不是 ASCII 字母，
    ///    用它覆盖 `displayLimits` 并消费掉（[`match_limit_modifier`]）。
    /// 2. 循环解析紧邻的 `_`/`^` 参数（记号前允许空格/制表符）。参数先 `normalize_output`
    ///    再删掉 ASCII 空格；同一方向第二次出现时置 `supported = false`。
    /// 3. display 模式、`displayLimits` 为真且有上下标 → 生成 `LayoutNode::Operator`
    ///    （上下标堆在字形上/下方）。
    /// 4. 否则输出单行：下标按 `inline_lower_style`（`Bracket` → `[x]`，`Script` →
    ///    Unicode 下标或 `_x`）、上标走 [`format_script`]；`spaced` 为真时整体两侧各加
    ///    一个空格。
    fn parse_operator(
        &mut self,
        operator: &str,
        inline_lower_style: LowerStyle,
        display_limits: bool,
        spaced: bool,
    ) -> String {
        let mut use_display_limits = display_limits;
        let mut modifier_position = self.position;
        while modifier_position < self.length()
            && matches!(self.source[modifier_position], ' ' | '\t')
        {
            modifier_position += 1;
        }
        if let Some((is_limits, length)) = match_limit_modifier(&self.rest_from(modifier_position))
        {
            use_display_limits = is_limits;
            self.position = modifier_position + length;
        }

        let mut lower: Option<String> = None;
        let mut upper: Option<String> = None;
        loop {
            let mut script_position = self.position;
            while script_position < self.length()
                && matches!(self.source[script_position], ' ' | '\t')
            {
                script_position += 1;
            }
            let kind = self.char_at(script_position);
            if kind != Some('_') && kind != Some('^') {
                break;
            }
            self.position = script_position + 1;
            let argument = self.parse_required_argument(false);
            if self.has_prototype_argument() {
                // TS：`normalizeOutput(value)` 的 `value.replace(...)`（`\sum_\constructor`）。
                return String::new();
            }
            let value = normalize_output(&argument).replace(' ', "");
            if kind == Some('_') {
                if lower.is_some() {
                    self.supported = false;
                }
                lower = Some(value);
            } else {
                if upper.is_some() {
                    self.supported = false;
                }
                upper = Some(value);
            }
        }

        if self.display && use_display_limits && (lower.is_some() || upper.is_some()) {
            return self.push_node(LayoutNode::Operator {
                operator: operator.to_string(),
                lower,
                upper,
            });
        }

        let mut rendered = operator.to_string();
        if let Some(lower) = &lower {
            rendered.push_str(&match inline_lower_style {
                LowerStyle::Bracket => format!("[{lower}]"),
                LowerStyle::Script => format_script(lower, ScriptKind::Sub),
            });
        }
        if let Some(upper) = &upper {
            rendered.push_str(&format_script(upper, ScriptKind::Sup));
        }
        if spaced {
            format!(" {rendered} ")
        } else {
            rendered
        }
    }

    /// 解析一个必需参数，并临时收窄 `stack_fractions`。
    ///
    /// 收窄规则是 **AND**：只有当前层与本次参数都允许时才允许，所以 `\frac` 的分子/分母里
    /// 再出现 `\frac` 不会继续堆叠。进入前清空 [`Self::argument_prototype`]，让调用方读到
    /// 的是本次参数的标记；解析后无论成败都恢复 `stack_fractions`。
    fn parse_required_argument(&mut self, stack_fractions: bool) -> String {
        let previous_stack_fractions = self.stack_fractions;
        self.stack_fractions = previous_stack_fractions && stack_fractions;
        // `parseRequiredArgumentValue` 只在"参数就是一个 `\command`"时才可能返回函数值
        // （`\hat\constructor`）；带花括号/普通字符的参数永远是字符串。标记由
        // [`Self::parse_command_body`] 在该情形下写入，这里先清空再读取。
        self.argument_prototype = None;
        let value = self.parse_required_argument_value();
        self.stack_fractions = previous_stack_fractions;
        value
    }

    /// `parseRequiredArgumentValue`：参数值的三种形态。
    ///
    /// - `{...}`：消费 `{`，用 [`Self::parse_sequence`] 解析到 `}`。结果经 `+=` 拼接后
    ///   **永远是字符串**，所以这里清掉内层留下的原型标记（`\frac{\text\constructor}{2}`
    ///   的分子是文本而不是函数值）。
    /// - `\command`：整个参数就是一个命令，返回值原样透传，是否函数值记入
    ///   [`Self::argument_prototype`]（`\hat\constructor` 依赖它）。
    /// - 其他：只取**一个 UTF-16 码元**（TS 的 `source[position]`）；增补平面字符由
    ///   [`Self::take_utf16_unit`] 拆成高/低占位符。
    ///
    /// 已无输入可读时置 `supported = false` 并返回空串。
    fn parse_required_argument_value(&mut self) -> String {
        while self.position < self.length() && is_js_whitespace(self.source[self.position]) {
            self.position += 1;
        }
        if self.position >= self.length() {
            self.supported = false;
            return String::new();
        }
        if self.source[self.position] == '{' {
            self.position += 1;
            let value = self.parse_sequence(Some('}'));
            // TS：带花括号的参数经 `parseSequence` 的 `result += command` 后**永远是字符串**
            // （`\frac{\text\constructor}{2}` 的分子是 33 个字符的文本，不是函数值）。
            // 内层命令调用 `parseRequiredArgument` 时留下的标记必须在这里清掉，否则外层
            // 命令会误以为自己拿到的是函数值。
            self.argument_prototype = None;
            return value;
        }
        if self.source[self.position] == '\\' {
            let value = self.parse_command();
            // 整个参数就是一个 `\command`：返回值原样传给调用方；是否函数值由
            // [`CommandValue::prototype`] 决定（`\hat\constructor` 是，`\hat\mathbf{x}` 不是）。
            self.argument_prototype = value.prototype;
            return value.text;
        }
        // TS 的 `value = this.source[this.position]; this.position++`：裸参数只取一个
        // UTF-16 码元（增补平面字符会被劈成两半，见 [`Self::take_utf16_unit`]）。
        self.take_utf16_unit().to_string()
    }

    /// 参数值是否是 `Object.prototype` 上的函数/对象（见 [`Self::argument_prototype`]）。
    ///
    /// TS 在这些位置对值调用 `.trim()` / `.replace()`，函数值会抛 `TypeError`
    /// （如 `\frac\constructor2` → `numerator.trim is not a function`），异常直接冒泡出
    /// `renderLatex`。本文件的公开签名是 `Option<String>`（骨架冻结），没有异常通道，也不允许
    /// 为模型可控输入而 panic，因此把"TS 会抛 TypeError"映射成 `supported = false`
    /// （`renderLatex` 返回 `None`，markdown 层回退到原文）：两种情况下都拿不到渲染结果。
    fn has_prototype_argument(&mut self) -> bool {
        if self.argument_prototype.is_some() {
            self.supported = false;
            true
        } else {
            false
        }
    }

    /// `parseOptionalArgument`：只接受紧跟其后的 `[...]`（前面允许空格/制表符）。
    ///
    /// 方括号内的文本交给嵌套 parser 渲染，因此 `\sqrt[3]` 的 `3` 也要走完整解析流程。
    /// 没有 `[` 是正常情况，返回 `None`；有 `[` 却没有 `]` 是语法错误：置
    /// `supported = false` 并返回 `None`。
    fn parse_optional_argument(&mut self) -> Option<String> {
        while self.position < self.length() && matches!(self.source[self.position], ' ' | '\t') {
            self.position += 1;
        }
        if self.char_at(self.position) != Some('[') {
            return None;
        }
        let end = self.index_of("]", self.position + 1);
        let Some(end) = end else {
            self.supported = false;
            return None;
        };
        let value = self.slice(self.position + 1, end);
        self.position = end + 1;
        Some(self.render_nested(&value, true))
    }

    /// `readRawGroup`：读取 `{...}` 的**原始**内容（不做命令解析），支持嵌套花括号。
    ///
    /// 环境名靠它按原样取出（唯一调用点是 [`Self::parse_environment`]；`array` 的列格式
    /// 参数由 [`strip_leading_group`] 单独处理，不经过这里；TS 的 `readRawGroup` 同样只用于
    /// 环境名）。读取前先跳过空格/制表符（不是全部 JS 空白）；遇到 `\` 时无条件跳过两个
    /// 字符（TS 的 `position += 2`），因此 `\{`/`\}` 不参与花括号计数，但两者仍原样留在
    /// 返回的组内容里。跳过可能越过输入末尾，此时按"未闭合"处理。
    ///
    /// 失败：当前位置不是 `{`，或读不到配对的 `}` → 置 `supported = false` 并返回 `None`。
    fn read_raw_group(&mut self) -> Option<String> {
        while self.position < self.length() && matches!(self.source[self.position], ' ' | '\t') {
            self.position += 1;
        }
        if self.char_at(self.position) != Some('{') {
            self.supported = false;
            return None;
        }

        self.position += 1;
        let start = self.position;
        let mut depth = 1i32;
        while self.position < self.length() {
            let character = self.source[self.position];
            if character == '\\' {
                // TS 的 `position += 2` 只跳过 `\` 与转义字符；两者仍留在
                // `slice(start, position)` 返回的组内容里（增补平面字符的两半相邻，
                // 打印时仍是原字符），因此这里不需要拆分。
                self.position += 2;
                continue;
            }
            if character == '{' {
                depth += 1;
            }
            if character == '}' {
                depth -= 1;
            }
            if depth == 0 {
                let value = self.slice(start, self.position);
                self.position += 1;
                return Some(value);
            }
            self.position += 1;
        }
        self.supported = false;
        None
    }

    /// 把环境体按 `\\`（换行命令）切成若干行，返回至少一个元素（末尾空行也算一行）。
    ///
    /// `\\` 后面若紧跟 `[长度]`（LaTeX 的行距参数），在 `]` 之前的同段文本一并丢弃；
    /// 若 `[` 后先遇到换行或输入结束（没有 `]`），则只丢弃 `\\` 本身，`[` 及其后内容保留。
    ///
    /// 注意：这是纯字符扫描，**不跟踪花括号嵌套**，所以单元格内容里出现的 `\\` 同样会被
    /// 当成分行符。
    fn split_environment_rows(body: &str) -> Vec<String> {
        let chars: Vec<char> = body.chars().collect();
        let mut rows: Vec<String> = Vec::new();
        let mut current = String::new();
        let mut index = 0usize;
        while index < chars.len() {
            if chars[index] == '\\' && chars.get(index + 1) == Some(&'\\') {
                let mut end = index + 2;
                if chars.get(end) == Some(&'[') {
                    let mut scan = end + 1;
                    while scan < chars.len() && chars[scan] != ']' && chars[scan] != '\n' {
                        scan += 1;
                    }
                    if scan < chars.len() && chars[scan] == ']' {
                        end = scan + 1;
                    }
                }
                rows.push(std::mem::take(&mut current));
                index = end;
                continue;
            }
            current.push(chars[index]);
            index += 1;
        }
        rows.push(current);
        rows
    }

    /// `parseEnvironment`（`\begin{...}`）：取环境名、找到配对的 `\end{环境名}`，按环境分派。
    ///
    /// - **空环境名**：TS 的 `if (!environment) return "";` 中空串是 falsy，所以只返回空串并
    ///   **继续正常解析**，不置 `supported = false`（回归测试
    ///   `empty_environment_name_is_falsy_and_parsing_continues` 锁定）。但紧随其后的
    ///   `\end{...}` 仍会被当作孤立的 `\end` 命令报错。
    /// - `equation` / `equation*` / `displaymath`：渲染体内文本并去掉两端空白，嵌套 parser
    ///   的 display 仍按本层设置。
    /// - `align`/`aligned`/`gather`/`multline`/`split` 等：按 `\\` 分行、按 `&` 分列；
    ///   `alignat` 系先去掉 `{列数}` 参数，再把相邻两列拼成一组（组间一个空格）。空行被
    ///   过滤，行与行用 `\n` 连接。
    /// - `cases`/`cases*` → [`Self::render_cases`]。
    /// - `array`/`matrix`/`pmatrix` 等 → [`Self::render_matrix`]（`array` 先去掉列格式）。
    /// - 其他环境：置 `supported = false`，但仍返回环境体原文（外层最终得到 `None`）。
    ///
    /// 找结束标记用的是 [`Self::index_of`]（第一次出现），**不检查嵌套**；找不到时置假并
    /// 返回空串。
    fn parse_environment(&mut self) -> String {
        let Some(environment) = self.read_raw_group() else {
            return String::new();
        };
        // TS：`if (!environment) return "";` —— 空串是 falsy，所以 `\begin{}` 静默返回空串并
        // 继续正常解析（**不**置 `supported = false`）。只有 `readRawGroup` 失败（None）才是错误。
        if environment.is_empty() {
            return String::new();
        }
        let end_marker = format!("\\end{{{environment}}}");
        let Some(end) = self.index_of(&end_marker, self.position) else {
            self.supported = false;
            return String::new();
        };
        let body = self.slice(self.position, end);
        self.position = end + end_marker.chars().count();

        if environment == "equation" || environment == "equation*" || environment == "displaymath" {
            return trim_js(&self.render_nested(&body, true)).to_string();
        }

        if matches!(
            environment.as_str(),
            "aligned"
                | "align"
                | "align*"
                | "alignedat"
                | "alignat"
                | "alignat*"
                | "gather"
                | "gathered"
                | "multline"
                | "multline*"
                | "split"
        ) {
            let aligned_at = matches!(environment.as_str(), "alignedat" | "alignat" | "alignat*");
            let aligned_body = if aligned_at {
                strip_leading_group(&body)
            } else {
                body.clone()
            };
            let rows: Vec<String> = Self::split_environment_rows(&aligned_body);
            let rendered: Vec<String> = rows
                .iter()
                .map(|row| {
                    let cells: Vec<&str> = row.split('&').collect();
                    let source = if aligned_at {
                        let group_count = cells.len().div_ceil(2);
                        (0..group_count)
                            .map(|index| {
                                cells[index * 2..(index * 2 + 2).min(cells.len())].concat()
                            })
                            .collect::<Vec<String>>()
                            .join(" ")
                    } else {
                        cells.concat()
                    };
                    trim_js(&self.render_nested(&source, true)).to_string()
                })
                .filter(|row| !row.is_empty())
                .collect();
            return rendered.join("\n");
        }

        if environment == "cases" || environment == "cases*" {
            return self.render_cases(&body);
        }

        if matches!(
            environment.as_str(),
            "array"
                | "matrix"
                | "smallmatrix"
                | "pmatrix"
                | "bmatrix"
                | "Bmatrix"
                | "vmatrix"
                | "Vmatrix"
        ) {
            let matrix_body = if environment == "array" {
                strip_leading_group(&body)
            } else {
                body.clone()
            };
            return self.render_matrix(&environment, &matrix_body);
        }

        self.supported = false;
        body
    }

    /// `renderCases`：`cases` 环境的排版。
    ///
    /// 每行按 `&` 分列：第一列是值（去掉末尾逗号，见 [`trim_end_comma`]），第二列是条件。
    /// 单元格经 `display = false` 的子 parser 渲染（分数/算子/脚本不会堆叠；嵌套矩阵仍会
    /// 按自身规则生成节点）。
    /// 条件为空时只输出值；否则用 `PROTECTED_SPACE` 把值补齐到所有值的最宽显示宽度，再拼接
    /// 条件——条件以 `if`/`when`/`for`/`otherwise` 开头（不区分大小写，且关键字之后是输入
    /// 结尾或不是 ASCII 字母数字/`_`）时只加一个空格，否则加 `" if "`。
    ///
    /// 行数 > 1 时生成 `LayoutNode::Matrix`：左花括号按首行 `⎧`、中间 `⎨`、末行 `⎩`；
    /// 行数为**偶数**时在中间额外插入一行只含 `⎨` 的空行（TS 的居中做法），基线取
    /// `行数 / 2`。只有一行时直接返回 `"⎧ {content}"`，不产生布局节点。
    fn render_cases(&mut self, body: &str) -> String {
        let rows: Vec<Vec<String>> = Self::split_environment_rows(body)
            .iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| trim_js(&self.render_nested(cell, false)).to_string())
                    .collect::<Vec<String>>()
            })
            .filter(|row| row.iter().any(|cell| !cell.is_empty()))
            .collect();
        let value_width = rows
            .iter()
            .map(|row| latex_visible_width(trim_end_comma(row.first().map_or("", String::as_str))))
            .max()
            .unwrap_or(0);
        let contents: Vec<String> = rows
            .iter()
            .map(|row| {
                let value = trim_end_comma(row.first().map_or("", String::as_str));
                let condition = row.get(1).map_or("", String::as_str);
                if condition.is_empty() {
                    return value.to_string();
                }
                let condition_prefix = if starts_with_case_keyword(condition) {
                    " "
                } else {
                    " if "
                };
                format!(
                    "{}{}{}{}",
                    value,
                    PROTECTED_SPACE
                        .to_string()
                        .repeat(value_width.saturating_sub(latex_visible_width(value))),
                    condition_prefix,
                    condition
                )
            })
            .collect();
        if contents.len() <= 1 {
            return match contents.first() {
                None => String::new(),
                Some(content) => format!("⎧ {content}"),
            };
        }

        let middle = contents.len() / 2;
        let mut visual_rows: Vec<Option<String>> = contents.iter().cloned().map(Some).collect();
        if contents.len().is_multiple_of(2) {
            visual_rows.insert(middle, None);
        }
        let lines: Vec<String> = visual_rows
            .iter()
            .enumerate()
            .map(|(index, content)| {
                let delimiter = if index == 0 {
                    "⎧"
                } else if index == visual_rows.len() - 1 {
                    "⎩"
                } else {
                    "⎨"
                };
                match content {
                    None => delimiter.to_string(),
                    Some(content) => format!("{delimiter} {content}"),
                }
            })
            .collect();
        self.push_node(LayoutNode::Matrix {
            lines,
            baseline: middle,
        })
    }

    /// `renderMatrix`：`matrix`/`pmatrix`/`array` 等环境的排版。
    ///
    /// 每行按 `&` 分列，单元格各自嵌套渲染并去两端空白；整行全空的会被过滤。列宽取该列
    /// 最宽的显示宽度，单元格右侧用 `PROTECTED_SPACE` 补齐，列与列之间用 `" │ "` 连接。
    /// `array`/`matrix`/`smallmatrix` 不加括号；`pmatrix`/`bmatrix`/`Bmatrix`/`vmatrix`/
    /// `Vmatrix` 按首行/中间行/末行分别选左、右定界符。
    ///
    /// 行数 ≤ 1 时直接返回该行文本（不产生节点）；否则生成 `LayoutNode::Matrix`（基线 0）。
    /// 环境名不在上述集合时置 `supported = false` 并返回不带括号的行文本——现有调用路径
    /// 已在 [`Self::parse_environment`] 过滤过环境名，这条分支实际不可达。
    fn render_matrix(&mut self, environment: &str, body: &str) -> String {
        let matrix: Vec<Vec<String>> = Self::split_environment_rows(body)
            .iter()
            .map(|row| {
                row.split('&')
                    .map(|cell| trim_js(&self.render_nested(cell, false)).to_string())
                    .collect::<Vec<String>>()
            })
            .filter(|row| row.iter().any(|cell| !cell.is_empty()))
            .collect();
        let column_count = matrix.iter().map(|row| row.len()).max().unwrap_or(0);
        let column_widths: Vec<usize> = (0..column_count)
            .map(|column| {
                matrix
                    .iter()
                    .map(|row| latex_visible_width(row.get(column).map_or("", String::as_str)))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let rows: Vec<String> = matrix
            .iter()
            .map(|row| {
                (0..column_count)
                    .map(|column| {
                        let cell = row.get(column).map_or("", String::as_str);
                        format!(
                            "{}{}",
                            cell,
                            PROTECTED_SPACE.to_string().repeat(
                                column_widths[column].saturating_sub(latex_visible_width(cell))
                            )
                        )
                    })
                    .collect::<Vec<String>>()
                    .join(" │ ")
            })
            .collect();

        let lines: Vec<String> =
            if environment == "array" || environment == "matrix" || environment == "smallmatrix" {
                rows.clone()
            } else {
                let delimiter: Option<[&str; 6]> = match environment {
                    "pmatrix" => Some(["⎛", "⎞", "⎜", "⎟", "⎝", "⎠"]),
                    "bmatrix" => Some(["⎡", "⎤", "⎢", "⎥", "⎣", "⎦"]),
                    "Bmatrix" => Some(["⎧", "⎫", "⎨", "⎬", "⎩", "⎭"]),
                    "vmatrix" => Some(["│", "│", "│", "│", "│", "│"]),
                    "Vmatrix" => Some(["║", "║", "║", "║", "║", "║"]),
                    _ => None,
                };
                let Some(delimiter) = delimiter else {
                    self.supported = false;
                    return rows.join("\n");
                };
                rows.iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let left = if index == 0 {
                            delimiter[0]
                        } else if index == rows.len() - 1 {
                            delimiter[4]
                        } else {
                            delimiter[2]
                        };
                        let right = if index == 0 {
                            delimiter[1]
                        } else if index == rows.len() - 1 {
                            delimiter[5]
                        } else {
                            delimiter[3]
                        };
                        format!("{left} {row} {right}")
                    })
                    .collect()
            };

        if lines.len() <= 1 {
            return lines.first().cloned().unwrap_or_default();
        }
        self.push_node(LayoutNode::Matrix { lines, baseline: 0 })
    }

    /// 用**新的** parser 渲染一段子文本（`\sqrt` 的参数、方括号可选参数、环境体/单元格等）。
    ///
    /// 新 parser 共享同一个 `layout_nodes`，所以子文本产生的布局标记对本层有效；`depth`
    /// 从当前层继承（不重置），总深度仍受 [`MAX_PARSE_DEPTH`] 约束。子 parser 的 `display`
    /// = 本层 `display` **且** 允许堆叠，因此单元格里的分数/算子/脚本不会展开成多行
    /// （矩阵节点不受 `display` 控制，仍按自身规则生成）。
    ///
    /// 子解析失败时把本层 `supported` 置假，并返回**原始子文本**——调用方可以继续消费输入，
    /// 但最终 [`Self::render`] 会因为 `supported` 为假而返回 `None`。
    fn render_nested(&mut self, source: &str, stack_fractions: bool) -> String {
        let display = self.display && stack_fractions;
        let mut nested = LatexParser::new(source, &mut *self.layout_nodes, display);
        nested.depth = self.depth;
        let rendered = nested.render();
        match rendered {
            None => {
                self.supported = false;
                source.to_string()
            }
            Some(rendered) => rendered,
        }
    }
}

/// `/[A-Z*∗]/`（`canUseLayout` 的排除条件；未锚定）。
fn is_layout_allowed_marker(character: char) -> bool {
    character.is_ascii_uppercase() || character == '*' || character == '∗'
}

/// `(row[0] ?? "").replace(/,\s*$/, "")`。
fn trim_end_comma(value: &str) -> &str {
    let trimmed = trim_end_js(value);
    if let Some(stripped) = trimmed.strip_suffix(',') {
        stripped
    } else {
        value
    }
}

/// `/^(?:if|when|for|otherwise)\b/i`（JS 无 `u` 标志时 `\b` 用 ASCII 单词字符）。
fn starts_with_case_keyword(condition: &str) -> bool {
    for keyword in ["if", "when", "for", "otherwise"] {
        let Some(head) = condition.get(..keyword.len()) else {
            continue;
        };
        if !head.eq_ignore_ascii_case(keyword) {
            continue;
        }
        let boundary = condition[keyword.len()..]
            .chars()
            .next()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'));
        if boundary {
            return true;
        }
    }
    false
}

/// `body.replace(/^\s*\{[^}]*\}/, "")`（只替换首个匹配）。
fn strip_leading_group(body: &str) -> String {
    let chars: Vec<char> = body.chars().collect();
    let mut index = 0usize;
    while index < chars.len() && is_js_whitespace(chars[index]) {
        index += 1;
    }
    if chars.get(index) != Some(&'{') {
        return body.to_string();
    }
    let mut scan = index + 1;
    while scan < chars.len() && chars[scan] != '}' {
        scan += 1;
    }
    if scan >= chars.len() {
        return body.to_string();
    }
    chars[scan + 1..].iter().collect()
}

/// `/^\\(limits|nolimits)(?![A-Za-z])/`：返回 `(是否 limits, 匹配长度)`。
fn match_limit_modifier(rest: &str) -> Option<(bool, usize)> {
    for (keyword, is_limits) in [("\\limits", true), ("\\nolimits", false)] {
        if let Some(tail) = rest.strip_prefix(keyword) {
            let boundary = tail
                .chars()
                .next()
                .is_none_or(|character| !character.is_ascii_alphabetic());
            if boundary {
                return Some((is_limits, keyword.chars().count()));
            }
        }
    }
    None
}

/// `TRAILING_LAYOUT_MARKER_PATTERN.exec(result)`：返回末尾标记的节点下标。
fn trailing_layout_marker(result: &str) -> Option<usize> {
    let end = result.strip_suffix(LAYOUT_MARKER_END)?;
    let start_index = end.rfind(LAYOUT_MARKER_START)?;
    let digits = &end[start_index + LAYOUT_MARKER_START.len_utf8()..];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse::<usize>().ok()
}

/// 递归安全阈值：`{` 与 `\` 的总数不超过它时，当前栈一定能容纳解析深度。
///
/// 每进入一层递归都必然消费掉一个 `{` 或一个 `\`（`parse_sequence` 由 `{` 进入，
/// `parse_command` / `render_nested` 由 `\` 进入），所以"`{`+`\` 的出现次数"是递归深度的
/// 上界。64 层在任何线程栈上（tokio worker 默认 2 MB）都安全，因此常规公式都在当前栈上
/// 就地渲染，零额外开销。
const INLINE_RECURSION_BUDGET: usize = 64;

/// 处理深嵌套输入时使用的大栈（见 [`with_large_stack`]）。
const LARGE_STACK_SIZE: usize = 64 * 1024 * 1024;

/// 在**独立的大栈线程**上执行 `body`。
///
/// `parse_sequence` / `parse_command` / `render_layout` 都是递归的，而 TS 的递归深度只受 JS
/// 调用栈限制（很松）。Rust 的栈溢出是**不可捕获的进程 abort**，而调用方可能位于 tokio
/// worker（默认 2 MB 栈）上，深层输入会在那里杀掉整个 TUI/agent 进程。因此超过
/// [`INLINE_RECURSION_BUDGET`] 的输入改在 64 MiB 栈的线程上解析：这足以容纳
/// [`MAX_PARSE_DEPTH`] = 8192 层的整个解析与布局过程。
///
/// 不用 `stacker`：它会引入 C 依赖（`psm`），而本 crate 其余部分没有 C 依赖。
///
/// 线程创建失败（极端资源耗尽）时返回 `None`：`renderLatex` 的 `None` 语义是"这条公式
/// 无法渲染"，调用方（markdown）会回退到原始文本——与栈溢出 abort 整个进程相比，这是
/// 唯一不扩大影响面的选择。
fn with_large_stack<T: Send>(body: impl FnOnce() -> T + Send) -> Option<T> {
    std::thread::scope(|scope| {
        let handle = std::thread::Builder::new()
            .stack_size(LARGE_STACK_SIZE)
            .spawn_scoped(scope, body)
            .ok()?;
        match handle.join() {
            Ok(value) => Some(value),
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

/// `renderLatex(source, options)`。
///
/// 返回 `None` ⟺ 源串包含不支持或格式错误的语法（TS 的 `undefined`），或会触发 TS 异常
/// 的输入（见模块文档）。
///
/// 普通输入在当前线程同步完成；`{`/`\` 的出现次数超过 `INLINE_RECURSION_BUDGET` 时
/// 临时创建一条 64 MiB 栈的线程（见 `with_large_stack`），线程创建失败也返回 `None`。
/// 不读文件、不访问环境变量；空输入返回 `Some("")`。
pub fn render_latex(source: &str, options: &RenderLatexOptions) -> Option<String> {
    if recursion_budget(source) <= INLINE_RECURSION_BUDGET {
        return render_latex_inner(source, options, false);
    }
    let options = *options;
    with_large_stack(move || render_latex_inner(source, &options, true)).flatten()
}

/// `recursion_budget(source)` 的上界统计（`{` 与 `\` 的出现次数）。
///
/// 计的是**字符出现次数**而非实际嵌套深度，只是一个保守上界：每层递归必定消费 `{` 或 `\`
/// 之一（见 [`INLINE_RECURSION_BUDGET`]）。偏大只会让输入多走一次大栈线程，不改变输出。
fn recursion_budget(source: &str) -> usize {
    source
        .chars()
        .filter(|character| *character == '{' || *character == '\\')
        .count()
}

/// 编码空间里的完整渲染：`encode_source` → [`render_latex_encoded`] → `decode_output`。
///
/// 编解码必须成对：解析、布局、宽度计算全程工作在编码空间，占位符与用户输入因此不会
/// 混淆。`on_large_stack` 只做转发，表示当前是否已经在大栈线程上（决定布局阶段能否再
/// 开一条线程）。
fn render_latex_inner(
    source: &str,
    options: &RenderLatexOptions,
    on_large_stack: bool,
) -> Option<String> {
    // 顶层编码一次（字面占位区字符转义）；解析/布局全程在编码空间里，最后解码成可见文本。
    let encoded = encode_source(source);
    render_latex_encoded(&encoded, options, on_large_stack).map(|rendered| decode_output(&rendered))
}

/// 编码空间里的渲染主体（[`render_latex_inner`] 去掉编解码包装）。
///
/// 流程：
///
/// 1. [`LatexParser::render`] 得到可见文本与 `layout_nodes`；
/// 2. 没有布局节点：把 `PROTECTED_SPACE` 换成普通空格后直接返回（单行的
///    `cases`/`matrix` 会走到这里）；
/// 3. 有布局节点：[`layout_expansion_depth`] 先做环检测（`None` → 整体 `None`）并给出
///    展开深度；超过 [`MAX_LAYOUT_DEPTH`] → `None`；超过 [`INLINE_RECURSION_BUDGET`] 且
///    当前不在大栈线程上 → 换大栈线程执行 [`finish_layout`]；
/// 4. 否则在当前栈上执行 [`finish_layout`]。
fn render_latex_encoded(
    source: &str,
    options: &RenderLatexOptions,
    on_large_stack: bool,
) -> Option<String> {
    let mut layout_nodes: Vec<LayoutNode> = Vec::new();
    let rendered = LatexParser::new(source, &mut layout_nodes, options.display).render()?;
    if layout_nodes.is_empty() {
        return Some(rendered.replace(PROTECTED_SPACE, " "));
    }
    // 环 → TS 的 `RangeError` → `None`；深度决定用哪个栈展开（布局链可以比解析更深，
    // 例如用户输入里的字面量布局标记把互不相邻的节点串成一条长链）。
    let depth = layout_expansion_depth(&rendered, &layout_nodes)?;
    if depth > MAX_LAYOUT_DEPTH {
        return None;
    }
    if depth > INLINE_RECURSION_BUDGET && !on_large_stack {
        return with_large_stack(move || finish_layout(&rendered, &layout_nodes)).flatten();
    }
    finish_layout(&rendered, &layout_nodes)
}

/// `render_layout` 之后的收尾：去除公共缩进、合并受保护空格。
///
/// 在所有**非空行**上取最小缩进（布局行的前导空白都来自解析阶段折叠出的 ASCII 空格或
/// `pad_layout_line` 的补齐，所以按字节计算与按列计算一致），每行截掉这段缩进再去行尾
/// 空白，用 `\n` 连接后整体去尾部空白，最后把 `PROTECTED_SPACE` 换成普通空格。全部行都是
/// 空白时返回 `Some("")`（空公式是合法结果，不是 `None`）。
fn finish_layout(rendered: &str, layout_nodes: &[LayoutNode]) -> Option<String> {
    let lines = render_layout(rendered, layout_nodes).lines;
    let indentation = lines
        .iter()
        .filter(|line| !trim_js(line).is_empty())
        .map(|line| line.len() - line.trim_start_matches(is_js_whitespace).len())
        .min();
    let Some(indentation) = indentation else {
        return Some(String::new());
    };
    let body: Vec<String> = lines
        .iter()
        .map(|line| {
            let start = indentation.min(line.len());
            trim_end_js(&line[start..]).to_string()
        })
        .collect();
    Some(trim_end_js(&body.join("\n")).replace(PROTECTED_SPACE, " "))
}

#[cfg(test)]
mod symbol_table_tests {
    use super::symbol;

    #[test]
    fn pi_uses_the_typescript_symbol_name() {
        assert_eq!(symbol("Pi").map(|value| value.text), Some("Π"));
        assert!(symbol("Kiana").is_none());
    }
}
