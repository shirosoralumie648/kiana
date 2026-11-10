//! ICU 78.3 词典分词引擎的 Rust 移植（`Intl.Segmenter("word")` 的词典那一半）。
//!
//! ## 数据
//!
//! `src/dicts/*.bin` 是从 Node 26.12.1 内置的 ICU 78.3 数据包（`icudt78l.dat`，
//! 编译进 node 二进制的 `icudt78_dat` 符号）里取出的 `brkitr/<name>.dict` 项中的
//! **序列化 trie** 部分（去掉 ICU 数据包头与 8 个索引 int32）：
//!
//! | 文件 | 来源项 | trie 类型 | 变换 | 字节数 | sha256 |
//! |------|--------|-----------|------|--------|--------|
//! | `cjdict.bin` | `icudt78l/brkitr/cjdict.dict` | UCharsTrie + values | — | 2,007,110 | `937aa803…6b76d` |
//! | `thaidict.bin` | `icudt78l/brkitr/thaidict.dict` | BytesTrie | `0xE00` | 125,960 | `d2f46fa4…bcc598` |
//! | `laodict.bin` | `icudt78l/brkitr/laodict.dict` | BytesTrie | `0xE80` | 162,447 | `58113bfa…340278` |
//! | `khmerdict.bin` | `icudt78l/brkitr/khmerdict.dict` | BytesTrie | `0x1780` | 445,366 | `3962de16…0baa07` |
//! | `burmesedict.bin` | `icudt78l/brkitr/burmesedict.dict` | BytesTrie | `0x1000` | 254,262 | `26ae4160…eb731d` |
//!
//! `.dict` 的格式见 ICU `dictionarydata.h`：数据头后是 `int32_t indexes[8]`，
//! `indexes[IX_STRING_TRIE_OFFSET]`..`indexes[IX_TOTAL_SIZE]` 之间就是 trie 字节。
//! 这里只保存 trie 本体；`indexes[IX_TRIE_TYPE]`/`indexes[IX_TRANSFORM]` 的值在
//! [`DictKind`] 里硬编码（它们随数据固定）。
//!
//! 为什么必须自带数据：`Intl.Segmenter` 走 ICU4C 的规则引擎 + 词典引擎，而
//! `icu_segmenter`（ICU4X）的词典数据与 ICU 78.3 **不是同一份**（例如 ICU4X 把
//! `ひらがな` 切成 `ひらが`/`な`，ICU 78.3 是 `ひ`/`ら`/`が`/`な`；泰文 `จอห์น`
//! ICU4X 切三段、ICU 78.3 不切）。逐字节对齐只能使用同一份词典数据。
//!
//! ## 算法
//!
//! 逐函数对应 ICU4C 的 `common/dictbe.cpp`（`UCharsDictionaryMatcher`/
//! `UCharsDictionaryMatcher::matches`、`PossibleWord`、`ThaiBreakEngine`、
//! `LaoBreakEngine`、`BurmeseBreakEngine`、`KhmerBreakEngine`、`CjkBreakEngine`）
//! 与 `common/dictionarydata.*`（trie 遍历、`matches` 的 `limit`/`prefix` 语义）。
//! trie 的序列化格式见 ICU `unicode/ucharstrie.h` / `unicode/bytestrie.h`。
//!
//! 调用方式与 [`crate::text::default_word_segments`] 的规则引擎配合：规则引擎给出
//! 规则段与 `isWordLike` 标签，词典引擎只在**词典字符的连续段内部**给出细分边界
//! （ICU 的 RBBI 也是"规则引擎决定范围、词典引擎给出段内边界"）。
//!
//! 四个 SA 引擎的 `fSet`（泰/老/高棉/缅里 `LineBreak=SA` 的字符）全部是 BMP，
//! 码点长度 == UTF-16 码元长度；CJK 词典段则可能包含补充平面字符（`[:Han:]` 含
//! U+2F800..U+2FA1D 等补充平面汉字，每个占 2 个 UTF-16 码元），因此内部仍按 ICU
//! 的"码点索引 ↔ UTF-16 偏移"映射来算边界，对外返回**段内字符下标**。
//!
//! ## 在本 crate 中的位置
//!
//! 通过 `#[path = "word_dict.rs"] mod word_dict;` 挂在 [`crate::text`] 下（文件在
//! `src/` 而不是 `src/text/`），是**私有**子模块：唯一的生产代码调用方是
//! [`crate::text::default_word_segments`]（本文件的单元测试也直接调用
//! [`segment_run`]）。它先按 ICU 规则引擎切出规则段，再把其中的
//! 词典字符连续段交给本模块，最后把返回的段内下标平移回整串（见该函数里的
//! `piece_starts`）。本模块不关心 `isWordLike` 标签，也不产生输出段。
//!
//! ## 执行流程
//!
//! 1. [`segment_run`] 按 [`DictKind`] 选引擎：CJK → [`cjk_divide`]（NFKC 归一化 →
//!    动态规划求最小代价切分 → 回溯）；泰/老/高棉/缅 → [`sea_divide`]（用 3 个
//!    [`PossibleWord`] 槽位轮转，即当前词 + 最多 2 个后续词）。
//! 2. 两个引擎都通过 `DictionaryMatcher::matches` 在序列化 trie 上前缀匹配，得到
//!    "以当前位置开头的所有词典词"及长度/代价。
//! 3. 返回段内字符下标（升序、不含段尾）；调用方加上 `run_start` 后与规则段边界合并。
//!
//! ## 失败模式与不变量
//!
//! - 词典是 `include_bytes!` 的编译期常量，运行期不读文件、不做 IO；trie 遍历
//!   （`unit`/`byte`）不做边界检查，数据被截断或换成另一种格式会在读取处 panic，
//!   而不是返回 `Result`。因此 `src/dicts/*.bin` 不能手工编辑（sha256 见上表）。
//! - [`segment_run`] **不校验** `chars` 是否真是该引擎的 `fSet` 连续段：混入其他字符
//!   时结果无定义（`transform` 可能提前返回 `None`）。这是调用方的契约。
//! - 非空输入返回的下标都在 `1..chars.len()` 内且严格递增；空输入返回空 `Vec`。
//!   泰文段 `<= 4` 个码点、其他 SA 段 `< 4` 个码点时直接返回空（见 [`sea_divide`]）。
//! - 所有可写状态都是调用内的局部变量（`Vec` 与游标），没有跨调用缓存或全局可变
//!   状态；词典字节只以 `&'static [u8]` 读取。
//!
//! ## 阅读顺序
//!
//! 先读 [`DictKind`] 与 `DictionaryMatcher::matches`（数据与匹配语义），再读
//! [`PossibleWord`] + [`sea_divide`]；CJK 从 [`normalize_run`] 读到 [`cjk_divide`]。
//! 两个 trie 遍历器（[`UCharsTrie`]/[`BytesTrie`]）只是 ICU 序列化格式的机械翻译，
//! 可以最后看，不影响理解分词策略。

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::canonical_combining_class;

// ============================================================================
// 词典数据
// ============================================================================

// 五份词典在编译期嵌入（`include_bytes!`）：没有运行期路径、没有 IO，也无法用外部
// 文件替换——更新词典必须重编。每个文件的来源/大小/哈希见模块文档的表格。
static CJ_DICT: &[u8] = include_bytes!("dicts/cjdict.bin");
static THAI_DICT: &[u8] = include_bytes!("dicts/thaidict.bin");
static LAO_DICT: &[u8] = include_bytes!("dicts/laodict.bin");
static KHMER_DICT: &[u8] = include_bytes!("dicts/khmerdict.bin");
static BURMESE_DICT: &[u8] = include_bytes!("dicts/burmesedict.bin");

/// `DictionaryData::TRIE_TYPE_UCHARS` / `TRIE_TYPE_BYTES` / `TRIE_HAS_VALUES`。
const TRIE_TYPE_UCHARS: i32 = 1;
const TRIE_TYPE_BYTES: i32 = 0;
#[allow(dead_code, reason = "对齐 DictionaryData 常量表；值直接编码在 trie 里")]
const TRIE_HAS_VALUES: i32 = 8;

/// `DictionaryData::TRANSFORM_TYPE_OFFSET` / `TRANSFORM_TYPE_MASK` / `TRANSFORM_OFFSET_MASK`。
const TRANSFORM_TYPE_OFFSET: i32 = 0x0100_0000;
const TRANSFORM_TYPE_MASK: i32 = 0x7f00_0000;
const TRANSFORM_OFFSET_MASK: i32 = 0x001f_ffff;

/// 每个脚本的词典引擎（对应 ICU `brkitr/dictionaries` 资源里脚本短名到词典文件的映射）。
///
/// 只有这五个变体：ICU 78.3 的标准数据里没有韩语词典，谚文音节由调用方直接跳过，
/// 不会进入本模块（见 `text.rs` 的 `default_word_segments`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DictKind {
    /// Han/Hiragana/Katakana → `cjdict`（`CjkBreakEngine(kChineseJapanese)`）。
    Cjk,
    /// Thai → `thaidict`。
    Thai,
    /// Lao → `laodict`。
    Lao,
    /// Khmer → `khmerdict`。
    Khmer,
    /// Myanmar → `burmesedict`。
    Burmese,
}

impl DictKind {
    /// 本引擎的序列化 trie 字节（编译期嵌入，来源见模块文档的表格）。
    fn trie_data(self) -> &'static [u8] {
        match self {
            DictKind::Cjk => CJ_DICT,
            DictKind::Thai => THAI_DICT,
            DictKind::Lao => LAO_DICT,
            DictKind::Khmer => KHMER_DICT,
            DictKind::Burmese => BURMESE_DICT,
        }
    }

    /// `IX_TRIE_TYPE`：CJK 用 `UCharsTrie`（UTF-16 单元），四个 SA 引擎用 `BytesTrie`。
    ///
    /// BytesTrie 的输入不是码点本身，而是先经 [`DictionaryMatcher::transform`] 映射到
    /// `0..=0xFF` 的字节（见该函数）。
    fn trie_type(self) -> i32 {
        match self {
            DictKind::Cjk => TRIE_TYPE_UCHARS,
            _ => TRIE_TYPE_BYTES,
        }
    }

    /// `.dict` 索引里的 `IX_TRANSFORM`（`TRANSFORM_TYPE_OFFSET | offset`）。
    ///
    /// 返回 0 表示不做变换（CJK 的 UCharsTrie 直接用码点作为输入单元）；其余四个是
    /// 脚本基址，`DictionaryMatcher::transform` 用 `码点 - offset` 得到 trie 的字节键。
    fn transform(self) -> i32 {
        match self {
            DictKind::Cjk => 0,
            DictKind::Thai => TRANSFORM_TYPE_OFFSET | 0x0E00,
            DictKind::Lao => TRANSFORM_TYPE_OFFSET | 0x0E80,
            DictKind::Khmer => TRANSFORM_TYPE_OFFSET | 0x1780,
            DictKind::Burmese => TRANSFORM_TYPE_OFFSET | 0x1000,
        }
    }
}

// ============================================================================
// UStringTrie 结果
// ============================================================================

/// `UStringTrieResult`（ICU `unicode/ustringtrie.h`）。
///
/// - `NoMatch`：当前输入单元不存在于节点里，游标已经停止；
/// - `NoValue`：路径存在，但该位置不是词尾（只是更长词的前缀）；
/// - `IntermediateValue`：是词尾，且同一游标还能继续匹配更长的词；
/// - `FinalValue`：是词尾，且没有更长的匹配，匹配到此结束。
///
/// 匹配循环用 [`TrieResult::has_value`] 判断后两者（`IntermediateValue`/
/// `FinalValue`）是否算“命中一个词”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrieResult {
    NoMatch,
    NoValue,
    FinalValue,
    IntermediateValue,
}

impl TrieResult {
    fn has_value(self) -> bool {
        matches!(self, TrieResult::FinalValue | TrieResult::IntermediateValue)
    }
}

/// `valueResult(node)`：`FINAL_VALUE` 对应 bit15 置位。
fn value_result_uchars(node: i32) -> TrieResult {
    if node & 0x8000 != 0 {
        TrieResult::FinalValue
    } else {
        TrieResult::IntermediateValue
    }
}

// ============================================================================
// UCharsTrie（ICU `common/ucharstrie.cpp` 的只读遍历）
// ============================================================================

/// `UCharsTrie` 节点常量（ICU `unicode/ucharstrie.h`）。
///
/// 序列化格式的高层结构（细节以 ICU `ucharstrie.h`/`ucharstrie.cpp` 为准）：首单元
/// 是根节点；lead unit 小于 `U_MIN_LINEAR_MATCH` 是分支节点（后跟排序的分支项，
/// 项多时用 delta 跳转），`U_MIN_LINEAR_MATCH..U_MIN_VALUE_LEAD` 是线性匹配链，
/// `>= U_MIN_VALUE_LEAD` 是带值节点。带值节点的 bit15 是 final 标志，其余位与后续
/// 单元一起编码 value（见 `read_value`/`read_node_value`）。
const U_MIN_LINEAR_MATCH: i32 = 0x30;
const U_MAX_LINEAR_MATCH_LENGTH: i32 = 0x10;
const U_MIN_VALUE_LEAD: i32 = U_MIN_LINEAR_MATCH + U_MAX_LINEAR_MATCH_LENGTH; // 0x0040
const U_NODE_TYPE_MASK: i32 = U_MIN_VALUE_LEAD - 1; // 0x003f
const U_VALUE_IS_FINAL: i32 = 0x8000;
const U_MIN_TWO_UNIT_VALUE_LEAD: i32 = 0x4000;
const U_THREE_UNIT_VALUE_LEAD: i32 = 0x7fff;
const U_MIN_TWO_UNIT_NODE_VALUE_LEAD: i32 = 0x4040;
const U_THREE_UNIT_NODE_VALUE_LEAD: i32 = 0x7fc0;
const U_MIN_TWO_UNIT_DELTA_LEAD: i32 = 0xfc00;
const U_THREE_UNIT_DELTA_LEAD: i32 = 0xffff;
const MAX_BRANCH_LINEAR_SUB_NODE_LENGTH: i32 = 5;

/// 序列化 `UCharsTrie` 的只读游标（单位是 char16_t，数据以小端 2 字节存储）。
///
/// `pos` 是游标：通常指向“下一个待读单元”，命中带值节点时指向该节点的 lead
/// （由 `get_value` 读取）；`rem` 是线性匹配链剩余长度减 1：`0` 表示还剩 1 个
/// 单元，负值表示不在链上（或链已匹配完）。
/// 停止状态用 `pos == usize::MAX` 表示（对应 ICU 的 `pos_ = nullptr`）。游标不持有
/// 输入文本：码点由调用方逐个喂给 `first`/`next`。
struct UCharsTrie<'a> {
    data: &'a [u8],
    pos: usize,
    rem: i32,
}

impl<'a> UCharsTrie<'a> {
    fn new(data: &'a [u8]) -> Self {
        UCharsTrie {
            data,
            pos: 0,
            rem: -1,
        }
    }

    /// 读下标为 `index` 的 char16_t 单元（小端）。不做边界检查：越界会 panic，
    /// 正确性依赖 `include_bytes!` 数据的完整性（见模块文档的失败模式）。
    fn unit(&self, index: usize) -> i32 {
        let offset = index * 2;
        u16::from_le_bytes([self.data[offset], self.data[offset + 1]]) as i32
    }

    /// `skipValue(pos, leadUnit)`：`pos` 已越过 lead unit。
    fn skip_value(&self, pos: usize, lead: i32) -> usize {
        if lead >= U_MIN_TWO_UNIT_VALUE_LEAD {
            if lead < U_THREE_UNIT_VALUE_LEAD {
                pos + 1
            } else {
                pos + 2
            }
        } else {
            pos
        }
    }

    fn read_value(&self, pos: usize, lead: i32) -> (i32, usize) {
        if lead < U_MIN_TWO_UNIT_VALUE_LEAD {
            (lead, pos)
        } else if lead < U_THREE_UNIT_VALUE_LEAD {
            (
                ((lead - U_MIN_TWO_UNIT_VALUE_LEAD) << 16) | self.unit(pos),
                pos + 1,
            )
        } else {
            ((self.unit(pos) << 16) | self.unit(pos + 1), pos + 2)
        }
    }

    fn skip_node_value(&self, pos: usize, lead: i32) -> usize {
        if lead >= U_MIN_TWO_UNIT_NODE_VALUE_LEAD {
            if lead < U_THREE_UNIT_NODE_VALUE_LEAD {
                pos + 1
            } else {
                pos + 2
            }
        } else {
            pos
        }
    }

    fn read_node_value(&self, pos: usize, lead: i32) -> (i32, usize) {
        if lead < U_MIN_TWO_UNIT_NODE_VALUE_LEAD {
            ((lead >> 6) - 1, pos)
        } else if lead < U_THREE_UNIT_NODE_VALUE_LEAD {
            (
                (((lead & 0x7fc0) - U_MIN_TWO_UNIT_NODE_VALUE_LEAD) << 10) | self.unit(pos),
                pos + 1,
            )
        } else {
            ((self.unit(pos) << 16) | self.unit(pos + 1), pos + 2)
        }
    }

    fn jump_by_delta(&self, pos: usize) -> usize {
        let mut pos = pos;
        let mut delta = self.unit(pos);
        pos += 1;
        if delta >= U_MIN_TWO_UNIT_DELTA_LEAD {
            if delta == U_THREE_UNIT_DELTA_LEAD {
                delta = (self.unit(pos) << 16) | self.unit(pos + 1);
                pos += 2;
            } else {
                delta = ((delta - U_MIN_TWO_UNIT_DELTA_LEAD) << 16) | self.unit(pos);
                pos += 1;
            }
        }
        pos + delta as usize
    }

    fn skip_delta(&self, pos: usize) -> usize {
        let mut pos = pos;
        let delta = self.unit(pos);
        pos += 1;
        if delta >= U_MIN_TWO_UNIT_DELTA_LEAD {
            if delta == U_THREE_UNIT_DELTA_LEAD {
                pos += 2;
            } else {
                pos += 1;
            }
        }
        pos
    }

    /// `branchNext(pos, length, uchar)`。
    ///
    /// 在分支节点里找等于 `uchar` 的项。`length` 是“分支项数 - 1”（`0` 表示这个
    /// 减一后的值存在 `pos` 处的单元里）；项数大于
    /// `MAX_BRANCH_LINEAR_SUB_NODE_LENGTH` 时先用 `jump_by_delta`/`skip_delta`
    /// 折半缩小范围，再线性比较。
    ///
    /// 命中且所在节点是 final value 时，只把游标留在该节点并返回 `FinalValue`
    /// （值由 `get_value` 再读）；其余命中返回 `NoValue`/`IntermediateValue`；
    /// 未命中调用 `stop()` 并返回 `NoMatch`。
    fn branch_next(&mut self, mut pos: usize, mut length: i32, uchar: i32) -> TrieResult {
        if length == 0 {
            length = self.unit(pos);
            pos += 1;
        }
        length += 1;
        // 大分支：折半跳过不含目标的那一半，直到候选不超过 5 项。
        while length > MAX_BRANCH_LINEAR_SUB_NODE_LENGTH {
            if uchar < self.unit(pos) {
                length >>= 1;
                pos = self.jump_by_delta(pos + 1);
            } else {
                length -= length >> 1;
                pos = self.skip_delta(pos + 1);
            }
        }
        // 小分支：线性比较前 `length-1` 项；最后一项在循环外单独处理
        // （因此 break 条件是 `length <= 1`）。
        loop {
            if uchar == self.unit(pos) {
                pos += 1;
                let node = self.unit(pos);
                if node & U_VALUE_IS_FINAL != 0 {
                    // 把 final value 留给 getValue() 读。
                    self.pos = pos;
                    return TrieResult::FinalValue;
                }
                pos += 1;
                let (delta, next) = self.read_value(pos, node);
                pos = next + delta as usize;
                let node = self.unit(pos);
                self.pos = pos;
                return if node >= U_MIN_VALUE_LEAD {
                    value_result_uchars(node)
                } else {
                    TrieResult::NoValue
                };
            }
            pos += 1; // 跳过比较 unit
            let lead = self.unit(pos);
            pos = self.skip_value(pos + 1, lead & 0x7fff);
            length -= 1;
            if length <= 1 {
                break;
            }
        }
        if uchar == self.unit(pos) {
            pos += 1;
            self.pos = pos;
            let node = self.unit(pos);
            if node >= U_MIN_VALUE_LEAD {
                value_result_uchars(node)
            } else {
                TrieResult::NoValue
            }
        } else {
            self.stop();
            TrieResult::NoMatch
        }
    }

    /// `nextImpl(pos, uchar)`：从节点 `pos` 出发再消费一个输入单元。
    ///
    /// 节点可能是分支、线性匹配链或带值节点；对带值节点，`uchar` 不匹配意味着
    /// “没有更长的词”，于是返回 `NoMatch` 并停止，而不是数据错误。
    fn next_impl(&mut self, mut pos: usize, uchar: i32) -> TrieResult {
        let mut node = self.unit(pos);
        pos += 1;
        loop {
            if node < U_MIN_LINEAR_MATCH {
                return self.branch_next(pos, node, uchar);
            } else if node < U_MIN_VALUE_LEAD {
                // 匹配 length+1 个 unit 中的第一个。
                let mut length = node - U_MIN_LINEAR_MATCH;
                if uchar == self.unit(pos) {
                    pos += 1;
                    length -= 1;
                    self.rem = length;
                    self.pos = pos;
                    if length < 0 && self.unit(pos) >= U_MIN_VALUE_LEAD {
                        return value_result_uchars(self.unit(pos));
                    }
                    return TrieResult::NoValue;
                }
                break;
            } else if node & U_VALUE_IS_FINAL != 0 {
                break;
            } else {
                // 跳过中间值。
                pos = self.skip_node_value(pos, node);
                node &= U_NODE_TYPE_MASK;
            }
        }
        self.stop();
        TrieResult::NoMatch
    }

    /// 置为停止状态；之后的 `next` 立即返回 `NoMatch`（`first` 会重新从根开始）。
    fn stop(&mut self) {
        self.pos = usize::MAX;
        self.rem = -1;
    }

    fn stopped(&self) -> bool {
        self.pos == usize::MAX
    }

    /// `first(uchar)`：重置到根节点并消费第一个输入单元。
    ///
    /// 返回 `FinalValue`/`IntermediateValue` 表示已有命中，此时才能调用 `get_value`；
    /// 返回 `NoMatch` 后游标已停止，除非重新 `first`，后续 `next` 不会命中。
    fn first(&mut self, uchar: i32) -> TrieResult {
        self.rem = -1;
        self.next_impl(0, uchar)
    }

    /// `next(uchar)`：在当前命中路径上再消费一个输入单元。
    ///
    /// 游标已停止时直接返回 `NoMatch`；返回值语义与 `first` 相同。
    fn next(&mut self, uchar: i32) -> TrieResult {
        if self.stopped() {
            return TrieResult::NoMatch;
        }
        let mut pos = self.pos;
        let mut length = self.rem;
        if length >= 0 {
            // 线性匹配节点剩余部分。
            if uchar == self.unit(pos) {
                pos += 1;
                length -= 1;
                self.rem = length;
                self.pos = pos;
                if length < 0 && self.unit(pos) >= U_MIN_VALUE_LEAD {
                    return value_result_uchars(self.unit(pos));
                }
                return TrieResult::NoValue;
            }
            self.stop();
            return TrieResult::NoMatch;
        }
        self.next_impl(pos, uchar)
    }

    /// `getValue()`：读取最近一次命中的 value。
    ///
    /// 前置条件：上一次 `first`/`next` 返回带值结果，且此后没有再推进游标；否则读到
    /// 的内容没有意义。`FinalValue` 的 value 编码在节点 lead 与后续单元中，
    /// `IntermediateValue` 则由 `read_node_value` 解出。在停止状态
    /// （`pos == usize::MAX`）调用会 panic（被当作下标）。
    fn get_value(&self) -> i32 {
        let mut pos = self.pos;
        let lead = self.unit(pos);
        pos += 1;
        if lead & U_VALUE_IS_FINAL != 0 {
            self.read_value(pos, lead & 0x7fff).0
        } else {
            self.read_node_value(pos, lead).0
        }
    }
}

// ============================================================================
// BytesTrie（ICU `common/bytestrie.cpp` 的只读遍历）
// ============================================================================

// BytesTrie 是 UCharsTrie 的字节版：同样是分支/线性匹配/带值节点，但编码单位是 u8，
// 分支长度与 value 的变长编码不同（常量见下）。SA 词典的 trie 是字节型，输入单元
// 来自 `DictionaryMatcher::transform` 的 `0..=0xFF` 映射。
const B_MIN_LINEAR_MATCH: i32 = 0x10;
const B_MAX_LINEAR_MATCH_LENGTH: i32 = 0x10;
const B_MIN_VALUE_LEAD: i32 = B_MIN_LINEAR_MATCH + B_MAX_LINEAR_MATCH_LENGTH; // 0x20
const B_VALUE_IS_FINAL: i32 = 1;
const B_MIN_ONE_BYTE_VALUE_LEAD: i32 = B_MIN_VALUE_LEAD / 2; // 0x10
const B_MAX_ONE_BYTE_VALUE: i32 = 0x40;
const B_MIN_TWO_BYTE_VALUE_LEAD: i32 = B_MIN_ONE_BYTE_VALUE_LEAD + B_MAX_ONE_BYTE_VALUE + 1; // 0x51
const B_MIN_THREE_BYTE_VALUE_LEAD: i32 = 0x6c;
const B_FOUR_BYTE_VALUE_LEAD: i32 = 0x7e;
const B_MIN_TWO_BYTE_DELTA_LEAD: i32 = 0xc0;
const B_MIN_THREE_BYTE_DELTA_LEAD: i32 = 0xf0;
const B_FOUR_BYTE_DELTA_LEAD: i32 = 0xfe;

/// 序列化 `BytesTrie` 的只读游标。
///
/// 字段语义与 [`UCharsTrie`] 相同（`pos`/`rem`、`usize::MAX` 表示停止；`rem` 同样
/// 是线性匹配链剩余长度减 1），只是数据单位是字节而不是 char16_t。
struct BytesTrie<'a> {
    data: &'a [u8],
    pos: usize,
    rem: i32,
}

impl<'a> BytesTrie<'a> {
    fn new(data: &'a [u8]) -> Self {
        BytesTrie {
            data,
            pos: 0,
            rem: -1,
        }
    }

    /// 读下标为 `index` 的字节。不做边界检查：越界会 panic，正确性依赖
    /// `include_bytes!` 数据的完整性（见模块文档的失败模式）。
    fn byte(&self, index: usize) -> i32 {
        self.data[index] as i32
    }

    /// `readValue(pos, leadByte)`：`leadByte` 已右移 1 位（bit0 是 final 标志）。
    fn read_value(&self, pos: usize, lead: i32) -> (i32, usize) {
        if lead < B_MIN_TWO_BYTE_VALUE_LEAD {
            (lead - B_MIN_ONE_BYTE_VALUE_LEAD, pos)
        } else if lead < B_MIN_THREE_BYTE_VALUE_LEAD {
            (
                ((lead - B_MIN_TWO_BYTE_VALUE_LEAD) << 8) | self.byte(pos),
                pos + 1,
            )
        } else if lead < B_FOUR_BYTE_VALUE_LEAD {
            (
                ((lead - B_MIN_THREE_BYTE_VALUE_LEAD) << 16)
                    | (self.byte(pos) << 8)
                    | self.byte(pos + 1),
                pos + 2,
            )
        } else if lead == B_FOUR_BYTE_VALUE_LEAD {
            (
                (self.byte(pos) << 16) | (self.byte(pos + 1) << 8) | self.byte(pos + 2),
                pos + 3,
            )
        } else {
            (
                (self.byte(pos) << 24)
                    | (self.byte(pos + 1) << 16)
                    | (self.byte(pos + 2) << 8)
                    | self.byte(pos + 3),
                pos + 4,
            )
        }
    }

    /// `skipValue(pos, leadByte)`：`leadByte` 是未移位的原始 lead byte。
    fn skip_value(&self, mut pos: usize, lead: i32) -> usize {
        if lead >= (B_MIN_TWO_BYTE_VALUE_LEAD << 1) {
            if lead < (B_MIN_THREE_BYTE_VALUE_LEAD << 1) {
                pos += 1;
            } else if lead < (B_FOUR_BYTE_VALUE_LEAD << 1) {
                pos += 2;
            } else {
                pos += (3 + ((lead >> 1) & 1)) as usize;
            }
        }
        pos
    }

    fn jump_by_delta(&self, mut pos: usize) -> usize {
        let mut delta = self.byte(pos);
        pos += 1;
        if delta < B_MIN_TWO_BYTE_DELTA_LEAD {
            // nothing
        } else if delta < B_MIN_THREE_BYTE_DELTA_LEAD {
            delta = ((delta - B_MIN_TWO_BYTE_DELTA_LEAD) << 8) | self.byte(pos);
            pos += 1;
        } else if delta < B_FOUR_BYTE_DELTA_LEAD {
            delta = ((delta - B_MIN_THREE_BYTE_DELTA_LEAD) << 16)
                | (self.byte(pos) << 8)
                | self.byte(pos + 1);
            pos += 2;
        } else if delta == B_FOUR_BYTE_DELTA_LEAD {
            delta = (self.byte(pos) << 16) | (self.byte(pos + 1) << 8) | self.byte(pos + 2);
            pos += 3;
        } else {
            delta = (self.byte(pos) << 24)
                | (self.byte(pos + 1) << 16)
                | (self.byte(pos + 2) << 8)
                | self.byte(pos + 3);
            pos += 4;
        }
        pos + delta as usize
    }

    fn skip_delta(&self, mut pos: usize) -> usize {
        let delta = self.byte(pos);
        pos += 1;
        if delta >= B_MIN_TWO_BYTE_DELTA_LEAD {
            if delta < B_MIN_THREE_BYTE_DELTA_LEAD {
                pos += 1;
            } else if delta < B_FOUR_BYTE_DELTA_LEAD {
                pos += 2;
            } else {
                pos += (3 + (delta & 1)) as usize;
            }
        }
        pos
    }

    /// `branchNext(pos, length, inByte)`：字节版的分支查找，语义同
    /// [`UCharsTrie::branch_next`]（长度编码、折半跳转、命中/停止规则都对应）。
    fn branch_next(&mut self, mut pos: usize, mut length: i32, in_byte: i32) -> TrieResult {
        if length == 0 {
            length = self.byte(pos);
            pos += 1;
        }
        length += 1;
        // 大分支：折半跳过不含目标的那一半，直到候选不超过 5 项。
        while length > MAX_BRANCH_LINEAR_SUB_NODE_LENGTH {
            if in_byte < self.byte(pos) {
                length >>= 1;
                pos = self.jump_by_delta(pos + 1);
            } else {
                length -= length >> 1;
                pos = self.skip_delta(pos + 1);
            }
        }
        // 小分支：线性比较前 `length-1` 项；最后一项在循环外单独处理。
        loop {
            if in_byte == self.byte(pos) {
                pos += 1;
                let node = self.byte(pos);
                if node & B_VALUE_IS_FINAL != 0 {
                    self.pos = pos;
                    return TrieResult::FinalValue;
                }
                pos += 1;
                let (delta, next) = self.read_value(pos, node >> 1);
                pos = next + delta as usize;
                let node = self.byte(pos);
                self.pos = pos;
                return if node >= B_MIN_VALUE_LEAD {
                    bytes_value_result(node)
                } else {
                    TrieResult::NoValue
                };
            }
            pos += 1;
            let lead = self.byte(pos);
            pos = self.skip_value(pos + 1, lead);
            length -= 1;
            if length <= 1 {
                break;
            }
        }
        if in_byte == self.byte(pos) {
            pos += 1;
            self.pos = pos;
            let node = self.byte(pos);
            if node >= B_MIN_VALUE_LEAD {
                bytes_value_result(node)
            } else {
                TrieResult::NoValue
            }
        } else {
            self.stop();
            TrieResult::NoMatch
        }
    }

    /// `nextImpl(pos, inByte)`：字节版节点分派，语义同 [`UCharsTrie::next_impl`]。
    fn next_impl(&mut self, mut pos: usize, in_byte: i32) -> TrieResult {
        loop {
            let node = self.byte(pos);
            pos += 1;
            if node < B_MIN_LINEAR_MATCH {
                return self.branch_next(pos, node, in_byte);
            } else if node < B_MIN_VALUE_LEAD {
                let mut length = node - B_MIN_LINEAR_MATCH;
                if in_byte == self.byte(pos) {
                    pos += 1;
                    length -= 1;
                    self.rem = length;
                    self.pos = pos;
                    if length < 0 && self.byte(pos) >= B_MIN_VALUE_LEAD {
                        return bytes_value_result(self.byte(pos));
                    }
                    return TrieResult::NoValue;
                }
                break;
            } else if node & B_VALUE_IS_FINAL != 0 {
                break;
            } else {
                // 跳过中间值。
                pos = self.skip_value(pos, node);
            }
        }
        self.stop();
        TrieResult::NoMatch
    }

    /// 置为停止状态；之后的 `next` 立即返回 `NoMatch`（`first` 会重新从根开始）。
    fn stop(&mut self) {
        self.pos = usize::MAX;
        self.rem = -1;
    }

    fn stopped(&self) -> bool {
        self.pos == usize::MAX
    }

    /// `first(inByte)`：重置到根节点并消费第一个字节。
    fn first(&mut self, in_byte: i32) -> TrieResult {
        self.rem = -1;
        self.next_impl(0, in_byte)
    }

    /// `next(inByte)`：在当前命中路径上再消费一个字节。
    ///
    /// `in_byte` 为负时先加 `0x100` 折到 `0..=0xFF`（与 ICU `BytesTrie::next` 一致）；
    /// 游标已停止时直接返回 `NoMatch`。
    fn next(&mut self, in_byte: i32) -> TrieResult {
        if self.stopped() {
            return TrieResult::NoMatch;
        }
        let in_byte = if in_byte < 0 {
            in_byte + 0x100
        } else {
            in_byte
        };
        let mut pos = self.pos;
        let mut length = self.rem;
        if length >= 0 {
            if in_byte == self.byte(pos) {
                pos += 1;
                length -= 1;
                self.rem = length;
                self.pos = pos;
                if length < 0 && self.byte(pos) >= B_MIN_VALUE_LEAD {
                    return bytes_value_result(self.byte(pos));
                }
                return TrieResult::NoValue;
            }
            self.stop();
            return TrieResult::NoMatch;
        }
        self.next_impl(pos, in_byte)
    }

    /// `getValue()`：`readValue(pos, leadByte >> 1)`。
    ///
    /// 前置条件与 [`UCharsTrie::get_value`] 相同：只能在带值结果之后、游标未再推进时
    /// 调用；停止状态下调用会 panic（`usize::MAX` 被当作下标）。
    fn get_value(&self) -> i32 {
        let mut pos = self.pos;
        let lead = self.byte(pos);
        pos += 1;
        self.read_value(pos, lead >> 1).0
    }
}

fn bytes_value_result(node: i32) -> TrieResult {
    if node & B_VALUE_IS_FINAL != 0 {
        TrieResult::FinalValue
    } else {
        TrieResult::IntermediateValue
    }
}

// ============================================================================
// DictionaryMatcher（ICU `dictionarydata.cpp` 的 matches）
// ============================================================================

/// `DictionaryMatcher::matches()` 的输出。
#[derive(Debug, Default)]
struct DictMatches {
    /// 命中的完整词（按长度升序），单位是**码点**（ICU 的 `cpLengths`）。
    cp_lengths: Vec<i32>,
    /// 每个词的值（对应 ICU `values`）。
    values: Vec<i32>,
    /// 最长前缀匹配的码点数（ICU 的 `prefix`，包含失败的那个码点）。
    prefix: i32,
}

/// 词典匹配器（每个脚本一份，数据是 `&'static`）。
///
/// 只保存 [`DictKind`]；trie 字节通过 `trie_data` 在每次匹配时取到（编译期常量），
/// 因此可以 `Copy`、没有生命周期参数，也不持有任何分配。
#[derive(Debug, Clone, Copy)]
struct DictionaryMatcher {
    kind: DictKind,
}

impl DictionaryMatcher {
    fn new(kind: DictKind) -> Self {
        DictionaryMatcher { kind }
    }

    /// `utf16_len(c)`：该码点占几个 UTF-16 码元（BMP 1，增补平面 2）。
    ///
    /// `matches` 用累计码元数与 `max_length` 比较，因此与 ICU 的 `UText` 区间单位
    /// 一致；SA 引擎的 `fSet` 全在 BMP，CJK 段则可能遇到 2 码元的汉字。
    fn units(c: char) -> usize {
        c.len_utf16()
    }

    /// 把码点转成 trie 的输入单元；`None` 表示 ICU 的 `U_SENTINEL`（永不匹配）。
    ///
    /// 只有“字节 trie + `TRANSFORM_TYPE_OFFSET`”的词典（泰/老/高棉/缅）会变换：
    /// `码点 - offset` 落在 `0..=0xFD` 时取该值；ZWNJ (U+200C)/ZWJ (U+200D) 分别
    /// 固定映射为 `0xFE`/`0xFF`；其他码点返回 `None`。CJK 的 UCharsTrie 不做变换，
    /// 直接把码点原样作为单元。
    fn transform(&self, c: char) -> Option<i32> {
        let transform = self.kind.transform();
        if self.kind.trie_type() == TRIE_TYPE_BYTES
            && (transform & TRANSFORM_TYPE_MASK) == TRANSFORM_TYPE_OFFSET
        {
            let cp = c as i32;
            if cp == 0x200D {
                return Some(0xFF);
            } else if cp == 0x200C {
                return Some(0xFE);
            }
            let delta = cp - (transform & TRANSFORM_OFFSET_MASK);
            if !(0..=0xFD).contains(&delta) {
                return None;
            }
            Some(delta)
        } else {
            Some(c as i32)
        }
    }

    /// `DictionaryMatcher::matches(text, maxLength, limit, …, &prefix)`。
    ///
    /// `maxLength` 是 UTF-16 码元数；`limit` 是输出数组容量（最多收集多少个词）。
    /// `cps[0]` 是本次匹配的起点，返回的 `cp_lengths` 是“以它开头”的各词长度。
    ///
    /// 每消费一个码点就向 trie 喂一个单元，并在返回 `IntermediateValue`/
    /// `FinalValue` 时记录长度与值。停止条件（按代码中的检查顺序）：
    ///
    /// 1. `transform` 返回 `None`（码点不在该词典的变换范围）：不匹配立即结束，
    ///    但该码点仍计入 `prefix`（对应 ICU 的 `U_SENTINEL`）；
    /// 2. `FinalValue`：词典里没有更长的词，立即结束；
    /// 3. `NoMatch`：扩展失败，结束；
    /// 4. 已消费的 UTF-16 码元数 `>= max_length`（检查在消费之后，因此包含触发
    ///    上限的那个码点）。
    ///
    /// `prefix` 是**实际消费的**码点数，包含导致 `NoMatch` 的那个码点（ICU 语义），
    /// [`PossibleWord::longest_prefix`] 用它判断“像不像词的开头”。即使 `limit`
    /// 已满，循环仍继续推进，因此 `prefix` 不受 `limit` 影响；超出容量的命中只被
    /// 丢弃（长度和值都不记录）。
    fn matches(&self, cps: &[char], max_length: usize, limit: usize) -> DictMatches {
        let mut out = DictMatches::default();
        let mut uchars = UCharsTrie::new(self.kind.trie_data());
        let mut bytes = BytesTrie::new(self.kind.trie_data());
        let is_uchars = self.kind.trie_type() == TRIE_TYPE_UCHARS;

        let mut code_points_matched = 0usize;
        let mut units_matched = 0usize;
        for &c in cps {
            let Some(input) = self.transform(c) else {
                // `transform` 返回 `None`（ICU 的 `U_SENTINEL`）：不调用 trie，绝不匹配，
                // 但 `prefix` 已计入该码点。
                code_points_matched += 1;
                break;
            };
            // 第一个码点从根节点开始（first），之后的沿当前路径前进（next）。
            let r = if is_uchars {
                if code_points_matched == 0 {
                    uchars.first(input)
                } else {
                    uchars.next(input)
                }
            } else if code_points_matched == 0 {
                bytes.first(input)
            } else {
                bytes.next(input)
            };
            units_matched += Self::units(c);
            code_points_matched += 1;
            // 命中一个词：记录长度与值。`limit` 只限制收集数量，不影响继续匹配，
            // 因此 prefix 始终是完整的已消费码点数。
            if r.has_value() {
                if out.values.len() < limit {
                    out.values.push(if is_uchars {
                        uchars.get_value()
                    } else {
                        bytes.get_value()
                    });
                    out.cp_lengths.push(code_points_matched as i32);
                }
                if r == TrieResult::FinalValue {
                    break;
                }
            } else if r == TrieResult::NoMatch {
                break;
            }
            // 达到调用方给的 UTF-16 长度上限：结束（检查在消费之后，可能已越过上限）。
            if units_matched >= max_length {
                break;
            }
        }
        out.prefix = code_points_matched as i32;
        out
    }
}

// ============================================================================
// PossibleWord（ICU `dictbe.cpp` 里的泰/老/高棉/缅共用辅助类）
// ============================================================================

/// `POSSIBLE_WORD_LIST_MAX`。
const POSSIBLE_WORD_LIST_MAX: usize = 20;

/// 一次向前看得到的“以某位置开头的所有词典词”（ICU `dictbe.cpp` 的同名类）。
///
/// 不变量：`cu_lengths[0..count]` 严格升序（由 `DictionaryMatcher::matches` 的
/// 升序输出保证），`current`/`mark` 是其中的下标；`offset` 是这批候选对应的码点
/// 起点（`-1` 表示尚未计算）。方法会把游标移到 `offset + cu_lengths[下标]`：
/// `candidates`/`back_up` 用 `current`，`accept_marked` 用 `mark`；候选数为 0
/// 时 `candidates` 把游标留在起点。`accept_marked`/`back_up` 要求先调用过
/// `candidates`（`offset >= 0`），否则 `-1 as usize` 会越界 panic。
///
/// 本结构只被 SA 引擎（[`sea_divide`]）使用；CJK 走自己的动态规划，不经过这里，
/// 所以“码点长度 == 码元长度”的前提（SA 引擎的 `fSet` 全在 BMP）成立。
struct PossibleWord {
    /// 候选个数（`matches` 收集到的词数，最多 `POSSIBLE_WORD_LIST_MAX`）。
    count: i32,
    /// 上一次 `candidates` 的 `prefix`（最长前缀码点数，含失败码点）。
    prefix: i32,
    /// 这批候选对应的码点起点。
    offset: i32,
    /// 当前采纳的候选下标（`accept_marked` 使用）。
    mark: i32,
    /// 回溯位置（`back_up` 递减）。
    current: i32,
    /// 各候选的码点长度（升序）。
    cu_lengths: Vec<i32>,
    /// 各候选的码点长度（与 `cu_lengths` 值相同；ICU 有 cu/cp 两份数组）。
    cp_lengths: Vec<i32>,
}

impl PossibleWord {
    fn new() -> Self {
        PossibleWord {
            count: 0,
            prefix: 0,
            offset: -1,
            mark: 0,
            current: 0,
            cu_lengths: vec![0; POSSIBLE_WORD_LIST_MAX],
            cp_lengths: vec![0; POSSIBLE_WORD_LIST_MAX],
        }
    }

    /// `candidates(text, dict, rangeEnd)`；`text[0]` 是当前游标。
    ///
    /// 计算“从 `text.index` 开始、不越过 `rangeEnd` 的所有词典词”，返回候选数。
    /// 结果按起点缓存（`offset`）：同一位置重复调用不会重新匹配 trie，这正合
    /// [`sea_divide`] 反复回退/重试的需要。
    ///
    /// 返回 0 时游标留在原地；否则把游标移到**最长**候选的末尾（升序数组的最后
    /// 一项），并把 `current`/`mark` 都置为该候选，供调用方继续向前看。
    ///
    /// 调用方必须保证 `text.index <= range_end <= chars.len()`（[`sea_divide`]
    /// 维持该不变量）；否则 `text.chars[start..range_end]` 会 panic。
    fn candidates(
        &mut self,
        text: &mut RunText<'_>,
        dict: &DictionaryMatcher,
        range_end: usize,
    ) -> i32 {
        let start = text.index;
        if start as i32 != self.offset {
            self.offset = start as i32;
            let matches = dict.matches(
                &text.chars[start..range_end],
                range_end - start,
                POSSIBLE_WORD_LIST_MAX,
            );
            self.count = matches.cp_lengths.len() as i32;
            for (i, &len) in matches.cp_lengths.iter().enumerate() {
                // 词典段内部全部是 BMP，码点长度 == 码元长度；仍按 ICU 的
                // cuLengths/cpLengths 两份数组保存（见模块文档）。
                self.cu_lengths[i] = len;
                self.cp_lengths[i] = len;
            }
            self.prefix = matches.prefix;
            if self.count <= 0 {
                text.index = start;
            }
        }
        if self.count > 0 {
            text.index = start + self.cu_lengths[(self.count - 1) as usize] as usize;
        }
        self.current = self.count - 1;
        self.mark = self.current;
        self.count
    }

    /// 采纳 `mark` 指向的候选：游标移到该候选末尾，返回其码点长度。
    fn accept_marked(&mut self, text: &mut RunText<'_>) -> i32 {
        text.index = self.offset as usize + self.cu_lengths[self.mark as usize] as usize;
        self.cu_lengths[self.mark as usize]
    }

    /// 退到更短的候选（升序数组的前一项）。已经是最短候选时返回 `false` 且
    /// 不移动游标——调用方据此结束回溯。
    fn back_up(&mut self, text: &mut RunText<'_>) -> bool {
        if self.current > 0 {
            self.current -= 1;
            text.index = self.offset as usize + self.cu_lengths[self.current as usize] as usize;
            true
        } else {
            false
        }
    }

    /// 上一次 `candidates` 的最长前缀码点数（含失败码点）。
    fn longest_prefix(&self) -> i32 {
        self.prefix
    }

    /// 把当前候选记为 `mark`（后续 `accept_marked` 采纳它）。
    fn mark_current(&mut self) {
        self.mark = self.current;
    }

    fn marked_cp_length(&self) -> i32 {
        self.cp_lengths[self.mark as usize]
    }
}

/// 引擎操作的"文本"：一个词段的字符序列 + 游标（对应 ICU 的 `UText`）。
struct RunText<'a> {
    chars: &'a [char],
    index: usize,
}

impl<'a> RunText<'a> {
    fn new(chars: &'a [char]) -> Self {
        RunText { chars, index: 0 }
    }

    /// 读当前码点；越界返回 `None`，不移动游标。
    fn current32(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    /// 前进一个码点；已在末尾时返回 `None` 且不移动游标。
    fn next32(&mut self) -> Option<char> {
        let c = self.chars.get(self.index).copied();
        if c.is_some() {
            self.index += 1;
        }
        c
    }

    /// `utext_moveIndex32(text, delta)`：向前移动 `delta` 个码点。
    ///
    /// `delta` 是无符号数（只用于前进）；越过段尾时钳到 `chars.len()`，因此泰文的
    /// “移动 4 个码点后是否已到段尾”判断在剩余不足 4 个字符时也成立。
    fn move_index32(&mut self, delta: usize) {
        self.index = (self.index + delta).min(self.chars.len());
    }
}

// ============================================================================
// 泰/老/高棉/缅词典引擎（ICU `dictbe.cpp` 的 *BreakEngine::divideUpDictionaryRange）
// ============================================================================

/// 各 SA 引擎的 `*_LOOKAHEAD`（都是 3）。
const SA_LOOKAHEAD: usize = 3;
/// `*_ROOT_COMBINE_THRESHOLD` / `*_PREFIX_COMBINE_THRESHOLD`（都是 3）。
const SA_ROOT_COMBINE_THRESHOLD: i32 = 3;
const SA_PREFIX_COMBINE_THRESHOLD: i32 = 3;
/// `*_MIN_WORD_SPAN`（= `*_MIN_WORD * 2` = 4）。
const SA_MIN_WORD_SPAN: usize = 4;

/// `THAI_PAIYANNOI` / `THAI_MAIYAMOK`。
const THAI_PAIYANNOI: char = '\u{0E2F}';
const THAI_MAIYAMOK: char = '\u{0E46}';

/// 一个 SA 引擎的字符集（ICU 用 `UnicodeSet`，这里用谓词 + 排他区间表达）。
///
/// 段范围（`fSet`）由调用方切好，不放在这里；`mark`/`end_word`/`begin_word` 对应
/// `fMarkSet`/`fEndWordSet`/`fBeginWordSet`，`suffix` 只有泰文用（其余脚本填
/// `never`）。
struct SeaSets {
    mark: fn(char) -> bool,
    end_word: fn(char) -> bool,
    begin_word: fn(char) -> bool,
    suffix: fn(char) -> bool,
}

// 以下谓词一一对应 ICU 各 *BreakEngine 构造函数里的 UnicodeSet 定义：
//   mark       ← fMarkSet：断点不能落在这些字符之前（见 sea_divide 的 mark 循环）；
//   end_word   ← fEndWordSet：可作为词尾；
//   begin_word ← fBeginWordSet：可作为词首；
//   suffix     ← fSuffixSet：泰文的 PAIYANNOI / MAIYAMOK。
// 四个 `*_mark` 都显式把 ASCII 空格算作 mark（各函数里的第一个条件）。
fn thai_mark(c: char) -> bool {
    c == ' '
        || (super::is_script_thai(c as u32)
            && super::is_complex_context(c as u32)
            && super::is_mark(c as u32))
}

fn thai_end_word(c: char) -> bool {
    let cp = c as u32;
    super::is_script_thai(cp)
        && super::is_complex_context(cp)
        && cp != 0x0E31
        && !(0x0E40..=0x0E44).contains(&cp)
}

fn thai_begin_word(c: char) -> bool {
    let cp = c as u32;
    (0x0E01..=0x0E2E).contains(&cp) || (0x0E40..=0x0E44).contains(&cp)
}

fn thai_suffix(c: char) -> bool {
    c == THAI_PAIYANNOI || c == THAI_MAIYAMOK
}

fn lao_mark(c: char) -> bool {
    c == ' '
        || (super::is_script_lao(c as u32)
            && super::is_complex_context(c as u32)
            && super::is_mark(c as u32))
}

fn lao_end_word(c: char) -> bool {
    let cp = c as u32;
    super::is_script_lao(cp) && super::is_complex_context(cp) && !(0x0EC0..=0x0EC4).contains(&cp)
}

fn lao_begin_word(c: char) -> bool {
    let cp = c as u32;
    (0x0E81..=0x0EAE).contains(&cp)
        || (0x0EDC..=0x0EDD).contains(&cp)
        || (0x0EC0..=0x0EC4).contains(&cp)
}

fn khmer_mark(c: char) -> bool {
    c == ' '
        || (super::is_script_khmer(c as u32)
            && super::is_complex_context(c as u32)
            && super::is_mark(c as u32))
}

fn khmer_end_word(c: char) -> bool {
    let cp = c as u32;
    super::is_script_khmer(cp) && super::is_complex_context(cp) && cp != 0x17D2
}

fn khmer_begin_word(c: char) -> bool {
    (0x1780..=0x17B3).contains(&(c as u32))
}

fn burmese_mark(c: char) -> bool {
    c == ' '
        || (super::is_script_myanmar(c as u32)
            && super::is_complex_context(c as u32)
            && super::is_mark(c as u32))
}

fn burmese_end_word(c: char) -> bool {
    let cp = c as u32;
    super::is_script_myanmar(cp) && super::is_complex_context(cp)
}

fn burmese_begin_word(c: char) -> bool {
    (0x1000..=0x102A).contains(&(c as u32))
}

/// 不匹配任何字符：非泰文脚本的 `suffix` 占位。
fn never(_: char) -> bool {
    false
}

/// 按引擎选谓词集合。CJK 没有 SA 引擎，因此 `DictKind::Cjk` 会触发
/// `unreachable!`；[`segment_run`] 不会这样调用。
fn sea_sets(kind: DictKind) -> SeaSets {
    match kind {
        DictKind::Thai => SeaSets {
            mark: thai_mark,
            end_word: thai_end_word,
            begin_word: thai_begin_word,
            suffix: thai_suffix,
        },
        DictKind::Lao => SeaSets {
            mark: lao_mark,
            end_word: lao_end_word,
            begin_word: lao_begin_word,
            suffix: never,
        },
        DictKind::Khmer => SeaSets {
            mark: khmer_mark,
            end_word: khmer_end_word,
            begin_word: khmer_begin_word,
            suffix: never,
        },
        DictKind::Burmese => SeaSets {
            mark: burmese_mark,
            end_word: burmese_end_word,
            begin_word: burmese_begin_word,
            suffix: never,
        },
        DictKind::Cjk => unreachable!("CJK 不是 SA 引擎"),
    }
}

/// `*BreakEngine::divideUpDictionaryRange(rangeStart, rangeEnd, …)`。
///
/// 返回段内边界（字符下标，升序，不含 `range_end`）。
///
/// 输入：`chars` 是整条词典段（[`segment_run`] 传 `0..chars.len()`），`sets` 是
/// 脚本的四个判定谓词，`is_thai` 打开泰文专属的后缀处理。`range_end` 是扫描与
/// 候选切片的实际终点；`range_start` 只参与两种短段门槛与泰文入口的游标初始化
/// （非泰文路径的 `text.index` 从 0 开始，没有设为 `range_start`）。因此本移植
/// 只支持 `range_start == 0`（即 `0..chars.len()`）的调用；若将来要支持子区间，
/// 需先把 `text.index` 置为 `range_start`。调用方必须保证
/// `range_start <= range_end <= chars.len()`，越界会在切片/下标处 panic。
///
/// 处理阶段（与 ICU 相同）：
///
/// 1. **短段门槛**：泰文先“移动 4 个码点”，到达/越过段尾（即长度 `<= 4`）就不切；
///    其余脚本按长度 `< 4` 判断——所以泰文比别人多一档（见
///    `tests/regression_word_segmentation.rs` 的 `short_thai_runs_are_not_subdivided`）。
/// 2. **主循环**：用 3 个 [`PossibleWord`] 槽位轮转（`words_found % SA_LOOKAHEAD`
///    决定当前槽），先取当前位置的所有词典词；唯一候选直接采纳，多候选则向前看
///    1–2 个词，选一个能让后续也成词的切法。
/// 3. **合并未知片段**：当前词短于阈值、下一个位置没有词典词、且（当前为空或
///    最长前缀也不足阈值）时，按 `end_word`/`begin_word` 把字符并进当前词，
///    避免每个字符都成为一段。
/// 4. **收尾修饰**：边界不落在组合标记（`mark`）之前；泰文还会把词尾的
///    PAIYANNOI/MAIYAMOK 吞进当前词。
/// 5. 每轮在 `cu_word_length > 0` 时把 `current + cu_word_length` 作为边界推入；
///    最后去掉落在段尾的边界。
///
/// 终止性：主循环每轮至少消费一个码点（候选为 0 时合并逻辑也至少调用一次
/// `next32`），所以每轮的起点 `current` 严格递增、必然到达 `range_end`。探测期间
/// `text.index` 会因多候选向前看（`PossibleWord::back_up`）与 `candidates` 而前后
/// 移动，但每轮结束时都等于 `current + cu_word_length`，因此推入的边界严格递增。
fn sea_divide(
    chars: &[char],
    dict: &DictionaryMatcher,
    range_start: usize,
    range_end: usize,
    sets: &SeaSets,
    is_thai: bool,
) -> Vec<usize> {
    let mut found: Vec<usize> = Vec::new();
    let mut text = RunText::new(chars);
    if is_thai {
        // 泰文入口用 `utext_moveIndex32(text, THAI_MIN_WORD_SPAN)` 判断：
        // 移动 4 个码点后到达或越过 rangeEnd（即码点数 <= 4）就不切。
        text.index = range_start;
        text.move_index32(SA_MIN_WORD_SPAN);
        if text.index >= range_end {
            return found;
        }
        text.index = range_start;
    } else if (range_end - range_start) < SA_MIN_WORD_SPAN {
        return found;
    }

    // 三个槽位轮流复用：当前词用 `words_found % SA_LOOKAHEAD`，向前看依次用 +1、+2
    // （都对 3 取模）。槽里的候选按起点缓存，回退重试时不需要重新匹配 trie。
    let mut words: Vec<PossibleWord> = (0..SA_LOOKAHEAD).map(|_| PossibleWord::new()).collect();
    let mut words_found: usize = 0;

    while text.index < range_end {
        let current = text.index;
        let mut cp_word_length: i32 = 0;
        let mut cu_word_length: i32 = 0;

        let candidates = words[words_found % SA_LOOKAHEAD].candidates(&mut text, dict, range_end);
        if candidates == 1 {
            // 唯一候选：无歧义，直接采纳。
            cu_word_length = words[words_found % SA_LOOKAHEAD].accept_marked(&mut text);
            cp_word_length = words[words_found % SA_LOOKAHEAD].marked_cp_length();
            words_found += 1;
        } else if candidates > 1 {
            // 多候选：做两段向前看。先看下一个位置是否有词；有的话把当前候选记为
            // `mark`，再看第三个位置。第三处也有词说明“当前词 + 下一个词 + 再下一个词”
            // 成立，采纳当前候选；否则回退下一个词（更短的候选）重试。第二处怎么试
            // 都不行时，回退当前词再重来。`text.index` 在这些探测中来回移动，最后由
            // `accept_marked` 复位到采纳候选的末尾。
            let mut found_best = text.index >= range_end;
            while !found_best {
                if words[(words_found + 1) % SA_LOOKAHEAD].candidates(&mut text, dict, range_end)
                    > 0
                {
                    words[words_found % SA_LOOKAHEAD].mark_current();
                    if text.index >= range_end {
                        break;
                    }
                    loop {
                        if words[(words_found + 2) % SA_LOOKAHEAD]
                            .candidates(&mut text, dict, range_end)
                            != 0
                        {
                            words[words_found % SA_LOOKAHEAD].mark_current();
                            found_best = true;
                            break;
                        }
                        if !words[(words_found + 1) % SA_LOOKAHEAD].back_up(&mut text) {
                            break;
                        }
                    }
                    if found_best {
                        break;
                    }
                }
                if !words[words_found % SA_LOOKAHEAD].back_up(&mut text) {
                    break;
                }
            }
            cu_word_length = words[words_found % SA_LOOKAHEAD].accept_marked(&mut text);
            cp_word_length = words[words_found % SA_LOOKAHEAD].marked_cp_length();
            words_found += 1;
        }

        // 向前看一个词：不是词典词时按阈值合并。
        if text.index < range_end && cp_word_length < SA_ROOT_COMBINE_THRESHOLD {
            if words[words_found % SA_LOOKAHEAD].candidates(&mut text, dict, range_end) <= 0
                && (cu_word_length == 0
                    || words[words_found % SA_LOOKAHEAD].longest_prefix()
                        < SA_PREFIX_COMBINE_THRESHOLD)
            {
                // 逐字符吞并；`remaining` 保证不越过 `range_end`（saturating_sub 只是
                // 防御，正常路径下每个码点至少消费 1）。
                let mut remaining = range_end - (current + cu_word_length as usize);
                let mut chars_passed: i32 = 0;
                loop {
                    let pc_index = text.index;
                    let Some(pc) = text.next32() else {
                        break;
                    };
                    let pc_size = (text.index - pc_index) as i32;
                    chars_passed += pc_size;
                    remaining = remaining.saturating_sub(pc_size as usize);
                    if remaining == 0 {
                        break;
                    }
                    // 前一个字符可作词尾、后一个可作词首：像是一个自然边界。再探测
                    // 这里能否起一个词典词——能则停，不能就继续吞并。`candidates`
                    // 会移动游标，所以探测后要立刻把游标复位到合并进度。
                    let uc = text.current32();
                    if (sets.end_word)(pc) && uc.is_some_and(sets.begin_word) {
                        let num_candidates = words[(words_found + 1) % SA_LOOKAHEAD]
                            .candidates(&mut text, dict, range_end);
                        text.index = current + cu_word_length as usize + chars_passed as usize;
                        if num_candidates > 0 {
                            break;
                        }
                    }
                }
                if cu_word_length <= 0 {
                    words_found += 1;
                }
                cu_word_length += chars_passed;
            } else {
                text.index = current + cu_word_length as usize;
            }
        }

        // 不要在组合标记之前断开。
        loop {
            let curr_pos = text.index;
            if !(curr_pos < range_end && text.current32().is_some_and(sets.mark)) {
                break;
            }
            text.next32();
            cu_word_length += (text.index - curr_pos) as i32;
        }

        // 泰文：词典词不跟随时吞掉后缀 PAIYANNOI / MAIYAMOK。
        if is_thai && text.index < range_end && cu_word_length > 0 {
            if words[words_found % SA_LOOKAHEAD].candidates(&mut text, dict, range_end) <= 0
                && text.current32().is_some_and(sets.suffix)
            {
                let mut uc = text.current32().unwrap_or('\0');
                if uc == THAI_PAIYANNOI {
                    // `utext_previous32(text)` 把游标退一格再取字符。
                    // 这里 `text.index >= current + cu_word_length > 0`，退一格不会下溢。
                    text.index -= 1;
                    let prev = text.chars[text.index];
                    // 前一个字符也是后缀时不吃，避免把两个连续后缀并进同一个词尾。
                    if !(sets.suffix)(prev) {
                        text.index += 1;
                        let paiyannoi_index = text.index;
                        text.index += 1;
                        cu_word_length += (text.index - paiyannoi_index) as i32;
                        uc = text.current32().unwrap_or('\0');
                    } else {
                        text.index += 1;
                    }
                }
                if uc == THAI_MAIYAMOK {
                    text.index -= 1;
                    let prev = text.chars[text.index];
                    // 与前一个字符相同（连续 MAIYAMOK）时不重复吞。
                    if prev != THAI_MAIYAMOK {
                        text.index += 1;
                        let maiyamok_index = text.index;
                        text.index += 1;
                        cu_word_length += (text.index - maiyamok_index) as i32;
                    } else {
                        text.index += 1;
                    }
                }
            } else {
                text.index = current + cu_word_length as usize;
            }
        }

        if cu_word_length > 0 {
            found.push(current + cu_word_length as usize);
        }
    }

    // 不要为词典段的末尾返回边界（ICU 的同名收尾逻辑）。主循环在词恰好结束于
    // `range_end` 时也会推入该值，这里去掉；调用方会自己补段尾。
    if found.last().is_some_and(|&last| last >= range_end) {
        found.pop();
        words_found = words_found.saturating_sub(1);
    }
    // 此后 `words_found` 不再影响输出；`let _` 只为消除“赋值后未使用”的告警。
    let _ = words_found;
    found
}

// ============================================================================
// CJK 词典引擎（ICU `dictbe.cpp` 的 CjkBreakEngine::divideUpDictionaryRange）
// ============================================================================

// CjkBreakEngine 的代价参数：`KATAKANA_COST[len]` 是把长度为 len 的片假名连续串
// 当作一个词的代价（下标 1..=8；len=4 最低，之后回升），超过 8 个码点按 8192 计。
// `MAX_SNLP` 是词典里没有以该字符开头的单字词时（`count == 0` 或
// `lengths[0] != 1`，且不是韩文音节，见 `cjk_divide`）补的单字边的代价（255）。
const K_MAX_KATAKANA_LENGTH: usize = 8;
const K_MAX_KATAKANA_GROUP_LENGTH: usize = 20;
const MAX_SNLP: i32 = 255;
const KATAKANA_COST: [u32; K_MAX_KATAKANA_LENGTH + 1] =
    [8192, 984, 408, 240, 204, 252, 300, 372, 480];

/// 长度超过 `K_MAX_KATAKANA_LENGTH` 时统一返回 8192（数组只有 0..=8 项）。
fn get_katakana_cost(word_length: usize) -> u32 {
    if word_length > K_MAX_KATAKANA_LENGTH {
        8192
    } else {
        KATAKANA_COST[word_length]
    }
}

/// 片假名判定（用于片假名连续串的启发式代价）：U+30A1..=U+30FE 去掉中点 U+30FB，
/// 加上半角片假名 U+FF66..=U+FF9F（表达式里 `&&` 优先于 `||`）。
fn is_katakana(c: char) -> bool {
    let v = c as u32;
    (0x30A1..=0x30FE).contains(&v) && v != 0x30FB || (0xFF66..=0xFF9F).contains(&v)
}

/// ICU `$HangulSyllable` = U+AC00..=U+D7A3。
///
/// 动态规划用它跳过“没有单字词命中时补单字边”的默认规则：韩文音节默认连在
/// 一起，不拆成单字。
fn is_hangul_syllable(c: char) -> bool {
    (0xAC00..=0xD7A3).contains(&(c as u32))
}

/// ICU `Normalizer2::hasBoundaryBefore`（NFKC）在本模块输入上的等价判定。
///
/// 词典段的原始字符集只有 Han/Hiragana/Katakana/ーｰﾞﾟ；归一化在切块之后进行，
/// `fragment.nfkc()` 的展开产物不会作为本函数的输入。判定只对组合类非 0 的字符
/// 与半角浊点（U+FF9E/U+FF9F，NFKC 会把它与前一个半角假名合成为全角假名）返回
/// false；Hangul V/T jamo 分支对应 ICU 通用实现，在本模块的词典段输入上不可达
/// （`text.rs` 的 `default_word_segments` 跳过谚文音节，词典段只收集
/// `is_cjk_dictionary_char` 字符）。
///
/// 因此这是**本模块输入集合上的专用判定**，不是通用的 NFKC boundary 实现：
/// 换到别的字符集上使用会漏判其他组合规则。
fn has_boundary_before(c: char) -> bool {
    if canonical_combining_class(c) != 0 {
        return false;
    }
    let v = c as u32;
    if (0x1161..=0x1175).contains(&v) || (0x11A8..=0x11C2).contains(&v) {
        return false;
    }
    if v == 0xFF9E || v == 0xFF9F {
        return false;
    }
    true
}

/// 把词段的字符做 NFKC 归一化并建立"归一化码点 → 原段 UTF-16 偏移"映射。
///
/// 对应 ICU `CjkBreakEngine::divideUpDictionaryRange` 的前半段：按
/// `hasBoundaryBefore` 切块，逐块 NFKC，块内每个归一化码点都映射到块首的原始位置
/// （`fragmentOriginalStart`）。返回 `(归一化字符, 映射)`；映射长度 = 归一化码点数 + 1，
/// 最后一项是段末尾的 UTF-16 偏移。已归一化时返回 `None`（1:1 映射）。
///
/// 输入只读，不修改 `chars`。整段本来就是 NFKC 时直接克隆原字符并返回 `None`，
/// 由调用方改用原字符的 UTF-16 偏移表，省掉映射表的构造。
fn normalize_run(chars: &[char]) -> (Vec<char>, Option<Vec<usize>>) {
    let source: String = chars.iter().collect();
    if unicode_normalization::is_nfkc(&source) {
        return (chars.to_vec(), None);
    }

    // 每个原段码点的 UTF-16 偏移（末项是段长）。
    let mut utf16_offsets = Vec::with_capacity(chars.len() + 1);
    let mut offset = 0usize;
    for &c in chars {
        utf16_offsets.push(offset);
        offset += c.len_utf16();
    }
    utf16_offsets.push(offset);

    let mut normalized: Vec<char> = Vec::with_capacity(chars.len());
    let mut map: Vec<usize> = Vec::with_capacity(chars.len() + 1);
    // 切块：每块从 `src` 开始，吞掉后面所有“前面没有边界”的字符。块与块之间
    // 不会发生 NFKC 组合，因此可以分别归一化。
    let mut src = 0usize;
    while src < chars.len() {
        let fragment_start = src;
        let mut fragment = String::new();
        fragment.push(chars[src]);
        src += 1;
        while src < chars.len() && !has_boundary_before(chars[src]) {
            fragment.push(chars[src]);
            src += 1;
        }
        let fragment_original_start = utf16_offsets[fragment_start];
        // 整块一起 NFKC；块内每个产物都映射回块首的原始偏移，所以多个归一化
        // 码点可能映射到同一位置（回溯时靠去重处理）。
        for c in fragment.nfkc() {
            map.push(fragment_original_start);
            normalized.push(c);
        }
    }
    map.push(offset);
    (normalized, Some(map))
}

/// `CjkBreakEngine::divideUpDictionaryRange`：返回段内边界（字符下标，升序）。
///
/// 阶段：
/// 1. [`normalize_run`] 做 NFKC，得到归一化码点序列（必要时带“归一化码点 →
///    原段 UTF-16 偏移”的映射）；
/// 2. 在归一化序列上跑动态规划：`best_snlp[i]` 是前 i 个码点的最小代价，
///    `prev[i]` 记录最后一段的起点；边来自词典，外加单字回退与片假名连续串
///    两条启发式边；
/// 3. 从末尾沿 `prev` 回溯出边界，再映射回原段的字符下标。
///
/// 代价用 `u32::MAX` 表示“不可达”。调用方传入的 CJK 段不含韩文音节（`text.rs`
/// 跳过韩文），因此单字回退保证每个位置都可达；“末尾不可达”分支在本仓库是
/// 防御性的。返回的下标已排序去重，且不含 0 与 `chars.len()`。
fn cjk_divide(chars: &[char], dict: &DictionaryMatcher) -> Vec<usize> {
    let n = chars.len();
    if n == 0 {
        return Vec::new();
    }
    let (normalized, map) = normalize_run(chars);
    let norm: &[char] = &normalized;
    let num_code_points = norm.len();

    // 以下 DP 的下标都在**归一化码点空间**，到函数末尾才映射回原段。
    // best_snlp[i] 是前 i 个码点的最优代价；prev[i] 是前一个词末尾的码点下标。
    let mut best_snlp = vec![u32::MAX; num_code_points + 1];
    best_snlp[0] = 0;
    let mut prev = vec![-1i32; num_code_points + 1];

    let max_word_size = 20usize;
    let mut is_prev_katakana = false;
    for i in 0..num_code_points {
        if best_snlp[i] == u32::MAX {
            continue;
        }
        // 词典给出的边：所有以 `norm[i]` 开头的词。`limit` 取全段码点数，大于任何
        // 起点可能的最大词数，因此不会截断；`max_word_size` 是 UTF-16 码元上限。
        let matches = dict.matches(&norm[i..], max_word_size, num_code_points);
        let mut lengths = matches.cp_lengths.clone();
        let mut values = matches.values.clone();
        let mut count = lengths.len();
        // 词典里没有以该字符开头的单字词时，把该字符当成单字词（最高代价）。
        // 韩文音节除外：它们默认连在一起。
        if (count == 0 || lengths[0] != 1) && !is_hangul_syllable(norm[i]) {
            values.push(MAX_SNLP);
            lengths.push(1);
            count += 1;
        }
        for j in 0..count {
            let new_snlp = best_snlp[i].saturating_add(values[j] as u32);
            let ln_j_i = lengths[j] as usize + i;
            if new_snlp < best_snlp[ln_j_i] {
                best_snlp[ln_j_i] = new_snlp;
                prev[ln_j_i] = i as i32;
            }
        }

        // 片假名连续串的启发式代价（单个片假名字符成词很罕见）。只在连续串的
        // 起点结算一次（`is_prev_katakana` 为假），否则同一串会在每个字符上重复
        // 加边；串长达到 `K_MAX_KATAKANA_GROUP_LENGTH`（20）时不加这条边。
        let is_kata = is_katakana(norm[i]);
        let mut katakana_run_length = 1usize;
        if !is_prev_katakana && is_kata {
            let mut j = i + 1;
            while j < num_code_points
                && katakana_run_length < K_MAX_KATAKANA_GROUP_LENGTH
                && is_katakana(norm[j])
            {
                j += 1;
                katakana_run_length += 1;
            }
            if katakana_run_length < K_MAX_KATAKANA_GROUP_LENGTH {
                let new_snlp = best_snlp[i].saturating_add(get_katakana_cost(katakana_run_length));
                let target = i + katakana_run_length;
                if new_snlp < best_snlp[target] {
                    best_snlp[target] = new_snlp;
                    prev[target] = i as i32;
                }
            }
        }
        is_prev_katakana = is_kata;
    }

    // t_boundary：从末尾回溯，之后再反转（ICU 的做法）。
    let mut t_boundary: Vec<i32> = Vec::with_capacity(num_code_points + 1);
    if best_snlp[num_code_points] == u32::MAX {
        // 末尾不可达（异常/防御场景）：只保留段尾，效果等价于整段不切分。
        t_boundary.push(num_code_points as i32);
    } else {
        let mut i = num_code_points as i32;
        while i > 0 {
            t_boundary.push(i);
            i = prev[i as usize];
        }
    }
    if t_boundary.last() != Some(&0) {
        t_boundary.push(0);
    }

    // 归一化空间（码点下标）→ 原段 UTF-16 偏移 → 字符下标。
    // 下面重建原段每个码点的 UTF-16 偏移（升序，末项是段长），用于把回溯结果
    // 二分成原段的字符下标。
    let mut utf16_offsets = Vec::with_capacity(chars.len() + 1);
    let mut offset = 0usize;
    for &c in chars {
        utf16_offsets.push(offset);
        offset += c.len_utf16();
    }
    utf16_offsets.push(offset);

    let mut found: Vec<usize> = Vec::new();
    let mut prev_utext_pos: Option<usize> = None;
    for &cp_pos in t_boundary.iter().rev() {
        let cp_pos = cp_pos as usize;
        let utext_pos = match &map {
            Some(map) => map[cp_pos],
            None => utf16_offsets[cp_pos],
        };
        if prev_utext_pos.is_none_or(|prev| utext_pos > prev) {
            prev_utext_pos = Some(utext_pos);
            if utext_pos != 0 {
                // 映射回字符下标（边界一定落在码点起点上）。
                let char_index = match utf16_offsets.binary_search(&utext_pos) {
                    Ok(index) => index,
                    Err(_) => continue,
                };
                if char_index < n {
                    found.push(char_index);
                }
            }
        }
    }
    // ICU 会丢掉 rangeEnd 处的边界；这里 `char_index < n` 已经保证。映射会把同一
    // 块的多个归一化边界折到同一偏移，所以必须去重；排序是防御性的（映射值单调
    // 不减、遍历顺序已是升序）。
    found.sort_unstable();
    found.dedup();
    found
}

// ============================================================================
// 对外入口
// ============================================================================

/// 对一个词典连续段求段内边界（段内字符下标，升序，不含段尾）。
///
/// `chars` 必须是 [`DictKind`] 对应引擎的 `fSet` 连续段；调用方
/// （[`crate::text::default_word_segments`]）已经保证这一点。
///
/// 返回的下标都落在 `1..chars.len()` 内且严格递增，可直接作为切点；没有内部边界
/// 或输入为空时返回空 `Vec`。分派：CJK → [`cjk_divide`]，其余 → [`sea_divide`]
/// （泰文多一个 `is_thai` 开关）。
///
/// 只读 `chars`，所有状态都是本次调用内的局部变量，不缓存任何中间结果。
pub(crate) fn segment_run(chars: &[char], kind: DictKind) -> Vec<usize> {
    if chars.is_empty() {
        return Vec::new();
    }
    let dict = DictionaryMatcher::new(kind);
    match kind {
        DictKind::Cjk => cjk_divide(chars, &dict),
        _ => sea_divide(
            chars,
            &dict,
            0,
            chars.len(),
            &sea_sets(kind),
            kind == DictKind::Thai,
        ),
    }
}

// 单元测试直接调用 `segment_run`，绕过 text.rs 的规则引擎，单独锁定词典引擎的段内
// 边界；期望值由 Node 26.12.1（ICU 78.3）的 `Intl.Segmenter` 生成。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_cut_points_match_icu78() {
        // 期望值来自 Node 26.12.1（ICU 78.3）的 Intl.Segmenter。
        let cases: &[(&str, &[usize])] = &[
            ("ひらがな", &[1, 2, 3]),
            ("中文字", &[1]),
            ("日本語", &[]),
            ("ありがとう", &[]),
            ("かたかな", &[1, 2]),
            ("コンピュータ", &[]),
            ("ファイルを保存します", &[4, 5, 7, 8]),
            ("ﾊﾟﾝﾊﾟﾝ", &[]),
        ];
        for (text, expected) in cases {
            let chars: Vec<char> = text.chars().collect();
            assert_eq!(segment_run(&chars, DictKind::Cjk), *expected, "{text:?}");
        }
    }

    #[test]
    fn thai_cut_points_match_icu78() {
        let cases: &[(&str, &[usize])] = &[
            ("าษาไทย", &[3]),
            ("จากนั้น", &[3]),
            ("จอห์น", &[]),
            ("สวัสดีครับ", &[6]),
            ("สมัครสมาชิกแล้ว", &[5, 11]),
        ];
        for (text, expected) in cases {
            let chars: Vec<char> = text.chars().collect();
            assert_eq!(segment_run(&chars, DictKind::Thai), *expected, "{text:?}");
        }
    }

    #[test]
    fn lao_khmer_burmese_cut_points_match_icu78() {
        let cases: &[(&str, DictKind, &[usize])] = &[
            ("ສະບາຍດີ", DictKind::Lao, &[]),
            ("ພາສາລາວ", DictKind::Lao, &[4]),
            ("សួស្តី", DictKind::Khmer, &[]),
            ("ភាសាខ្មែរ", DictKind::Khmer, &[]),
            ("မင်္ဂလာပါ", DictKind::Burmese, &[7]),
        ];
        for (text, kind, expected) in cases {
            let chars: Vec<char> = text.chars().collect();
            assert_eq!(segment_run(&chars, *kind), *expected, "{text:?}");
        }
    }
}
