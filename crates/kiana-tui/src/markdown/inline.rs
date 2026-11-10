//! inline tokens：`marked@18` 的 `Tokenizer.ts` 行内部分 + `marked-cjk-friendly@0.1.2`
//! 的 `emStrong` 重写 + 严格删除线。
//!
//! `inlineTokens` 的执行顺序固定：LaTeX 扩展 → escape → tag → link → reflink →
//! emStrong → codespan → br → del → autolink → url → text。掩码（mask）保持长度不变，
//! 供 `emStrong`/`del` 用「从尾部对齐」的方式截取同一区段。
//!
//! # 这个文件负责什么
//!
//! 块级 tokenizer 只把源码切成段落、标题、表格单元格等块；块内部的强调、链接、行内代码、
//! 自动链接等由本文件解析。`Lexer::lex` 在块级阶段把待解析片段登记进 `inline_queue`，
//! 块级结束后对每一项调用 `inline_tokens`，返回值再按 `inline_id` 回填到
//! paragraph/heading/table cell 的 `tokens` 字段；渲染侧由 `components::markdown` 的
//! `render_inline_tokens` 消费。链接文字、强调文字、删除线文字又会在本文件内递归调用
//! `inline_tokens`，所以一次解析可能嵌套多层。
//!
//! # 执行流程
//!
//! 1. 掩码：入口处一次性从完整 `src` 生成等长（按 UTF-16 码元计）的掩码副本，见下节。
//! 2. 主循环：每一轮只匹配 `source` 的最开头，命中哪个 tokenizer 就产出一个 token，
//!    并按该 token 的 `raw` 前进；`raw` 必须是当前 `source` 的前缀。
//! 3. 递归：link 的 label、em/strong 的内部文本、del 的内部文本会再次进入
//!    `inline_tokens`，得到子 token 列表。
//!
//! tokenizer 的尝试顺序与 marked 一致，顺序本身是语义的一部分（例如 `link` 必须先于
//! `reflink`，否则 `[a](b)` 会先被当成引用式链接候选）。未命中任何 tokenizer 时循环
//! 结束，剩余源码不再产出 token。
//!
//! # 掩码（mask）与「从尾部对齐」
//!
//! 三种掩码都只由本文件在入口处生成一次：
//!
//! - 引用式链接（仅在 `self.links` 非空且源码含 `[` 时）：把按原始键判定为已定义的
//!   引用式链接替换成等长占位串——通常是 `[aaa…]`，label 内含链接时只掩掉 label 里的
//!   链接；未定义的引用保持原样（它们本来就是普通文本）。
//! - 转义标点（`\<标点>`）：替换成同样多码元的 `+`，使被转义的 `*`/`_` 不再像定界符。
//! - 链接/行内代码/HTML（`rules.block_skip`）：替换成 `[aaa…]`。
//!
//! 掩码只保证 UTF-16 长度不变，字符本身会变。`emStrong` 需要的是「当前剩余源码在完整
//! 源码中的位置关系」：它先按 `l_length` 从尾部截取 `masked_src`，得到与「去掉左定界符
//! 后的 `source`」等长的掩码串，再在这个串上扫描右定界符。掩码长度一旦算错，右定界符
//! 的位置就会整体错位。
//!
//! # 状态（`Lexer::state`）
//!
//! 本文件直接读写 `Lexer` 上的三个状态位，它们跨 `inline_tokens` 调用保留，只有下面列出
//! 的分支会改写：
//!
//! - `in_link`：正在解析链接文字。`output_link` 进入时置 true、退出时置 false；`tag`
//!   遇到 `<a …>` / `</a>` 时也会切换。`url` 只在 `!in_link` 时尝试。
//! - `in_raw_block`：正在 `<pre|code|kbd|script>` 原始块内，由 `tag` 切换；`inline_text`
//!   把它记录到 `Text::escaped`。`output_link` 在「内层含链接而拒绝外层链接」的路径上
//!   会恢复它。
//! - `link_emitted`：`output_link` 是否在本段递归中产出过非图片链接（`autolink`/`url`
//!   直接返回 Link token，不置位）。`output_link` 在递归解析 label 前把它清零、递归后
//!   读取，用于实现 CommonMark「链接不能嵌套链接」。
//!
//! # 注意事项
//!
//! - 需要与 TS 逐单位对齐的索引换算都按 UTF-16 码元（JS 字符串单位）进行，不是直接用
//!   字节下标或 `char` 计数；换算函数在 `super::lexer`（`utf16_len`、`byte_at_utf16`、
//!   `slice_utf16` 等）。ASCII 输入下字节数、码元数、码点数一致。
//! - 正则执行失败（`fancy_regex` 返回 `Err`）一律按「不匹配」处理（`unwrap_or(false)`）；
//!   掩码遍历中途出错会停止掩码，剩余部分保持原文。
//! - 每个 tokenizer 返回的 `raw` 必须非空且是 `source` 的前缀；主循环有「长度必须严格
//!   变小」的保护，长度不缩小时直接退出，以免死循环。

use fancy_regex::{Captures, Regex};

use super::Token;
use super::block::{find_closing_bracket, replace_any_punctuation};
use super::lexer::{
    Lexer, byte_at_utf16, js_slice_from_1, js_slice_from_1_units, slice_from_utf16, slice_utf16,
    substring_after_raw, utf16_len,
};
use crate::text::trim_js;

/// `[...str].length`（按码点计数）。
///
/// 左定界符的匹配 `m0` 里可能还带着定界符后的一个非空白、非同类定界符的字符
/// （星号分支为第 1/2 组，下划线分支为第 3/4 组），
/// 用「码点数 - 1」正好得到开定界符的个数；定界符本身是 ASCII，所以这个值就是需要的
/// UTF-16 码元数。
fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// `str.slice(start, end)`（JS 语义：UTF-16 码元，负下标从尾部算）。
///
/// 下标越界时夹到 `[0, len]`；`end < start` 时结果为空串（与 JS 的 `slice` 一致）。
fn slice_units(text: &str, start: i64, end: i64) -> &str {
    let len = utf16_len(text) as i64;
    let start = if start < 0 {
        (len + start).max(0)
    } else {
        start.min(len)
    };
    let end = if end < 0 {
        (len + end).max(0)
    } else {
        end.min(len)
    };
    let end = end.max(start);
    let start_byte = byte_at_utf16(text, start as usize);
    let end_byte = byte_at_utf16(text, end as usize);
    &text[start_byte..end_byte]
}

/// 把单个 UTF-16 码元当成 JS 字符串的一个码元：代理项不是合法字符，正则也不会匹配它。
fn char_from_unit(unit: u16) -> Option<char> {
    char::from_u32(unit as u32)
}

fn unit_string(unit: Option<u16>) -> Option<char> {
    unit.and_then(char_from_unit)
}

/// 取第一个非空捕获组。
///
/// 本文件的右定界符正则用多个分支表示同一种「定界符 run」，未参与匹配的组由
/// `fancy_regex` 返回 `None`；参与匹配但内容为空串的组也在这里排除。
fn first_group<'t>(caps: &Captures<'t>, indices: &[usize]) -> Option<&'t str> {
    for &index in indices {
        if let Some(matched) = caps.get(index)
            && !matched.as_str().is_empty()
        {
            return Some(matched.as_str());
        }
    }
    None
}

/// 给定捕获组里是否有任意一个参与匹配且非空。
///
/// 和 `first_group` 的差别只在于不关心具体内容，用来判断「当前匹配来自正则的哪一类
/// 分支」（例如定界符是左定界符还是两侧都可的 run）。
fn group_present(caps: &Captures, indices: &[usize]) -> bool {
    indices.iter().any(|&index| {
        caps.get(index)
            .is_some_and(|matched| !matched.as_str().is_empty())
    })
}

/// 单码元是否能匹配某个字符类正则（代理项 → false，与 JS 对孤立代理的行为一致）。
///
/// 正则执行错误（如回溯超限）按 `false` 处理，调用方会把这个字符视为「不匹配」。
fn unit_matches(regex: &Regex, unit: Option<u16>) -> bool {
    let Some(ch) = unit_string(unit) else {
        return false;
    };
    let mut buffer = [0u8; 4];
    regex.is_match(ch.encode_utf8(&mut buffer)).unwrap_or(false)
}

/// 整段 `text` 是否能被 `regex` 匹配。
///
/// `Regex::is_match` 是「任意位置有匹配」而非全串匹配；正则执行错误按 `false` 处理。
fn text_matches(regex: &Regex, text: &str) -> bool {
    regex.is_match(text).unwrap_or(false)
}

/// 行内合并：把 `raw`/`text` 追加到最后一个 text token 上。
///
/// 只在最后一个 token 恰好是 `Text` 时生效，否则静默什么也不做；调用点都先做过
/// `matches!(tokens.last(), Some(Token::Text { .. }))` 判断，因此不会丢数据。
fn append_to_last_text(tokens: &mut [Token], raw: &str, text: &str) {
    if let Some(Token::Text {
        raw: raw_slot,
        text: text_slot,
        ..
    }) = tokens.last_mut()
    {
        raw_slot.push_str(raw);
        text_slot.push_str(text);
    }
}

/// 未转义标点掩码：每个匹配替换成同样 UTF-16 长度的 `+`。
///
/// 匹配来自 `rules.any_punctuation`（`\<标点>`），整段 `\*` 都会被掩掉，因此之后扫描
/// 强调定界符时不会再看到这个 `*`。`find_iter` 中途出错时停止掩码，剩余原文原样拼回。
fn mask_escaped_punctuation(regex: &Regex, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for matched in regex.find_iter(text) {
        let Ok(matched) = matched else { break };
        out.push_str(&text[last..matched.start()]);
        out.push_str(&"+".repeat(utf16_len(matched.as_str())));
        last = matched.end();
    }
    out.push_str(&text[last..]);
    out
}

/// 链接/代码/HTML 掩码：替换成 `[aaa...]`，长度不变。
///
/// 填充用的 `[`、`]`、`a` 都不是强调定界符，所以这些区段里的 `*`/`_` 不会影响右定界符
/// 扫描。`units` 是匹配文本的 UTF-16 码元数，替换后长度与原文相同。
fn mask_blocks(regex: &Regex, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for matched in regex.find_iter(text) {
        let Ok(matched) = matched else { break };
        out.push_str(&text[last..matched.start()]);
        let units = utf16_len(matched.as_str());
        out.push('[');
        out.push_str(&"a".repeat(units.saturating_sub(2)));
        out.push(']');
        last = matched.end();
    }
    out.push_str(&text[last..]);
    out
}

/// `emStrong` 的结果：`Fallback` 表示扩展 tokenizer 返回 `false`，应回退到 marked 基础实现。
///
/// 区分 `None`（扩展明确判定「此处没有强调」）与 `Fallback`（扩展无法处理）是 TS 扩展
/// 协议的一部分：前者不再尝试基础实现，后者必须。当前移植里两条路径共用同一份
/// `em_strong_l_delim` 并做同样的下划线词内检查，所以扩展返回 `Fallback` 的两种情形
/// （左定界符不匹配、`_` 紧跟字母数字）在基础实现里同样返回 `None`；保留该变体是为了
/// 与扩展协议一一对应。
enum EmOutcome {
    Token(Token),
    Fallback,
    None,
}

impl Lexer {
    // -----------------------------------------------------------------------
    // 主循环
    // -----------------------------------------------------------------------
    /// 把一段行内 markdown 解析成 token 列表（marked 的 `Lexer.inlineTokens`）。
    ///
    /// `src` 是一个块内部的行内源码。正常情况下各 token 的 `raw` 依次拼接应等于 `src`；
    /// 若某一轮没有任何 tokenizer 命中，循环会提前结束，剩余源码不会再产出 token。
    /// 函数会读写 `self.state` 并可能递归调用自身；`self.links` 必须已在块级阶段收集完毕
    /// （引用式链接可以定义在使用之后）。
    pub(crate) fn inline_tokens(&mut self, src: &str) -> Vec<Token> {
        let mut tokens: Vec<Token> = Vec::new();
        let mut masked_src = src.to_string();

        // 掩码用于 emStrong 的右定界符扫描，必须与原文保持相同的 UTF-16 码元数：
        // emStrong 会把 masked_src 的尾部与当前剩余源码对齐后再扫描。
        // 掩掉 reflink，避免干扰 em/strong
        if !self.links.is_empty() && src.contains('[') {
            masked_src = self.mask_reflinks(&masked_src);
        }
        // 掩掉转义标点（保持码元长度）：`\*` 里的 `*` 不能再当定界符
        let any_punctuation = &self.rules.any_punctuation;
        masked_src = mask_escaped_punctuation(any_punctuation, &masked_src);
        // 掩掉链接/代码/HTML：这些区段内部的 `*`/`_` 不参与外层强调配对
        let block_skip = &self.rules.block_skip;
        masked_src = mask_blocks(block_skip, &masked_src);

        let mut keep_prev_char = false;
        let mut prev_char: Option<u16> = None;
        let mut source = src.to_string();
        let mut src_length = usize::MAX;

        while !source.is_empty() {
            // 防死循环保护：只有 source 的字节长度严格变小才继续。若某个 tokenizer
            // 返回了零长度 raw，下一轮长度不变，这里会直接退出。
            let current_len = source.len();
            if current_len < src_length {
                src_length = current_len;
            } else {
                break;
            }

            // prev_char 只由 text 分支写入；其它 token 之后的那一轮会清零
            // （text 分支把 keep_prev_char 置 true，使下一轮保留它）。
            if !keep_prev_char {
                prev_char = None;
            }
            keep_prev_char = false;

            // 以下 tokenizer 的顺序与 marked 的 inlineTokens 一致：每个函数只看 source
            // 开头，先命中者消费源码。顺序会影响解析结果，不能随意调整。
            // 扩展（latex）
            if let Some(token) = super::latex_ext::tokenize_inline_latex(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // escape
            if let Some(token) = self.escape(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // tag
            if let Some(token) = self.tag(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // link
            if let Some(token) = self.link(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // reflink / nolink
            if let Some(token) = self.reflink(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                // 未定义的引用式链接只吐出一个字符的 Text token；它要并入前一个 Text，
                // 避免 token 流里出现大量碎片（与下面 text 分支的处理相同）。
                if matches!(token, Token::Text { .. })
                    && matches!(tokens.last(), Some(Token::Text { .. }))
                {
                    let (raw, text) = match &token {
                        Token::Text { raw, text, .. } => (raw.clone(), text.clone()),
                        _ => unreachable!(),
                    };
                    append_to_last_text(&mut tokens, &raw, &text);
                } else {
                    tokens.push(token);
                }
                continue;
            }

            // em & strong（masked_src 是完整源码的等长掩码，函数内部按左定界符长度
            // 从尾部对齐到当前 source）
            if let Some(token) = self.em_strong(&source, &masked_src, prev_char) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // codespan
            if let Some(token) = self.codespan(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // br
            if let Some(token) = self.inline_br(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // del（严格删除线）
            if let Some(token) = self.del(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // autolink
            if let Some(token) = self.autolink(&source) {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // url（gfm）
            if !self.state.in_link
                && let Some(token) = self.url(&source)
            {
                source = substring_after_raw(&source, token.raw()).to_string();
                tokens.push(token);
                continue;
            }

            // text（裁剪到扩展起点之前）
            let mut cut_src = source.clone();
            if let Some(start_index) =
                super::latex_ext::inline_extension_start(js_slice_from_1(&source))
            {
                // 扩展的 start() 告诉 marked 下一个 `$`/`\(`/`\[` 在哪；text token 只能
                // 吃到它之前，否则整段 `$...$` 会被当成普通文本吞掉。
                // `e.substring(0, startIndex + 1)`：startIndex 是 `e.slice(1)` 里的 UTF-16 下标，
                // 换回原串要补上被切掉的码元数（astral 首字符为 2）。
                let end = start_index + js_slice_from_1_units(&source);
                cut_src = slice_utf16(&source, end).to_string();
            }
            if let Some(token) = self.inline_text(&cut_src) {
                let raw = token.raw().to_string();
                source = substring_after_raw(&source, &raw).to_string();
                let text = token.text().unwrap_or("").to_string();
                // raw 以 `_` 结尾时不更新 prev_char（沿用上一轮的值），其余情况取 raw 的
                // 最后一个 UTF-16 码元，供下一轮的 `_` 词内判定与 CJK 邻接判定使用。
                if !raw.ends_with('_') {
                    prev_char = raw.encode_utf16().last();
                }
                keep_prev_char = true;
                if matches!(tokens.last(), Some(Token::Text { .. })) {
                    append_to_last_text(&mut tokens, &raw, &text);
                } else {
                    tokens.push(token);
                }
                continue;
            }

            // 没有任何 tokenizer 命中：结束循环，剩余源码不再产出 token。
            if !source.is_empty() {
                break;
            }
        }

        tokens
    }

    // -----------------------------------------------------------------------
    // 掩码与 linkInText
    // -----------------------------------------------------------------------
    /// 扫描整段源码里所有 `reflink_search` 候选（`[label][ref]`、`[ref]`），把其中
    /// 「键已定义」的引用式链接替换成等长占位串，见 `mask_reflink`。
    ///
    /// 只用于生成 `emStrong` 的掩码；未定义的引用保持原样，因为它们本来就是普通文本，
    /// 其中的强调应当照常解析。`find_iter` 出错时停止替换，剩余部分原样拼回。
    fn mask_reflinks(&self, masked: &str) -> String {
        let regex = &self.rules.reflink_search;
        let mut out = String::with_capacity(masked.len());
        let mut last = 0usize;
        for matched in regex.find_iter(masked) {
            let Ok(matched) = matched else { break };
            out.push_str(&masked[last..matched.start()]);
            out.push_str(&self.mask_reflink(matched.as_str()));
            last = matched.end();
        }
        out.push_str(&masked[last..]);
        out
    }

    /// 掩掉一个引用式链接候选；返回值与 `match0` 的 UTF-16 码元数相同。
    ///
    /// 键取最后一个 `[` 与末尾 `]` 之间的原文（`ref_start` 是 UTF-16 下标）。与 `reflink`
    /// 不同，这里不做空白折叠/小写归一化，也不做 `[label][]` / `[label]` 的 label 回退；
    /// 按这个原始键在 `self.links` 中查表失败的候选原样返回。
    fn mask_reflink(&self, match0: &str) -> String {
        let ref_start = match match0.rfind('[') {
            Some(byte_index) => utf16_len(&match0[..byte_index]) as i64,
            None => -1,
        };
        let key = slice_units(match0, ref_start + 1, -1);
        if !self.links.contains_key(key) {
            return match0.to_string();
        }
        // CommonMark：链接不能嵌套链接。含链接的候选本身不会成为链接，
        // 因此只掩掉其中的链接，保留仍然生效的强调。
        if ref_start > 1 && !match0.starts_with('!') {
            let text = slice_units(match0, 1, ref_start - 1);
            if self.link_in_text(text) {
                // label 里已有链接：只把 label 内的链接掩掉，方括号结构与 label 文字
                // 保持不变（长度不变），`[ref]` 部分整体填 `a`。
                let replaced = self.mask_reflinks(text);
                let tail = utf16_len(match0)
                    .saturating_sub(ref_start as usize)
                    .saturating_sub(2);
                return format!("[{replaced}][{}]", "a".repeat(tail));
            }
        }
        // 整体替换成等长的 `[aaa…]`：填 `a` 而不是定界符，避免参与强调配对。
        format!("[{}]", "a".repeat(utf16_len(match0).saturating_sub(2)))
    }

    /// `linkInText(text)`：这段文字里是否已经含有链接（图片不算）。
    ///
    /// 两条来源：`block_skip` 中同时能匹配 `link` 规则、且前面不是 `!` 的（真正的行内
    /// 链接），以及键已定义的引用式链接。引用式链接的 label 里若还有链接则继续递归。
    /// 用于 `output_link` 的嵌套链接判定，也被 `mask_reflink` 用来判断候选 label 内是否
    /// 已有链接；不产生 token。
    fn link_in_text(&self, text: &str) -> bool {
        if !text.contains('[') {
            return false;
        }

        // 行内链接：block_skip 也会匹配代码与 HTML，必须再用 link 规则确认。
        let link_rule = &self.rules.link;
        for matched in self.rules.block_skip.find_iter(text) {
            let Ok(matched) = matched else { break };
            let before_is_bang = text[..matched.start()].ends_with('!');
            if text_matches(link_rule, matched.as_str()) && !before_is_bang {
                return true;
            }
        }

        // 引用式链接：忽略图片与未定义键；label 内含链接时外层不算链接（递归判定）。
        for matched in self.rules.reflink_search.find_iter(text) {
            let Ok(matched) = matched else { break };
            let match0 = matched.as_str();
            let ref_start = match match0.rfind('[') {
                Some(byte_index) => utf16_len(&match0[..byte_index]) as i64,
                None => -1,
            };
            let key = slice_units(match0, ref_start + 1, -1);
            if match0.starts_with('!') || !self.links.contains_key(key) {
                continue;
            }
            if ref_start > 1 && self.link_in_text(slice_units(match0, 1, ref_start - 1)) {
                continue;
            }
            return true;
        }

        false
    }

    // -----------------------------------------------------------------------
    // escape
    // -----------------------------------------------------------------------
    /// 反斜杠转义：`\` + 标点，产出 `Escape` token（`text` 是去掉反斜杠的字符）。
    fn escape(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.escape.captures(src).ok().flatten()?;
        Some(Token::Escape {
            raw: cap.get(0)?.as_str().to_string(),
            text: cap.get(1)?.as_str().to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // tag
    // -----------------------------------------------------------------------
    /// 行内 HTML 标签（`rules.tag`），同时维护 `in_link` / `in_raw_block` 状态。
    ///
    /// 先更新状态、再构造 token，所以 `Html::in_link` / `Html::in_raw_block` 记录的是
    /// 「这个标签处理完之后」的状态。`in_raw_block` 之后会被 `inline_text` 复制到
    /// `Text::escaped`。token 的 `block`/`pre` 固定为 false：块级 HTML 由 `block` 模块
    /// 处理，行内只可能产出非块级标签。
    fn tag(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.tag.captures(src).ok().flatten()?;
        let raw = cap.get(0)?.as_str().to_string();

        if !self.state.in_link && text_matches(&rules.start_a_tag, &raw) {
            self.state.in_link = true;
        } else if self.state.in_link && text_matches(&rules.end_a_tag, &raw) {
            self.state.in_link = false;
        }
        if !self.state.in_raw_block && text_matches(&rules.start_pre_script_tag, &raw) {
            self.state.in_raw_block = true;
        } else if self.state.in_raw_block && text_matches(&rules.end_pre_script_tag, &raw) {
            self.state.in_raw_block = false;
        }

        Some(Token::Html {
            raw: raw.clone(),
            block: false,
            pre: false,
            text: raw,
            in_link: self.state.in_link,
            in_raw_block: self.state.in_raw_block,
        })
    }

    // -----------------------------------------------------------------------
    // link
    // -----------------------------------------------------------------------
    /// 行内链接/图片 `[label](href "title")`（marked 的 `link`）。
    ///
    /// 依次处理：尖括号包裹的 href（必须成对且结尾 `>` 未被转义）、普通 href 的括号配对
    /// 截断、title 的引号剥离、href/title 里的 `\<标点>` 反转义，最后交给 `output_link`
    /// 生成 token。任何一步判定语法不成立都返回 `None`，由主循环继续尝试后续 tokenizer。
    fn link(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.link.captures(src).ok().flatten()?;
        let mut cap0 = cap.get(0)?.as_str().to_string();
        let cap1 = cap.get(1)?.as_str().to_string();
        let mut cap2 = cap.get(2)?.as_str().to_string();
        let mut cap3: Option<String> = cap.get(3).map(|matched| matched.as_str().to_string());

        let trimmed_url = trim_js(&cap2).to_string();
        if text_matches(&rules.start_angle_bracket, &trimmed_url) {
            // commonmark 要求尖括号配对
            if !text_matches(&rules.end_angle_bracket, &trimmed_url) {
                return None;
            }
            // 结尾尖括号不能被转义。`sliced` 去掉末尾的 `>`，`rtrim_slash` 再去掉
            // 末尾连续的反斜杠；两者码元数之差 = 1（`>` 本身）+ 结尾反斜杠数。
            // 差值为偶数说明反斜杠数是奇数，即这个 `>` 被转义，不能当作收尾。
            let sliced = &trimmed_url[..trimmed_url.len().saturating_sub(1)];
            let rtrim_slash = super::block::rtrim(sliced, '\\');
            if (utf16_len(&trimmed_url) - utf16_len(rtrim_slash)).is_multiple_of(2) {
                return None;
            }
        } else {
            // 找与 `(` 配对的右括号（`find_closing_bracket`）
            let last_paren_index = find_closing_bracket(&cap2);
            if last_paren_index == -2 {
                // 开括号多于闭括号
                return None;
            }
            if last_paren_index > -1 {
                // 按 marked 的公式截断 raw 与 href（`start` 普通链接取 4、图片取 5，
                // label 长度另加），并清空 title：括号提前配平的分支不再解析 title。
                let start = if cap0.starts_with('!') { 5 } else { 4 };
                let link_len = start + utf16_len(&cap1) + last_paren_index as usize;
                cap2 = slice_utf16(&cap2, last_paren_index as usize).to_string();
                cap0 = trim_js(slice_utf16(&cap0, link_len)).to_string();
                cap3 = Some(String::new());
            }
        }

        let mut href = cap2;
        // title 捕获包含外层引号/括号，`slice(1, -1)` 去掉首尾各一个字符。
        let title = match &cap3 {
            Some(value) if !value.is_empty() => slice_units(value, 1, -1).to_string(),
            _ => String::new(),
        };

        href = trim_js(&href).to_string();
        if text_matches(&rules.start_angle_bracket, &href) {
            href = slice_units(&href, 1, -1).to_string();
        }

        // href/title 里 `\<标点>` 形式的转义在这里去掉反斜杠；空 href 直接保留。
        let href = if href.is_empty() {
            href
        } else {
            replace_any_punctuation(&rules.any_punctuation, &href)
        };
        let title = if title.is_empty() {
            None
        } else {
            Some(replace_any_punctuation(&rules.any_punctuation, &title))
        };
        let is_image = cap0.starts_with('!');
        self.output_link(&cap1, &href, title, &cap0, is_image)
    }

    /// `outputLink(cap, link, raw, lexer, rules)`。
    ///
    /// 先把 label 里的 `\[`/`\]` 反转义，再递归解析 label 得到子 token。递归期间
    /// `in_link` 置 true（递归内的 `url` 不会把裸 URL 再包成链接），`link_emitted` 清零。
    ///
    /// 嵌套链接：非图片链接如果发现 label 内已经产出过链接，按 CommonMark
    /// 「Links may not contain other links」返回 `None`——调用方不会消费源码，主循环会
    /// 改用其它 tokenizer 重新解析这段文本；返回前恢复递归前的 `in_raw_block`。
    ///
    /// 图片（`is_image`）不参与嵌套判定：它的 label 会作为 `text` 供渲染侧按纯文本输出。
    /// 成功产出非图片链接时把 `link_emitted` 置 true，供更外层的递归判断嵌套。
    ///
    /// 注意：成功路径只恢复 `link_emitted` 与 `in_link`；递归内部若切换过
    /// `in_raw_block`，其值会保留下来。
    fn output_link(
        &mut self,
        label: &str,
        href: &str,
        title: Option<String>,
        raw: &str,
        is_image: bool,
    ) -> Option<Token> {
        let rules = self.rules;
        // label 中的 `\[`、`\]` 反转义回方括号（`output_link_replace` 的 `$1`）。
        let text = rules
            .output_link_replace
            .replace_all(label, "$1")
            .into_owned();

        // 保存外层状态：递归解析 label 时，内层可能产出链接、也可能切换 in_raw_block。
        self.state.in_link = true;
        let outer_link_emitted = self.state.link_emitted;
        let outer_in_raw_block = self.state.in_raw_block;
        self.state.link_emitted = false;
        let tokens = self.inline_tokens(&text);
        // 递归期间是否产出过链接，就是「label 里含链接」的判据。
        let text_has_link = self.state.link_emitted;
        self.state.link_emitted = outer_link_emitted;
        self.state.in_link = false;

        if !is_image {
            // CommonMark：“Links may not contain other links, at any level of nesting.”
            // 回退到 text，让内层链接生效；图片豁免（其文字会摊平成 alt）。
            if text_has_link {
                self.state.in_raw_block = outer_in_raw_block;
                return None;
            }
            self.state.link_emitted = true;
        }

        Some(Token::Link {
            raw: raw.to_string(),
            href: href.to_string(),
            title,
            text,
            tokens,
            image: is_image,
        })
    }

    // -----------------------------------------------------------------------
    // reflink / nolink
    // -----------------------------------------------------------------------
    /// 引用式链接/图片：`[label][ref]`、`[label][]`、`[label]`（marked 的 `reflink` 与
    /// `nolink` 两条规则）。
    ///
    /// 键取第二个方括号里的内容；为空（`[label][]` / `[label]`）时退回用 label。
    /// 键按 marked 规则归一化（连续空白折叠成一个空格、转小写）后在 `self.links` 中查找。
    /// 定义可以出现在使用位置之后——行内解析被推迟到块级阶段结束（见 `Lexer::lex`）。
    ///
    /// 找不到定义时不报错，而是只消费一个字符并产出 `Text` token，让主循环继续推进；
    /// 剩余部分会在后续轮次重新解析。
    fn reflink(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules
            .reflink
            .captures(src)
            .ok()
            .flatten()
            .or_else(|| rules.nolink.captures(src).ok().flatten())?;

        // 显式引用 `[label][ref]` 用第 2 组；`[label][]` / `[label]` 的第 2 组为空，
        // 退回用第 1 组（label）。
        let reference = match cap.get(2).map(|matched| matched.as_str()) {
            Some(value) if !value.is_empty() => value,
            _ => cap.get(1)?.as_str(),
        };
        let link_string = rules.multiple_space_global.replace_all(reference, " ");
        let key = link_string.to_lowercase();

        // 未定义：只吐一个字符的 Text token，避免消费整个候选导致后面的内容被吞。
        let raw = cap.get(0)?.as_str().to_string();
        let Some(def) = self.links.get(&key).cloned() else {
            let text = raw
                .chars()
                .next()
                .map(|ch| ch.to_string())
                .unwrap_or_default();
            return Some(Token::Text {
                raw: text.clone(),
                text,
                inline_id: None,
                tokens: Vec::new(),
                escaped: false,
            });
        };
        let label = cap.get(1)?.as_str().to_string();
        let is_image = raw.starts_with('!');
        self.output_link(&label, &def.href, def.title, &raw, is_image)
    }

    // -----------------------------------------------------------------------
    // em & strong
    // -----------------------------------------------------------------------
    /// em/strong 入口：先尝试 marked-cjk-friendly 的扩展实现，扩展报告 `Fallback` 时再走
    /// marked 的基础实现。
    ///
    /// `masked_src` 是完整源码的等长掩码；`prev_char` 是当前源码前一个字符的最后一个
    /// UTF-16 码元（没有则为 `None`）。
    fn em_strong(&mut self, src: &str, masked_src: &str, prev_char: Option<u16>) -> Option<Token> {
        match self.em_strong_cjk(src, masked_src, prev_char) {
            EmOutcome::Token(token) => Some(token),
            EmOutcome::None => None,
            EmOutcome::Fallback => self.em_strong_base(src, masked_src, prev_char),
        }
    }

    /// marked-cjk-friendly 的 `emStrong` 重写。
    ///
    /// 与 marked 的基础实现相比，它在两处放宽了 CommonMark 的定界符规则：左定界符紧邻
    /// CJK 字符/标点时也允许开强调，右定界符紧邻 CJK 字符时也允许闭合。这样
    /// `**中文。**后面` 这类没有空格分隔的写法才能解析（对应测试
    /// `renders_emphasis_delimited_next_to_cjk_punctuation`）。
    ///
    /// 执行阶段：
    ///
    /// 1. 在 `src` 开头匹配左定界符 `em_strong_l_delim`；
    /// 2. 下划线词内规则与邻接判定决定是否开强调（下划线词内规则命中时返回 `Fallback`，
    ///    交给基础实现，最终同样返回 `None`；邻接判定不满足返回 `None`）；
    /// 3. 在「掩码串去掉左定界符后的部分」上顺序扫描右定界符，按 CommonMark 的
    ///    delimiter run 规则累计 `delim_total` / `mid_delim_total`；
    /// 4. 找到可闭合的 run 后，用「左定界符长度 + 中间文本长度 + 右 run 实际使用长度」
    ///    重建 `raw`，按 `min(l_length, trimmed)` 的奇偶决定 `Em` 还是 `Strong`；
    /// 5. 递归解析内部文本。
    ///
    /// 扫描结束或提前中断时仍未找到可闭合的 run 返回 `None`（不是 `Fallback`：扩展已经
    /// 成功匹配了左定界符；正则执行错误也归入这一类）。
    fn em_strong_cjk(&mut self, src: &str, masked_src: &str, prev_char: Option<u16>) -> EmOutcome {
        let rules = self.rules;
        let Some(cap) = rules.em_strong_l_delim.captures(src).ok().flatten() else {
            return EmOutcome::Fallback;
        };
        let m0 = match cap.get(0) {
            Some(matched) => matched.as_str().to_string(),
            None => return EmOutcome::Fallback,
        };
        // `cap.len()` 含组 0。显式捕获组有 4 个时按 `[1,2,3,4]` / `[1,3]` 取组，否则按
        // `[3]` / `[1,2]` 的另一套下标取组。本移植复用的 `em_strong_l_delim` 恰好有
        // 4 个显式捕获组（长度 5），所以 `modern_layout` 恒为 true，按另一套下标取组的
        // 分支在当前规则下不会执行。
        let modern_layout = cap.len() >= 5;
        if modern_layout && !group_present(&cap, &[1, 2, 3, 4]) {
            return EmOutcome::None;
        }

        // CommonMark 的下划线词内规则：`_` 紧跟在字母/数字之后时不能开强调
        // （`foo_bar_baz` 里的 `_` 不产生强调）。
        let underscore_word_char = if modern_layout {
            group_present(&cap, &[4])
        } else {
            group_present(&cap, &[3])
        };
        if underscore_word_char && unit_matches(&rules.unicode_alpha_numeric, prev_char) {
            return EmOutcome::Fallback;
        }

        // `next_char` 只取「定界符后紧跟标点」的捕获组；后面是普通字符时取不到（`None`），
        // 视为可以开强调。`prev_is_cjk` 与 `next` 的 CJK 判定是扩展相对基础实现多出来的
        // 放行条件。
        let next_char = if modern_layout {
            first_group(&cap, &[1, 3])
        } else {
            first_group(&cap, &[1, 2])
        };
        let prev_is_cjk = self.is_prev_char_cjk(prev_char, masked_src, src);

        let enters = match next_char {
            None => true,
            Some(next) => {
                prev_char.is_none()
                    || unit_matches(&rules.punctuation, prev_char)
                    || prev_is_cjk
                    || text_matches(&rules.cjk_test, next)
            }
        };
        if !enters {
            return EmOutcome::None;
        }

        let l_length = char_count(&m0) - 1;
        let mut delim_total = l_length as i64;
        let mut mid_delim_total = 0i64;
        let first_unit = m0.as_bytes().first().copied().unwrap_or(b'_') as u16;
        let prev_char_is_same_delimiter = modern_layout && prev_char == Some(first_unit);
        // 用模式串里是否含 `(?!~)` 判断 `em_strong_r_delim_ast` 是不是 GFM 形态，据此在
        // CJK 版与基础版之间选择。解析选项固定为 GFM（见 `markdown` 模块文档），所以这里
        // 恒为 true，选中 `em_strong_r_delim_ast_cjk`。
        let is_gfm = rules.em_strong_r_delim_ast.as_str().contains("(?!~)");
        let end_reg = if m0.starts_with('*') {
            if is_gfm {
                &rules.em_strong_r_delim_ast_cjk
            } else {
                &rules.em_strong_r_delim_ast
            }
        } else {
            &rules.em_strong_r_delim_und_cjk
        };

        // 掩码串与完整源码 UTF-16 等长；去掉左定界符的 l_length 个码元后，掩码串的尾部
        // 与「src 去掉左定界符」逐码元对齐。于是右定界符的 match_start 可以直接换算成
        // 「中间文本的 UTF-16 长度」。
        let src_units = utf16_len(src);
        let clipped_masked_src = slice_from_utf16(masked_src, src_units.saturating_sub(l_length));
        let mut position = 0usize;
        let mut r_match = match end_reg.captures_from_pos(clipped_masked_src, position) {
            Ok(value) => value,
            // 正则执行错误按「未匹配」处理。
            Err(_) => return EmOutcome::None,
        };
        while let Some(caps) = r_match {
            let full = match caps.get(0) {
                Some(matched) => matched,
                None => break,
            };
            let match_start = full.start();
            let match_end = full.end();
            // 没有捕获到定界符本身：跳过这个匹配；`max(match_start + 1)` 保证位置一定前进。
            let Some(r_delim) = first_group(&caps, &[1, 2, 3, 4, 5, 6]) else {
                position = match_end.max(match_start + 1);
                r_match = end_reg
                    .captures_from_pos(clipped_masked_src, position)
                    .ok()
                    .flatten();
                continue;
            };
            let r_length = char_count(r_delim);
            // 右定界符正则中带捕获组的分支按组号区分：3/4 组是「看起来像左定界符」
            // 的 run，5/6 组是两侧都可（既能开也能闭）的 run，1/2 组是普通闭合 run
            // （ast 版有 6 个捕获组，und 版有 5 个、没有第 6 组）。
            let is_cjk_adjacent = is_cjk_adjacent_to_delimiter(&caps, clipped_masked_src, rules);
            let is_left_only = group_present(&caps, &[3, 4]);
            let is_both = group_present(&caps, &[5, 6]);

            // CJK 放宽：本来是左定界符的 run，若紧邻 CJK 字符也可以当闭合用。
            if is_left_only && !is_cjk_adjacent {
                delim_total += r_length as i64;
                position = match_end.max(match_start + 1);
                r_match = end_reg
                    .captures_from_pos(clipped_masked_src, position)
                    .ok()
                    .flatten();
                continue;
            }
            if is_both || is_cjk_adjacent {
                // CommonMark 的「3 的倍数」规则（marked 的实现）：左 run 长度不是 3 的
                // 倍数、且左右长度之和是 3 的倍数时，这个右 run 不能用于闭合，计入
                // mid_delim_total 后继续扫描。
                if !l_length.is_multiple_of(3) && (l_length + r_length).is_multiple_of(3) {
                    mid_delim_total += r_length as i64;
                    position = match_end.max(match_start + 1);
                    r_match = end_reg
                        .captures_from_pos(clipped_masked_src, position)
                        .ok()
                        .flatten();
                    continue;
                }
                // 对应基础实现的 `midRun` 提前退出：左定界符前一个字符也是同种定界符、
                // 且当前 run 两侧都可时停止扫描，避免继续错配更短的强调。
                if prev_char_is_same_delimiter && is_both {
                    break;
                }
            }

            // 先用这个 run 的长度抵扣左 run；仍有剩余说明左 run 还没被用尽，继续找。
            delim_total -= r_length as i64;
            if delim_total > 0 {
                position = match_end.max(match_start + 1);
                r_match = end_reg
                    .captures_from_pos(clipped_masked_src, position)
                    .ok()
                    .flatten();
                continue;
            }

            // 去掉多余字符：`*a***` → `*a*`
            // 实际只能用右 run 的末尾若干码元：加上前面跳过的 mid run 后不能超过右 run
            // 自身长度（`min`），也不能为负（`max(0)`）。
            let trimmed = (r_length as i64)
                .min(r_length as i64 + delim_total + mid_delim_total)
                .max(0) as usize;
            // 带捕获组的分支会把定界符前的一个字符一起匹配进 `full`，raw 长度要把它算上。
            let last_char_length = full
                .as_str()
                .chars()
                .next()
                .map(char::len_utf16)
                .unwrap_or(1);
            let raw_units = l_length
                + utf16_len(&clipped_masked_src[..match_start])
                + last_char_length
                + trimmed;
            let raw = slice_utf16(src, raw_units).to_string();

            // `min(l_length, trimmed)` 为奇数 → Em；偶数 → Strong（marked 的判定）。
            return if (l_length.min(trimmed)) % 2 == 1 {
                let text = slice_units(&raw, 1, -1).to_string();
                let tokens = self.inline_tokens(&text);
                EmOutcome::Token(Token::Em { raw, text, tokens })
            } else {
                let text = slice_units(&raw, 2, -2).to_string();
                let tokens = self.inline_tokens(&text);
                EmOutcome::Token(Token::Strong { raw, text, tokens })
            };
        }
        // 左定界符匹配到了，但扫描结束或提前中断时仍未找到可闭合的 run（包括正则执行错误）。
        EmOutcome::None
    }

    /// marked 的基础 `emStrong`（扩展返回 `false` 时回退）。
    ///
    /// 与扩展版的差别：没有 CJK 邻接放行，右定界符固定用基础（非 CJK）正则。当前移植
    /// 里它与扩展共用 `em_strong_l_delim` 并做同样的下划线词内检查，所以扩展报告
    /// `Fallback` 的两种情形在这里同样返回 `None`；保留它是为了与 TS 扩展的 `false`
    /// 回退协议一一对应。
    fn em_strong_base(
        &mut self,
        src: &str,
        masked_src: &str,
        prev_char: Option<u16>,
    ) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.em_strong_l_delim.captures(src).ok().flatten()?;
        if !group_present(&cap, &[1, 2, 3, 4]) {
            return None;
        }
        // `_` 不能夹在两个字母数字之间
        if group_present(&cap, &[4]) && unit_matches(&rules.unicode_alpha_numeric, prev_char) {
            return None;
        }

        let next_char = first_group(&cap, &[1, 3]);
        // 与扩展版相比少了 CJK 放行：`next_char` 为 `None` 时直接允许；为 `Some` 时要求
        // 前一个字符不存在或是标点。
        let enters = match next_char {
            None => true,
            Some(_) => prev_char.is_none() || unit_matches(&rules.punctuation, prev_char),
        };
        if !enters {
            return None;
        }

        let m0 = cap.get(0)?.as_str().to_string();
        let l_length = char_count(&m0) - 1;
        let mut delim_total = l_length as i64;
        let mut mid_delim_total = 0i64;
        let first_unit = m0.as_bytes().first().copied().unwrap_or(b'*') as u16;
        let mid_run = prev_char == Some(first_unit);
        // `*` 用 ast 版规则，`_` 用 und 版（marked 固定如此，与 GFM 选项无关）。
        let end_reg = if m0.starts_with('*') {
            &rules.em_strong_r_delim_ast
        } else {
            &rules.em_strong_r_delim_und
        };

        // 与扩展版相同的尾部对齐：把掩码串裁到「src 去掉左定界符」的位置。
        let src_units = utf16_len(src);
        let clipped_masked_src = slice_from_utf16(masked_src, src_units.saturating_sub(l_length));
        let mut position = 0usize;
        let mut r_match = end_reg
            .captures_from_pos(clipped_masked_src, position)
            .ok()
            .flatten();
        while let Some(caps) = r_match {
            let full = match caps.get(0) {
                Some(matched) => matched,
                None => break,
            };
            let match_start = full.start();
            let match_end = full.end();
            let advance = |position: &mut usize| {
                *position = match_end.max(match_start + 1);
            };

            let Some(r_delim) = first_group(&caps, &[1, 2, 3, 4, 5, 6]) else {
                advance(&mut position);
                r_match = end_reg
                    .captures_from_pos(clipped_masked_src, position)
                    .ok()
                    .flatten();
                continue;
            };
            let r_length = char_count(r_delim);

            if group_present(&caps, &[3, 4]) {
                // 另一个左定界符
                delim_total += r_length as i64;
                advance(&mut position);
                r_match = end_reg
                    .captures_from_pos(clipped_masked_src, position)
                    .ok()
                    .flatten();
                continue;
            } else if group_present(&caps, &[5, 6]) {
                if !l_length.is_multiple_of(3) && (l_length + r_length).is_multiple_of(3) {
                    mid_delim_total += r_length as i64;
                    advance(&mut position);
                    r_match = end_reg
                        .captures_from_pos(clipped_masked_src, position)
                        .ok()
                        .flatten();
                    continue;
                }
                if mid_run {
                    break;
                }
            }

            delim_total -= r_length as i64;
            if delim_total > 0 {
                advance(&mut position);
                r_match = end_reg
                    .captures_from_pos(clipped_masked_src, position)
                    .ok()
                    .flatten();
                continue;
            }

            let trimmed = (r_length as i64)
                .min(r_length as i64 + delim_total + mid_delim_total)
                .max(0) as usize;
            let last_char_length = full
                .as_str()
                .chars()
                .next()
                .map(char::len_utf16)
                .unwrap_or(1);
            let raw_units = l_length
                + utf16_len(&clipped_masked_src[..match_start])
                + last_char_length
                + trimmed;
            let raw = slice_utf16(src, raw_units).to_string();

            return if (l_length.min(trimmed)) % 2 == 1 {
                let text = slice_units(&raw, 1, -1).to_string();
                let tokens = self.inline_tokens(&text);
                Some(Token::Em { raw, text, tokens })
            } else {
                let text = slice_units(&raw, 2, -2).to_string();
                let tokens = self.inline_tokens(&text);
                Some(Token::Strong { raw, text, tokens })
            };
        }
        None
    }

    /// `isPrevCharCjk(prevChar, maskedSrc, src)`。
    ///
    /// `prev_char` 是「当前 `src` 前一个字符」的最后一个 UTF-16 码元。要判断整个字符
    /// 是否 CJK，需要回到 `masked_src` 里取原始码元：
    ///
    /// 1. 先看 `punctuation_cjk` 字符类——它包含空白、标点、符号与 CJK，命中即返回 true；
    /// 2. 否则用 `masked_src` 的码元数减 `src` 的码元数得到当前源码的起始偏移，取前一个
    ///    码元；
    /// 3. 若该码元是变体选择符（U+FE00–U+FE0E），再往前一个码元取基字符；
    /// 4. 若是代理对，合并成码点再判断（U+E0100–U+E01EF 的补充变体选择符直接算 CJK）；
    /// 5. 否则按 `cjk_test` 字符类判断。
    fn is_prev_char_cjk(&self, prev_char: Option<u16>, masked_src: &str, src: &str) -> bool {
        let rules = self.rules;
        let mut prev_is_cjk = unit_matches(&rules.punctuation_cjk, prev_char);
        if !prev_is_cjk && prev_char.is_some() {
            // 当前源码在完整（掩码）串中的起始码元下标；往前一格就是上一个字符的末码元。
            let masked_units = utf16_len(masked_src);
            let src_units = utf16_len(src);
            let prev_idx = masked_units.saturating_sub(src_units);
            if prev_idx >= 1 {
                let units: Vec<u16> = masked_src.encode_utf16().collect();
                let mut idx = prev_idx - 1;
                let mut code = units[idx];
                // 变体选择符（U+FE00–U+FE0E）只是修饰前一个字符，跳过它看基字符。
                if (65024..=65038).contains(&code) && idx >= 1 {
                    idx -= 1;
                    code = units[idx];
                }
                // 代理对：低代理 0xDC00–0xDFFF 需要与前一个高代理（0xD800–0xDBFF）合并。
                if (code & 64512) == 56320 && idx >= 1 {
                    let high = units[idx - 1];
                    if (high & 64512) == 55296 {
                        let cp =
                            0x10000 + (((high as u32) - 0xD800) << 10) + ((code as u32) - 0xDC00);
                        if cp > 65535 {
                            if (917760..=917999).contains(&cp) {
                                prev_is_cjk = true;
                            } else if let Some(ch) = char::from_u32(cp) {
                                prev_is_cjk = text_matches(&rules.cjk_test, &ch.to_string());
                            }
                        }
                    }
                } else if let Some(ch) = char_from_unit(code) {
                    prev_is_cjk = text_matches(&rules.cjk_test, &ch.to_string());
                }
            }
        }
        prev_is_cjk
    }

    // -----------------------------------------------------------------------
    // codespan
    // -----------------------------------------------------------------------
    /// 行内代码 `` `code` ``（`rules.inline_code`）：反引号数量必须配对（正则用 `\1`
    /// 反向引用），内容里的换行折叠成空格。
    ///
    /// 按 CommonMark，内容首尾都是 ASCII 空格、且内容里含非空格字符时，去掉首尾各一个
    /// 空格；全为空格的内容保留原样（`non_space_char` 是 `[^ ]`，只排除 ASCII 空格）。
    fn codespan(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.inline_code.captures(src).ok().flatten()?;
        let mut text = rules
            .new_line_char_global
            .replace_all(cap.get(2)?.as_str(), " ")
            .into_owned();
        let has_non_space_chars = text_matches(&rules.non_space_char, &text);
        let has_space_chars_on_both_ends = text_matches(&rules.starting_space_char, &text)
            && text_matches(&rules.ending_space_char, &text);
        if has_non_space_chars && has_space_chars_on_both_ends {
            text = slice_units(&text, 1, -1).to_string();
        }
        Some(Token::Codespan {
            raw: cap.get(0)?.as_str().to_string(),
            text,
        })
    }

    // -----------------------------------------------------------------------
    // br
    // -----------------------------------------------------------------------
    /// 硬换行：行尾两个以上空格、或反斜杠后跟换行（`rules.br`）；换行之后必须还有非空白
    /// 内容（正则的 `(?!\s*$)`），否则不成立。渲染侧把它输出成单个 `\n`。
    fn inline_br(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.br.captures(src).ok().flatten()?;
        Some(Token::Br {
            raw: cap.get(0)?.as_str().to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // del（严格删除线：`~~x~~`）
    // -----------------------------------------------------------------------
    /// 严格删除线：只接受 `~~text~~`（`rules.strict_del`）。
    ///
    /// 额外约束：开头的 `~~` 后不能紧跟空白或 `~`，闭合 `~~` 前的最后一个元素不能是
    /// 未转义的空白、`~` 或反斜杠（`\ `、`\~` 这类转义对除外），闭合后不能紧跟 `~`。
    /// 规则要求成对 `~~`，因此单个 `~` 是普通文本（对应测试
    /// `keep_single_tilde_as_plain_text`）。内部文本递归解析成子 token，渲染时整体套
    /// 删除线样式。
    fn del(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.strict_del.captures(src).ok().flatten()?;
        let text = cap.get(2)?.as_str().to_string();
        let tokens = self.inline_tokens(&text);
        Some(Token::Del {
            raw: cap.get(0)?.as_str().to_string(),
            text,
            tokens,
        })
    }

    // -----------------------------------------------------------------------
    // autolink
    // -----------------------------------------------------------------------
    /// 尖括号自动链接 `<scheme:...>` / `<user@host>`（`rules.autolink`）。
    ///
    /// `text` 是尖括号内的原文；邮箱分支的 href 加 `mailto:` 前缀。子 token 只有一个
    /// `Text`。与 `url` 不同，这里不检查 `in_link`。
    fn autolink(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.autolink.captures(src).ok().flatten()?;
        let text = cap.get(1)?.as_str().to_string();
        let is_email = cap.get(2).is_some_and(|matched| matched.as_str() == "@");
        let href = if is_email {
            format!("mailto:{text}")
        } else {
            text.clone()
        };
        Some(Token::Link {
            raw: cap.get(0)?.as_str().to_string(),
            href,
            title: None,
            text: text.clone(),
            tokens: vec![Token::Text {
                raw: text.clone(),
                text,
                inline_id: None,
                tokens: Vec::new(),
                escaped: false,
            }],
            image: false,
        })
    }

    // -----------------------------------------------------------------------
    // url（gfm）
    // -----------------------------------------------------------------------
    /// GFM 裸 URL / 裸邮箱（`rules.url`）。
    ///
    /// 非邮箱分支会对匹配文本反复应用 `backpedal`，削掉结尾的标点、引号、不配对的括号
    /// 等，直到文本不再变化；以 `www.` 开头的补 `http://` 前缀。邮箱分支生成 `mailto:`
    /// 链接。主循环只在 `!in_link` 时调用本函数，避免把链接文字里的裸 URL 再包一层链接。
    fn url(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.url.captures(src).ok().flatten()?;
        let mut cap0 = cap.get(0)?.as_str().to_string();
        // 裸邮箱分支（`^email`）里 group 1 不参与匹配；TS 只有非邮箱分支才读 `cap[1]`
        // （`undefined === 'www.'` 为假），所以这里保持可选。
        let cap1 = cap.get(1).map(|matched| matched.as_str().to_string());
        let is_email = cap.get(2).is_some_and(|matched| matched.as_str() == "@");

        let (text, href) = if is_email {
            (cap0.clone(), format!("mailto:{cap0}"))
        } else {
            // 扩展自动链接的回退校验：反复取 backpedal 的匹配结果（最长合法前缀），
            // 直到不再变化；找不到匹配时结果为空串，下一轮比较后退出。
            loop {
                let previous = cap0.clone();
                cap0 = rules
                    .backpedal
                    .find(&cap0)
                    .ok()
                    .flatten()
                    .map(|matched| matched.as_str().to_string())
                    .unwrap_or_default();
                if previous == cap0 {
                    break;
                }
            }
            let text = cap0.clone();
            let href = if cap1.as_deref() == Some("www.") {
                format!("http://{cap0}")
            } else {
                cap0.clone()
            };
            (text, href)
        };

        Some(Token::Link {
            raw: cap0,
            href,
            title: None,
            text: text.clone(),
            tokens: vec![Token::Text {
                raw: text.clone(),
                text,
                inline_id: None,
                tokens: Vec::new(),
                escaped: false,
            }],
            image: false,
        })
    }

    // -----------------------------------------------------------------------
    // inlineText
    // -----------------------------------------------------------------------
    /// 兜底文本 token：`rules.inline_text` 匹配从当前位置开始的一段普通文本。
    ///
    /// `escaped` 复制当前的 `in_raw_block`；相邻 `Text` token 的合并由主循环负责，本函数
    /// 每次只产出一段。
    fn inline_text(&mut self, src: &str) -> Option<Token> {
        let rules = self.rules;
        let cap = rules.inline_text.captures(src).ok().flatten()?;
        let raw = cap.get(0)?.as_str().to_string();
        let escaped = self.state.in_raw_block;
        Some(Token::Text {
            raw: raw.clone(),
            text: raw,
            inline_id: None,
            tokens: Vec::new(),
            escaped,
        })
    }
}

/// `isCjkAdjacentToDelimiter(rightMatch, clippedMaskedSrc)`。
///
/// 判定右定界符 run 是否紧邻 CJK：右定界符正则里每个带捕获组的分支都会吞掉定界符 run
/// 前面的一个字符，所以 `full` 的首字符就是「前邻字符」；「后邻字符」取 `full` 结束位置
/// 之后的第一个字符。任一者是 CJK（`rules.cjk_test`）即返回 true。
///
/// 只有捕获组 1–4 参与判断；匹配来自第 5/6 组时直接返回 false。
fn is_cjk_adjacent_to_delimiter(
    caps: &Captures,
    clipped_masked_src: &str,
    rules: &super::lexer::Rules,
) -> bool {
    if !group_present(caps, &[1, 2, 3, 4]) {
        return false;
    }
    let Some(full) = caps.get(0) else {
        return false;
    };
    let char_before = match full.as_str().chars().next() {
        Some(ch) => ch,
        None => return false,
    };
    // 后邻字符：把 `full` 结束处的 UTF-16 码元下标换算成字节下标再取字符。
    let after_units = utf16_len(&clipped_masked_src[..full.end()]);
    let char_after = if after_units < utf16_len(clipped_masked_src) {
        let byte_index = byte_at_utf16(clipped_masked_src, after_units);
        clipped_masked_src[byte_index..].chars().next()
    } else {
        None
    };
    text_matches(&rules.cjk_test, &char_before.to_string())
        || char_after.is_some_and(|ch| text_matches(&rules.cjk_test, &ch.to_string()))
}
