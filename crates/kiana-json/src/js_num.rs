//! ECMAScript 数字 → JSON 文本。
//!
//! 这是整个工作区里**唯一**允许把浮点写成 JSON 的地方。JS 的 `JSON.stringify` 对数字用的是
//! `Number::toString`（最短往返表示），它与 Rust 的 `{}` / `format!` / serde 默认行为都不同：
//!
//! | 值 | JS `JSON.stringify` | Rust `format!("{v}")` |
//! |---|---|---|
//! | `1e21` | `"1e+21"` | `"1000000000000000000000"` |
//! | `1e-7` | `"1e-7"` | `"0.0000001"` |
//! | `1e-6` | `"0.000001"` | `"0.000001"` |
//! | `-0.0` | `"0"` | `"-0"` |
//! | `NaN` / `Infinity` | `"null"` | `"NaN"` / `"inf"` |
//! | `123456789012345680000.0` | `"123456789012345680000"` | `"123456789012345680000"` |
//!
//! 规则（ECMA-262 Number::toString，基数为 10）：
//! - `k` 是用最短往返位数表示的有效数字（由 `ryu` 给出，`ryu` 的输出格式需先解析回 (digits, exp)）。
//! - 若 `k <= n <= 21`：整数形式，后面补 `n - k` 个 `0`。
//! - 若 `0 < n <= 21`：`digits[..n] "." digits[n..]`。
//! - 若 `-6 < n <= 0`：`"0." + "0" * (-n) + digits`。
//! - 否则指数形式：`d[0]` + (`"." + rest` 若 k > 1) + `"e"` + (`"+"` 若 n-1 > 0) + `(n-1)`。
//!
//! 枚举型别的 `n` 是「小数点位置」：`n = exp + 1`（`value = 0.digits * 10^n`）。
//!
//! # 设计原因：为什么是「ryu + 重排」而不是自己实现最短表示
//!
//! JS 的 `Number::toString` 与 `ryu` 都要求「最短且能往返」的有效数字串，二者在
//! **位数**上是同一个结果，差异只在排版（小数点位置与指数写法）。因此本模块复用
//! `ryu` 的最短数字，再由 `decompose` 归一成 `(digits, n)`，最后按 ECMA-262 的四条
//! 排版规则拼字符串。这样 `kiana-json` **内部**只有一套最短表示算法和一处排版实现；
//! 工作区里另有两处本地复刻（`kiana-env-client/src/lib.rs` 与
//! `kiana-evals/src/report.rs`），修改排版规则时需要同步检查它们。`kiana-env-client`
//! 是因为 `kiana-json` 不在其依赖边内（`ARCHITECTURE.md` §2.2）；`kiana-evals` 现已
//! 声明 `kiana-json` 依赖，其副本属于待清理的重复实现。
//!
//! 这条「位数来自 ryu、排版来自 ECMA」的分工是硬约束：修改 `decompose` 或任一排版
//! 分支时，必须同时对照 `tests/js_number.rs` 的黄金表与 `tests/properties.rs` 的
//! 往返性质，否则可能与 V8 的输出分叉。
//!
//! # 执行流程
//!
//! - [`format_js_number`]：非有限值与零提前返回；否则 `ryu` 取最短数字 →
//!   `decompose` 归一 → 按 `k`（有效位数）与 `n`（小数点位置）选择整数/定点/指数
//!   排版 → 补负号。
//! - [`parse_js_number`]：按 ECMAScript `ToNumber` 的字符串分支处理——先按 JS 空白集
//!   trim；空串/纯空白返回 `+0`，其余输入再识别 `0x/0b/0o` 前缀（BigUint 精确累加后
//!   一次舍入）、`±Infinity`、十进制字面量；任一环节不匹配就返回 `NaN`。它是「宽松」
//!   入口：错误用 `NaN` 表达，没有 `Result`。
//! - [`to_radix_string`]：radix 10 直接委托 `format_js_number`；其余进制是 V8
//!   `DoubleToRadixStringView` 的逐位移植（整数与小数分别处理）。
//!
//! # 注意事项
//!
//! - 本模块只负责「数字的格式化/解析」，不处理 JSON 结构；键序与容器写出由
//!   [`crate::js_object`] 负责。
//! - `NaN`/`±Infinity` 在 JSON 语境写成 `null`，这个文本不可逆：从 `"null"` 解析回来
//!   只能得到 `Value::Null`。需要区分「原本是 Infinity」与「原本是 null」时，调用方
//!   必须在更上层保存信息。
//! - `-0` 的文本也是 `"0"`；符号只保留在 `f64` 内存表示里（`parse_js_number("-0")`
//!   会保留它，`format_js_number(-0.0)` 会写出零）。
//! - [`to_radix_string`] 对越界 radix 直接 `panic!`（JS 抛 `RangeError`，但该函数签名
//!   没有 `Result`）；调用方必须自行保证 `2..=36`。

/// 把 `f64` 写成与 JS `JSON.stringify` 一致的文本（不含引号）。
///
/// `NaN` 与 `±Infinity` 在 JSON 里写作 `null`——这就是 TS 里 `JSON.stringify(NaN)` 的结果。
///
/// # 参数与返回
/// - `value`：任意 `f64`，包括非有限值与 `-0.0`。
/// - 返回：可直接拼进 JSON 的文本——有限值是合法的 JSON 数字字面量，非有限值是
///   `null`。函数没有 `Result`，因为每种输入都有定义好的输出。
///
/// # 不变量
/// 对任何有限值，[`crate::js_parse::parse`] 能把本函数的输出读回同一个 `f64`
/// （`-0` 例外：文本 `"0"` 只能读回 `+0`，符号在文本层不可观察）。这条性质由
/// `tests/properties.rs` 的随机位模式测试覆盖。
///
/// # 为什么不直接用 `Display`
/// Rust 的 `format!("{v}")` 使用另一套排版（大数写全、`1e-7` 写成 `0.0000001`），
/// 与 JS 的 `JSON.stringify` 文本不同；任何浮点写出都必须经过本函数
/// （`ARCHITECTURE.md` §4.3 第 4 条）。整数可以保持 `i64`/`u64`，但需先满足 JS 的
/// f64 语义（[`crate::js_parse::normalize_numbers`]），或直接走
/// [`crate::js_object::stringify`]——它在写文本时统一按 f64 处理。
pub fn format_js_number(value: f64) -> String {
    if value.is_nan() || value.is_infinite() {
        return "null".to_string();
    }
    if value == 0.0 {
        // -0 与 +0 都是 "0"。
        return "0".to_string();
    }

    // ryu 给出最短往返表示，但格式不一定是 JS 的规则；先解析成 (digits, decimal_exponent)。
    let mut buf = ryu::Buffer::new();
    let ryu_text = buf.format_finite(value);
    let (digits, n) = decompose(ryu_text);

    let negative = value.is_sign_negative();
    let k = digits.len() as i32;
    let body = if k <= n && n <= 21 {
        let mut s = digits;
        for _ in 0..(n - k) {
            s.push('0');
        }
        s
    } else if 0 < n && n <= 21 {
        let n_usize = n as usize;
        let mut s = String::with_capacity(digits.len() + 1);
        s.push_str(&digits[..n_usize]);
        s.push('.');
        s.push_str(&digits[n_usize..]);
        s
    } else if -6 < n && n <= 0 {
        let mut s = String::with_capacity(digits.len() + 2 + (-n) as usize);
        s.push_str("0.");
        for _ in 0..(-n) {
            s.push('0');
        }
        s.push_str(&digits);
        s
    } else {
        let mut s = String::with_capacity(digits.len() + 6);
        s.push_str(&digits[..1]);
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        s.push('e');
        let e = n - 1;
        if e > 0 {
            s.push('+');
        }
        s.push_str(&e.to_string());
        s
    };

    if negative {
        let mut s = String::with_capacity(body.len() + 1);
        s.push('-');
        s.push_str(&body);
        s
    } else {
        body
    }
}

/// 把 ryu 的文本拆成 (有效数字串, 小数点位置 n)。
///
/// ryu 的 `format_finite` 输出形如 `1234.5`、`1e-7`、`1.2345e21`、`0.0001`；
/// 统一改写为 `digits`（无前导/尾随零）与十进制指数 `n`。
///
/// # 关键假设
/// 输入只来自 `ryu::Buffer::format_finite`：有限、非零（零与非有限值已在
/// [`format_js_number`] 入口提前返回），可能带 `-`，指数部分若存在必为合法整数。
/// 因此 `e[1..].parse().unwrap_or(0)` 只是防御性兜底——若升级 `ryu` 或改用其他
/// 格式化器，必须重跑 `tests/js_number.rs` 的黄金表；否则格式变化会被这个兜底
/// 静默变成数值错误，而不是显式失败。
///
/// # 为什么先拼接再裁零
/// 指数写法（`1.2345e21`）与定点写法（`0.0001`）的小数点位置不同；先把整数位与
/// 小数位拼成一条数字串，再用「整数部分长度 + 指数」统一算出小数点位置 `n`，
/// 后面才能用同一组 ECMA 规则排版。
fn decompose(text: &str) -> (String, i32) {
    let text = text.strip_prefix('-').unwrap_or(text);

    let (mantissa, exp) = match text.find(['e', 'E']) {
        Some(i) => {
            let (m, e) = text.split_at(i);
            let e: i32 = e[1..].parse().unwrap_or(0);
            (m, e)
        }
        None => (text, 0),
    };

    let (int_part, frac_part) = match mantissa.find('.') {
        Some(i) => (&mantissa[..i], &mantissa[i + 1..]),
        None => (mantissa, ""),
    };

    let mut digits = String::with_capacity(int_part.len() + frac_part.len());
    digits.push_str(int_part);
    digits.push_str(frac_part);

    // 小数点位置：整数部分长度 + 指数。
    let mut n = int_part.len() as i32 + exp;

    // 去掉前导零，同时左移小数点位置。
    let leading = digits.chars().take_while(|c| *c == '0').count() as i32;
    if leading > 0 {
        digits.drain(..leading as usize);
        n -= leading;
    }
    // 去掉尾随零；若全是零，则值本身为 0（已在入口处理）。
    let trailing = digits.chars().rev().take_while(|c| *c == '0').count();
    if trailing > 0 && trailing < digits.len() {
        let new_len = digits.len() - trailing;
        digits.truncate(new_len);
    }
    if digits.is_empty() {
        digits.push('0');
        n = 1;
    }
    (digits, n)
}

/// 判断一个 `f64` 在 JS 里是否是「整数」（`Number.isInteger` 语义）。
///
/// 等价于 `Number.isInteger(value)`：值为有限且没有小数部分。具体地：
/// - `-0.0` 返回 `true`（JS 里 `Number.isInteger(-0) === true`）；
/// - `1e21` 这类超出安全整数范围的 f64 仍返回 `true`（`Number.isInteger` 只看数值
///   形态，不看能否安全表示）；
/// - `NaN`/`±Infinity` 返回 `false`。
///
/// 调用方 `js_parse` 用它决定解析出的数字能否保留 `i64`/`u64` 表示；后者还需要
/// 另外检查 ±2^53 安全范围（见 `js_parse` 的 `integer_number`），本函数不承担这一
/// 职责。
pub fn is_js_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

const RADIX_DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// `Number.prototype.toString(radix)`，radix 取 2..=36（对应 `V8::DoubleToRadixStringView`）。
///
/// 与 `format_js_number`（基数 10 的 `Number::toString`）是两套算法：这里按 V8 的
/// `DoubleToRadixStringView` 逐位实现——整数部分反复除以 radix（每一步都重新对 f64 取整），
/// 小数部分用 `delta`（0.5 ulp）控制位数并在余数过半时向偶数舍入、必要时回溯进位。
///
/// radix 越界时与 JS 一样抛错：JS 抛 `RangeError`，签名返回 `String`，因此用 `panic!`。
///
/// `NaN`/`±Infinity` 分别写作 `"NaN"`/`"Infinity"`/`"-Infinity"`；`-0` 写作 `"0"`。
///
/// # 实现边界
/// - 小数部分用 `delta`（初值 0.5 ulp，逐步乘以 radix）判定何时停止，因此输出位数
///   取决于输入值的 f64 精度，而不是固定的小数位；`delta` 的下限是
///   [`f64::from_bits`] 的最小次正规数，保证循环一定终止。
/// - 整数部分始终用 f64 运算（与 V8 的 `DoubleToRadixStringView` 相同），因此
///   超过 2^53 时每一步除法都会按 f64 舍入。改成精确整数运算不是「纯优化」，
///   需要重新对照 V8 的输出。
/// - 只有越界 radix 会失败（`panic!`，见上）；任何 `f64` bit 模式都有确定输出。
///
/// # 与 `format_js_number` 的关系
/// radix 为 10 且值有限时两者产生相同文本（`tests/properties.rs` 的
/// `radix_ten_matches_number_to_string` 覆盖）。非有限值两者刻意不同：本函数是
/// `String(v)` 语境（`"NaN"`/`"Infinity"`/`"-Infinity"`），后者是 JSON 语境
/// （`"null"`）。
pub fn to_radix_string(value: f64, radix: u32) -> String {
    assert!((2..=36).contains(&radix), "radix out of range");
    // 非有限值与 ±0 先于进制分派：`(NaN).toString(10)` 是 `"NaN"`（不是 JSON 的 `"null"`），
    // 而 `format_js_number` 是 JSON 语境（`JSON.stringify(NaN) === "null"`）。
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return "0".to_string();
    }
    if radix == 10 {
        // V8 的 Number.prototype.toString(10) 直接走 Number::toString（有限非零值）。
        return format_js_number(value);
    }

    let negative = value < 0.0;
    let value = if negative { -value } else { value };
    let radix_f = f64::from(radix);
    let radix_i = i32::try_from(radix).expect("radix fits in i32");

    // Split the value into an integer part and a fractional part.
    let mut integer = value.floor();
    let mut fraction = value - integer;
    // We only compute fractional digits up to the input double's precision.
    // delta = 0.5 * (next_double(value) - value), clamped to the smallest
    // denormal so that fraction digits always terminate.
    let mut delta = 0.5 * (next_double(value) - value);
    let min_denormal = f64::from_bits(1);
    // `!(delta > min)` 而非 `delta <= min`：delta 可能是 0（次正规数），
    // 语义与 V8 的 `std::max` 一致。
    if delta.partial_cmp(&min_denormal) != Some(std::cmp::Ordering::Greater) {
        delta = min_denormal;
    }

    // 阶段 2：逐位生成小数。每轮把余数左移一位（乘以 radix）后取整数部分作为
    // digit，同时把 delta 放大同样的倍数。循环有两个退出条件：
    // 1. 进入下面的 round-to-even 舍入分支（写完整数位后 `break`）；
    // 2. 余数已经小于 delta——剩余位数低于输入 f64 能分辨的精度，再写也没有意义。
    // 这与 V8 的 `DoubleToRadixStringView` 一致。
    let mut fraction_digits = String::new();
    if fraction >= delta {
        fraction_digits.push('.');
        loop {
            // Shift up by one digit.
            fraction *= radix_f;
            delta *= radix_f;
            // Write digit.
            let digit = fraction as i32;
            fraction_digits.push(RADIX_DIGITS[digit as usize] as char);
            // Calculate remainder.
            fraction -= f64::from(digit);
            // Round to even.
            if (fraction > 0.5 || (fraction == 0.5 && (digit & 1) != 0)) && fraction + delta > 1.0 {
                // We need to back trace already written digits in case of carry-over.
                // V8 的游标从最后写入的 digit 开始（不是它前面一个）：命中 `digit + 1 < radix`
                // 就把它替换为 `digit + 1`；一路进到小数点则整数部分 +1 且不再有小数位。
                let mut cursor = fraction_digits.len();
                loop {
                    if cursor == 0 {
                        // 意外耗尽（理论上不会发生：'.' 总会先被看到）。
                        integer += 1.0;
                        fraction_digits.clear();
                        break;
                    }
                    cursor -= 1;
                    let c = fraction_digits.as_bytes()[cursor];
                    if c == b'.' {
                        // Carry over to the integer part；V8 的返回区间不含小数点。
                        fraction_digits.truncate(cursor);
                        integer += 1.0;
                        break;
                    }
                    let digit = if c > b'9' {
                        i32::from(c - b'a') + 10
                    } else {
                        i32::from(c - b'0')
                    };
                    if digit + 1 < radix_i {
                        // 用 `digit + 1` 覆写该位，并丢弃它之后已回溯掉的部分。
                        fraction_digits.truncate(cursor);
                        fraction_digits.push(RADIX_DIGITS[(digit + 1) as usize] as char);
                        break;
                    }
                }
                break;
            }
            if fraction.partial_cmp(&delta) == Some(std::cmp::Ordering::Less) {
                break;
            }
        }
    }

    // Compute integer digits. Fill unrepresented digits with zero.
    // V8 writes those zeros *after* the leading digit block, so count them here.
    // 阶段 3：整数部分。先用 `v8_exponent` 统计前导数字块之外还要补几个零（V8 的
    // 填充顺序要求零写在前导数字之后，而不是先补零再做除法）；再反复取余数得到各
    // 进制位。整个过程使用 f64 运算，与 V8 的 `DoubleToRadixStringView` 一致。
    let mut trailing_zeros = 0usize;
    while v8_exponent(integer / radix_f) > 0 {
        integer /= radix_f;
        trailing_zeros += 1;
    }
    let mut integer_digits: Vec<u8> = Vec::new();
    loop {
        let remainder = integer % radix_f;
        integer_digits.push(RADIX_DIGITS[remainder as usize]);
        integer = (integer - remainder) / radix_f;
        if integer.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
            break;
        }
    }
    integer_digits.reverse();

    let mut out =
        String::with_capacity(integer_digits.len() + trailing_zeros + fraction_digits.len() + 1);
    if negative {
        out.push('-');
    }
    for b in &integer_digits {
        out.push(*b as char);
    }
    for _ in 0..trailing_zeros {
        out.push('0');
    }
    out.push_str(&fraction_digits);
    out
}

/// V8 `Double::Exponent()`: `biased_e - kExponentBias (1075)`; denormals use `-1074`.
fn v8_exponent(value: f64) -> i32 {
    let bits = value.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    if biased == 0 { -1074 } else { biased - 1075 }
}

/// V8 `Double::NextDouble()`: the next representable double toward positive infinity.
fn next_double(value: f64) -> f64 {
    if value == f64::INFINITY {
        return value;
    }
    let bits = value.to_bits();
    if value.is_sign_negative() {
        if bits & 0x7fff_ffff_ffff_ffff == 0 {
            // -0.0 -> 0.0
            return 0.0;
        }
        f64::from_bits(bits - 1)
    } else {
        f64::from_bits(bits + 1)
    }
}

/// JS 的 `Number(string)`（`ToNumber` 语义，字符串输入）。
///
/// 与 `str::parse::<f64>()` 的差异：
/// - `trim` 集是 ECMAScript 的 `WhiteSpace + LineTerminator`，**不是** Rust 的 `char::is_whitespace`
///   （不含 U+0085/U+001C-1F/U+180E，但含 U+00A0/U+FEFF/所有 Zs）；
/// - 空串/纯空白 → `+0`；
/// - 前缀 `0x`/`0X`、`0b`/`0B`、`0o`/`0O`（**只接受无符号形式**，`"-0x10"` → `NaN`）；
/// - `Infinity`/`+Infinity`/`-Infinity` 精确拼写；
/// - 十进制字面量走 `StringToDouble`（正确舍入，`1e400` → `Infinity`）；
/// - 其余一律 `NaN`；`-0` 保留符号。
///
/// # 返回值编码
/// 没有 `Result`：不合法输入就是 `NaN`（与 JS 的 `Number("abc")` 一致），调用方用
/// `is_nan()` 区分。因此「字面量拼写为 `NaN`」与「输入非法」在返回值上不可区分——
/// JS 里同样如此（`Number("NaN")` 是 `NaN`）。
///
/// # 已知边界
/// - 只实现字符串输入的 `ToNumber`；对象的 `valueOf`/`toString` 强制转换不在本 crate
///   的职责内。
/// - `0x`/`0b`/`0o` 前缀只接受相应进制的 ASCII 数字；`0x` 后为空、含 `_` 或小数点
///   一律返回 `NaN`（JS 的数字分隔符不允许出现在 `Number()` 的字符串里）。
/// - 十进制路径依赖标准库 `f64` 解析的正确舍入保证；升级 MSRV 后应重跑
///   `tests/js_number.rs` 的 coercion 黄金表。
///
/// JSON 解析器 [`crate::js_parse::parse`] **不**调用本函数：它按 JSON 语法自行切分
/// 数字，再调用 `parse_decimal_literal`，因为 JSON 不允许进制前缀与 `Infinity`。
/// 本函数服务于 `Number(string)` 语义的调用点。
pub fn parse_js_number(text: &str) -> f64 {
    let t = trim_js_whitespace(text);
    if t.is_empty() {
        return 0.0;
    }

    // 二进制/八进制/十六进制前缀（无符号）。
    let bytes = t.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let (base, valid): (u32, fn(u8) -> bool) = match bytes[1] {
            b'x' | b'X' => (16, |b: u8| b.is_ascii_hexdigit()),
            b'b' | b'B' => (2, |b: u8| b == b'0' || b == b'1'),
            b'o' | b'O' => (8, |b: u8| (b'0'..=b'7').contains(&b)),
            _ => (0, |_| false),
        };
        if base != 0 {
            // V8 的 StringToDouble 对带进制前缀的字面量做**一次**正确舍入：整个数字串当作精确
            // 整数再转 f64。用 f64 累加（`acc = acc * base + d`）在超过 53 位后每步都可能丢位，
            // 实测 `Number("0xc6C03A14c6c8a41691E9CCEaeec18c")` 会差 1 ulp。这里改用 BigUint
            // 精确累加，最后 `to_f64()` 一次舍入（round-half-to-even，与 V8 一致）。
            let mut acc = num_bigint::BigUint::from(0u8);
            let big_base = num_bigint::BigUint::from(base);
            for &b in &bytes[2..] {
                if !valid(b) {
                    return f64::NAN;
                }
                let d = (b as char).to_digit(base).expect("validated digit");
                acc = acc * &big_base + num_bigint::BigUint::from(d);
            }
            return big_uint_to_f64(&acc);
        }
    }

    if let Some(rest) = t.strip_prefix('+') {
        // "+Infinity" is the only accepted signed-plus form; "+1" is valid too.
        if rest == "Infinity" {
            return f64::INFINITY;
        }
        return scan_decimal(rest, 1.0);
    }
    if let Some(rest) = t.strip_prefix('-') {
        if rest == "Infinity" {
            return f64::NEG_INFINITY;
        }
        return scan_decimal(rest, -1.0);
    }
    if t == "Infinity" {
        return f64::INFINITY;
    }
    scan_decimal(t, 1.0)
}

/// `BigUint` → `f64`（一次正确舍入；数值超出 f64 范围时为 `Infinity`）。
fn big_uint_to_f64(value: &num_bigint::BigUint) -> f64 {
    use num_traits::ToPrimitive;
    // `BigUint::to_f64` 内部按 IEEE-754 round-half-to-even 转换，溢出为 `Some(inf)`。
    value.to_f64().unwrap_or(f64::INFINITY)
}

/// `StringToDouble` 的十进制部分：`StrDecimalLiteral`（不含已有符号）。
///
/// 返回 `NaN` 表示整串不匹配；数值取整串并按 IEEE-754 正确舍入（Rust 的 `f64` 解析器
/// 在最近的编译器里已与 V8 一致，`1e400` 溢出为 `Infinity`、次正规数正确）。
fn scan_decimal(text: &str, sign: f64) -> f64 {
    if !is_decimal_literal(text) {
        return f64::NAN;
    }
    match text.parse::<f64>() {
        Ok(v) => v * sign,
        Err(_) => f64::NAN,
    }
}

/// 严格匹配 ECMAScript `StrDecimalLiteral`（不含符号）：
/// `Infinity | Digits . Digits? ExponentPart? | . Digits ExponentPart? | Digits ExponentPart?`。
fn is_decimal_literal(text: &str) -> bool {
    let b = text.as_bytes();
    let mut i = 0;
    let mut saw_int = false;
    let mut saw_frac = false;

    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
        saw_int = true;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
            saw_frac = true;
        }
    }
    if !saw_int && !saw_frac {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            i += 1;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == b.len()
}

/// ECMAScript 的 `WhiteSpace` + `LineTerminator` 集合（`String.prototype.trim` 的字符集）。
///
/// 与 `char::is_whitespace` 的差异：包含 U+FEFF（BOM/ZWNBSP）、U+00A0、U+1680、U+2000-200A、
/// U+202F、U+205F、U+3000；**不包含** U+0085（NEL）、U+001C-001F、U+180E。
///
/// 这是本 crate 中唯一的 ECMAScript 空白定义：`parse_js_number` 的 trim 与
/// `js_string::js_trim` 都复用它，因此修改这里会同时改变「数字解析」与
/// `String.prototype.trim` 的行为，两者不能分开演化。
pub(crate) fn is_js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// `String.prototype.trim()`（ECMAScript 的 WhiteSpace + LineTerminator）。
pub(crate) fn trim_js_whitespace(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// 把一个 JSON 数字字面量（已拆成符号/整数位/小数位/十进制指数）按 JS 语义解析成 `f64`。
///
/// 供 `js_parse` 使用。V8 的 `StringToDouble` 与 Rust 的 `str::parse::<f64>()` 都是
/// 正确舍入（round-half-to-even）的十进制→二进制转换，实测 601 条随机/极端字面量逐 bit 一致
/// （`123456789012345680000` → `441ac53a7e04bcda`、`9007199254740993` → `...992`、
/// `1e400` → `Infinity`、`2.4703282292062327e-324` → `0`、`2.4703282292062328e-324` → `5e-324`）。
/// 因此这里不重写大数运算，只把字面量规整成 `"<digits>e<exp>"` 交给标准库。
///
/// `negative` 只影响符号；`"-0"` 必须得到 `-0.0`（JS 保留负零）。
///
/// # 边界
/// `exp10` 由调用方提供，可能已经被饱和（`js_parse` 对超长指数取 `i64::MAX / 4`）；
/// `saturating_sub` 保证减去小数位长度时不会下溢。标准库解析溢出/下溢时的行为与
/// JS 一致：`1e400` → `Infinity`，`1e-400` → `0`。若解析意外失败（理论上不会，
/// 因为输入已被规整为 `<digits>e<exp>`），按指数符号退回 `Infinity` 或 `0`。
pub(crate) fn parse_decimal_literal(
    negative: bool,
    int_digits: &str,
    frac_digits: &str,
    exp10: i64,
) -> f64 {
    let mut digits = String::with_capacity(int_digits.len() + frac_digits.len());
    digits.push_str(int_digits);
    digits.push_str(frac_digits);
    let trimmed = digits.trim_start_matches('0');
    let magnitude = if trimmed.is_empty() {
        0.0
    } else {
        let effective_exp = exp10.saturating_sub(frac_digits.len() as i64);
        // `format!` 的指数写法 `1e-400` 与 `1e400` 都被标准库解析为正确舍入值
        // （含溢出到 Infinity 与下溢到 0/subnormal）。
        let text = format!("{trimmed}e{effective_exp}");
        text.parse::<f64>().unwrap_or({
            // 指数大到 `i64` 也装不下时才会走到这里（字面量本身已被上限截断），
            // 语义等价于 JS 的 Infinity。
            if effective_exp > 0 {
                f64::INFINITY
            } else {
                0.0
            }
        })
    };
    if negative { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::format_js_number;

    #[test]
    fn matches_javascript_number_to_string() {
        // 期望值取自 Node 22 的 `JSON.stringify`（见 ARCHITECTURE.md §4.3 的黄金表）。
        assert_eq!(format_js_number(0.0), "0");
        assert_eq!(format_js_number(-0.0), "0");
        assert_eq!(format_js_number(1.0), "1");
        assert_eq!(format_js_number(-1.5), "-1.5");
        assert_eq!(format_js_number(0.1), "0.1");
        assert_eq!(format_js_number(1e-6), "0.000001");
        assert_eq!(format_js_number(1e-7), "1e-7");
        assert_eq!(format_js_number(1e21), "1e+21");
        assert_eq!(format_js_number(1e20), "100000000000000000000");
        assert_eq!(
            format_js_number(123456789012345680000.0),
            "123456789012345680000"
        );
        assert_eq!(format_js_number(f64::NAN), "null");
        assert_eq!(format_js_number(f64::INFINITY), "null");
        assert_eq!(format_js_number(f64::NEG_INFINITY), "null");
        assert_eq!(format_js_number(1234.5), "1234.5");
        assert_eq!(format_js_number(1.25e-10), "1.25e-10");
        assert_eq!(format_js_number(9007199254740993.0), "9007199254740992");
    }
}
