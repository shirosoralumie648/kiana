//! TS `streaming-json.test.ts` 的生成器与独立参照实现（`referenceSnapshot`）的原生 Rust 版本。
//!
//! 逐字移植 `random` / `randomDocuments` / `render` / `read32` / `referenceSnapshot`，把差分模糊
//! 测试从 fixture（60 个文档、chunk size 1/3）扩展到 TS 的完整规模（250/400/300/150 个文档，
//! chunk size 1/2/3/7/random）。
//!
//! TS 按 UTF-16 码元切分且可能拆开代理对；Rust 的 `&str` 无法表示半个代理对
//! （ARCHITECTURE.md §4.6），因此调用方按字符切分（切分点永远落在码点边界）。

use serde_json::{Map, Number, Value};

/// 播种随机数（TS: `random(seed)`）。
///
/// JS 的 `(state * 1103515245 + 12345) & 0x7fffffff` 先以 double 计算，再做 `ToInt32`
/// （模 2^32 的有符号解释），最后清掉符号位。用 `f64` 复刻同样的浮点舍入。
pub struct Rand {
    state: f64,
}

impl Rand {
    pub fn new(seed: f64) -> Self {
        Self { state: seed }
    }

    pub fn next(&mut self) -> f64 {
        let next = self.state * 1_103_515_245.0 + 12_345.0;
        let wrapped = (next as i64 as i32) & 0x7fff_ffff;
        self.state = wrapped as f64;
        self.state / 2_147_483_647.0
    }
}

pub fn pick<'a, T>(rand: &mut Rand, items: &'a [T]) -> &'a T {
    let index = (rand.next() * items.len() as f64).floor() as usize;
    &items[index]
}

const STRING_PIECES: &[&str] = &[
    "a", "b", "x y", " ", "\n", "\t", "\\", "\"", "/", "é", "😀", "\u{0001}", "\u{001f}",
    "\u{2028}", "{", "]", ",",
];

pub fn random_string(rand: &mut Rand) -> String {
    let mut text = String::new();
    let length = (rand.next() * 6.0).floor() as usize;
    for _ in 0..length {
        text.push_str(pick(rand, STRING_PIECES));
    }
    text
}

fn number(value: f64) -> Value {
    Value::Number(Number::from_f64(value).expect("finite random number"))
}

pub fn random_scalar(rand: &mut Rand) -> Value {
    let choices: [fn(&mut Rand) -> Value; 10] = [
        |rand| Value::String(random_string(rand)),
        |rand| number((rand.next() * 2000.0).floor() - 1000.0),
        |rand| number(rand.next() * 100.0),
        |_| number(1e21),
        |_| number(-5e-8),
        |_| number(-0.0),
        |_| number(0.0),
        |_| Value::Bool(true),
        |_| Value::Bool(false),
        |_| Value::Null,
    ];
    let index = (rand.next() * choices.len() as f64).floor() as usize;
    choices[index](rand)
}

pub fn random_value(rand: &mut Rand, depth: usize) -> Value {
    let roll = rand.next();
    if depth > 3 || roll < 0.3 {
        return random_scalar(rand);
    }
    if roll < 0.65 {
        let mut members = Map::new();
        let count = (rand.next() * 4.0).floor() as usize;
        for _ in 0..count {
            let key = random_string(rand);
            let value = random_value(rand, depth + 1);
            members.insert(key, value);
        }
        return Value::Object(members);
    }
    let length = (rand.next() * 4.0).floor() as usize;
    Value::Array((0..length).map(|_| random_value(rand, depth + 1)).collect())
}

/// `JSON.stringify(value)`，`-0` 保留符号（TS 的 `render` 用 `Object.is(value, -0)` 特判）。
fn render_scalar(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some("null".to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        Value::String(text) => Some(kiana_json::js_object::stringify(&Value::String(
            text.clone(),
        ))),
        Value::Number(numeric) => {
            let value = numeric.as_f64().expect("numeric");
            if value == 0.0 && value.is_sign_negative() {
                Some("-0".to_string())
            } else {
                Some(kiana_json::js_num::format_js_number(value))
            }
        }
        _ => None,
    }
}

/// JSON text of `value`, with random insignificant whitespace when `spaced`
/// （求值顺序固定为：先 map/子渲染，再求分隔符；分隔符消耗随机数的时机决定
/// 了与固定种子的 golden 输出一致）。
pub fn render(value: &Value, rand: &mut Rand, spaced: bool) -> String {
    let space = |rand: &mut Rand| -> &'static str {
        if spaced {
            pick(rand, &["", "", " ", "\n  ", "\t", "\r\n"])
        } else {
            ""
        }
    };
    if let Some(text) = render_scalar(value) {
        return text;
    }
    match value {
        Value::Array(items) => {
            let leading = space(rand);
            let rendered: Vec<String> = items
                .iter()
                .map(|item| render(item, rand, spaced))
                .collect();
            let separator_left = space(rand);
            let separator_right = space(rand);
            let trailing = space(rand);
            format!(
                "[{leading}{}{trailing}]",
                rendered.join(&format!("{separator_left},{separator_right}"))
            )
        }
        Value::Object(members) => {
            let leading = space(rand);
            let mut rendered = Vec::with_capacity(members.len());
            for (key, item) in members {
                let key_json = kiana_json::js_object::stringify(&Value::String(key.clone()));
                let before = space(rand);
                let after = space(rand);
                let item_json = render(item, rand, spaced);
                rendered.push(format!("{key_json}{before}:{after}{item_json}"));
            }
            let separator_left = space(rand);
            let separator_right = space(rand);
            let trailing = space(rand);
            format!(
                "{{{leading}{}{trailing}}}",
                rendered.join(&format!("{separator_left},{separator_right}"))
            )
        }
        _ => unreachable!("scalars handled above"),
    }
}

pub fn random_documents(seed: i64, count: usize, spaced: bool) -> Vec<String> {
    let mut rand = Rand::new(seed as f64);
    (0..count)
        .map(|_| {
            let use_path = rand.next() < 0.8;
            let value = if use_path {
                let mut members = Map::new();
                let path = random_string(&mut rand);
                members.insert("path".to_string(), Value::String(path));
                let args = random_value(&mut rand, 0);
                members.insert("args".to_string(), args);
                Value::Object(members)
            } else {
                random_value(&mut rand, 0)
            };
            render(&value, &mut rand, spaced)
        })
        .collect()
}

/// JS `\s`（`/[ \t\n\r]/` 之外的空白集合）；用于 `/\s$/` 与 `SPACE` 判定。
pub fn is_js_space(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n' | '\u{000b}' | '\u{000c}' | '\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// 参照读取结果（TS: `Read`）。
struct Read {
    value: Value,
    complete: bool,
    end: usize,
}

/// TS 的 `referenceSnapshot(text, {pendingScalars})`：独立递归下降读取整个前缀。
pub fn reference_snapshot(text: &str, pending_scalars: bool) -> Option<Value> {
    let chars: Vec<char> = text.chars().collect();
    let start = skip_space(&chars, 0);
    read_value(&chars, start, pending_scalars).map(|read| read.value)
}

fn skip_space(chars: &[char], start: usize) -> usize {
    let mut index = start;
    while index < chars.len() && matches!(chars[index], ' ' | '\t' | '\n' | '\r') {
        index += 1;
    }
    index
}

fn read_value(chars: &[char], start: usize, pending_scalars: bool) -> Option<Read> {
    if start >= chars.len() {
        return None;
    }
    match chars[start] {
        '"' => Some(read_string(chars, start)),
        '{' => Some(read_object(chars, start, pending_scalars)),
        '[' => Some(read_array(chars, start, pending_scalars)),
        _ => read_scalar(chars, start, pending_scalars),
    }
}

fn simple_escape(ch: char) -> Option<char> {
    match ch {
        '"' => Some('"'),
        '\\' => Some('\\'),
        '/' => Some('/'),
        'b' => Some('\u{0008}'),
        'f' => Some('\u{000c}'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        _ => None,
    }
}

fn read_string(chars: &[char], start: usize) -> Read {
    // 以 UTF-16 码元累积后统一解码：`😀` 这类代理对按 JSON 字符串的码元语义还原；
    // 孤立代理项在 Rust 的 UTF-8 `String` 中无法表示，按 U+FFFD 处理。
    let mut units: Vec<u16> = Vec::new();
    let mut index = start + 1;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' {
            return Read {
                value: Value::String(String::from_utf16_lossy(&units)),
                complete: true,
                end: index + 1,
            };
        }
        if ch != '\\' {
            let mut buffer = [0u16; 2];
            units.extend_from_slice(ch.encode_utf16(&mut buffer));
            index += 1;
            continue;
        }
        let Some(&next) = chars.get(index + 1) else {
            return Read {
                value: Value::String(String::from_utf16_lossy(&units)),
                complete: false,
                end: chars.len(),
            };
        };
        if let Some(decoded) = simple_escape(next) {
            let mut buffer = [0u16; 2];
            units.extend_from_slice(decoded.encode_utf16(&mut buffer));
            index += 2;
            continue;
        }
        if next == 'u' {
            let mut end = index + 2;
            while end < index + 6 && end < chars.len() && chars[end].is_ascii_hexdigit() {
                end += 1;
            }
            if end == index + 6 {
                let digits: String = chars[index + 2..end].iter().collect();
                let code = u32::from_str_radix(&digits, 16).expect("four hex digits");
                units.push(code as u16);
                index = end;
                continue;
            }
            if end == chars.len() {
                return Read {
                    value: Value::String(String::from_utf16_lossy(&units)),
                    complete: false,
                    end,
                };
            }
            // 不是四位十六进制：转义保持字面量，后续字符照常读取。
            units.extend(chars[index..end].iter().map(|ch| *ch as u16));
            index = end;
            continue;
        }
        // 非法转义：反斜杠保持字面量，下一个字符照常读取。
        units.push('\\' as u16);
        index += 1;
    }
    Read {
        value: Value::String(String::from_utf16_lossy(&units)),
        complete: false,
        end: chars.len(),
    }
}

fn read_object(chars: &[char], start: usize, pending_scalars: bool) -> Read {
    let mut members = Map::new();
    let partial = |members: Map<String, Value>| Read {
        value: Value::Object(members),
        complete: false,
        end: chars.len(),
    };
    let mut index = skip_space(chars, start + 1);
    if index >= chars.len() {
        return partial(members);
    }
    if chars[index] == '}' {
        return Read {
            value: Value::Object(members),
            complete: true,
            end: index + 1,
        };
    }
    loop {
        if chars[index] != '"' {
            panic!(
                "reference reads valid JSON only: {}",
                chars.iter().collect::<String>()
            );
        }
        let key = read_string(chars, index);
        let key_text = match &key.value {
            Value::String(text) => text.clone(),
            _ => unreachable!(),
        };
        if !key.complete {
            return partial(members);
        }
        index = skip_space(chars, key.end);
        if index >= chars.len() {
            return partial(members);
        }
        index = skip_space(chars, index + 1);
        let Some(member) = read_value(chars, index, pending_scalars) else {
            return partial(members);
        };
        members.insert(key_text, member.value);
        if !member.complete {
            return partial(members);
        }
        index = skip_space(chars, member.end);
        if index >= chars.len() {
            return partial(members);
        }
        if chars[index] == '}' {
            return Read {
                value: Value::Object(members),
                complete: true,
                end: index + 1,
            };
        }
        index = skip_space(chars, index + 1);
        if index >= chars.len() {
            return partial(members);
        }
    }
}

fn read_array(chars: &[char], start: usize, pending_scalars: bool) -> Read {
    let mut elements: Vec<Value> = Vec::new();
    let partial = |elements: Vec<Value>| Read {
        value: Value::Array(elements),
        complete: false,
        end: chars.len(),
    };
    let mut index = skip_space(chars, start + 1);
    if index >= chars.len() {
        return partial(elements);
    }
    if chars[index] == ']' {
        return Read {
            value: Value::Array(elements),
            complete: true,
            end: index + 1,
        };
    }
    loop {
        let Some(element) = read_value(chars, index, pending_scalars) else {
            return partial(elements);
        };
        elements.push(element.value);
        if !element.complete {
            return partial(elements);
        }
        index = skip_space(chars, element.end);
        if index >= chars.len() {
            return partial(elements);
        }
        if chars[index] == ']' {
            return Read {
                value: Value::Array(elements),
                complete: true,
                end: index + 1,
            };
        }
        index = skip_space(chars, index + 1);
        if index >= chars.len() {
            return partial(elements);
        }
    }
}

/// 扫描 JSON number 的尾数 `-?(?:0|[1-9]\d*)(?:\.\d+)?`，返回尾数结束位置。
fn scan_mantissa(chars: &[char]) -> Option<usize> {
    let mut index = 0;
    if index < chars.len() && chars[index] == '-' {
        index += 1;
    }
    if index >= chars.len() {
        return None;
    }
    if chars[index] == '0' {
        index += 1;
    } else if chars[index].is_ascii_digit() {
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
    } else {
        return None;
    }
    if index < chars.len() && chars[index] == '.' {
        index += 1;
        if index >= chars.len() || !chars[index].is_ascii_digit() {
            return None;
        }
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
    }
    Some(index)
}

/// 完整 number 正则 `^-?(?:0|[1-9]\d*)(?:\.\d+)?(?:[eE][+-]?\d+)?$`（`\d` 仅 ASCII）。
fn full_number_value(chars: &[char]) -> Option<f64> {
    let mut index = scan_mantissa(chars)?;
    if index < chars.len() {
        if chars[index] != 'e' && chars[index] != 'E' {
            return None;
        }
        index += 1;
        if index < chars.len() && (chars[index] == '+' || chars[index] == '-') {
            index += 1;
        }
        if index >= chars.len() || !chars[index].is_ascii_digit() {
            return None;
        }
        while index < chars.len() && chars[index].is_ascii_digit() {
            index += 1;
        }
    }
    if index != chars.len() {
        return None;
    }
    let text: String = chars.iter().collect();
    Some(kiana_json::js_num::parse_js_number(&text))
}

/// `^(-?(?:0|[1-9]\d*)(?:\.\d+)?)[eE][+-]?$` 的捕获组值。
fn number_mantissa_value(chars: &[char]) -> Option<f64> {
    let mantissa_end = scan_mantissa(chars)?;
    let mut index = mantissa_end;
    if index >= chars.len() || (chars[index] != 'e' && chars[index] != 'E') {
        return None;
    }
    index += 1;
    if index < chars.len() && (chars[index] == '+' || chars[index] == '-') {
        index += 1;
    }
    if index != chars.len() {
        return None;
    }
    let text: String = chars[..mantissa_end].iter().collect();
    Some(kiana_json::js_num::parse_js_number(&text))
}

fn read_scalar(chars: &[char], start: usize, pending_scalars: bool) -> Option<Read> {
    let mut end = start;
    while end < chars.len()
        && !matches!(
            chars[end],
            ' ' | '\t' | '\n' | '\r' | ',' | ':' | '{' | '}' | '[' | ']' | '"'
        )
    {
        end += 1;
    }
    let token: Vec<char> = chars[start..end].to_vec();
    let token_text: String = token.iter().collect();
    if end < chars.len() {
        // TS 用 `JSON.parse(token)`；JS 的数字解析是正确舍入的，serde_json 的浮点解析在
        // 个别末尾数位上会有 1 ulp 偏差，因此走 kiana-json 的 JS 语义解析。
        let value: Value = kiana_json::js_parse::parse(&token_text)
            .expect("reference reads valid JSON scalars only");
        return Some(Read {
            value,
            complete: true,
            end,
        });
    }
    if !pending_scalars {
        return None;
    }
    for (literal, value) in [
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
    ] {
        if literal.starts_with(&token_text) {
            return Some(Read {
                value,
                complete: false,
                end,
            });
        }
    }
    if let Some(value) = full_number_value(&token) {
        return Some(Read {
            value: number(value),
            complete: false,
            end,
        });
    }
    number_mantissa_value(&token).map(|value| Read {
        value: number(value),
        complete: false,
        end,
    })
}

/// TS 的 `read32(text)`：按 UTF-16 码元算出的稳定种子，供随机 chunk size 使用。
pub fn read32(text: &str) -> u32 {
    let mut hash: i32 = 7;
    for unit in text.encode_utf16() {
        hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit));
    }
    // JS `Math.abs(hash) + 1`；`i32::MIN` 用 `unsigned_abs` 得到 2147483648。
    hash.unsigned_abs() + 1
}
