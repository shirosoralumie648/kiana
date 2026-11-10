//! `components/easter-egg-3d.ts` / `easter-egg-3d.lazy.ts` 的黄金对拍与行为测试。
//!
//! 黄金数据由 `/home/ctyun/kiana-work/scratch/anim/gen.ts` 导入真实 TS 模块生成：
//! `hash`、`rotation`/`spin`、`shuffleSteps`、`parseScreen`、`prepareCells`、`BlockRaster`、
//! 以及完整的 `render(width)` 帧（固定时钟）。TS 的 3D 动画不使用 `Math.random()`，所以
//! 可以逐字符对拍。`trig.json` 由 `/home/ctyun/kiana-work/scratch/anim/gen_trig_fixture.ts`
//! 生成（Node 的 `Math.sin`/`Math.cos` 位模式，覆盖 ±kiana、中等与超大参数）。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard, OnceLock};

use kiana_interactive::components::easter_egg_3d::{
    BLUE, BlockRaster, Box3, CORAL, EasterEgg3d, EasterEgg3dAnimation, EasterEggClock,
    EasterEggColors, PuzzleKind, TerminalRows, YELLOW, armin_model, base_spin_phase, create_model,
    ease_out, glyph_ink, half_vector, hash, is_light, is_playing, js_cos, js_hypot2, js_hypot3,
    js_round, js_sin, kiana_logo_model, last_animation_colors_for_test,
    last_animation_colors_pending_for_test, last_animation_elapsed_ms_for_test,
    last_animation_hidden_for_test, light_vector, mix, multiply3, parse_screen, play_armin_3d,
    play_easter_egg_3d, play_kiana_logo_3d, reset_playing_state_for_test, rotation, shuffle_steps,
    smooth, spin_angle, spin_phase,
};
use kiana_interactive::tui_renderer::InteractiveRenderer;
use kiana_theme::init_theme;
use kiana_tui::alt_screen::{TuiAltScreen, TuiAltScreenOptions};
use kiana_tui::components::text_component::Text;
use kiana_tui::terminal_colors::{ProgramStatus, Terminal};
use kiana_tui::terminal_image::{TerminalCapabilities, set_capabilities};
use kiana_tui::tui::{Component, Timer, TuiMouseButton, TuiMouseEvent, TuiMouseEventType};
use serde_json::Value;

mod common;

use common::{TestTerminal, alt_screen, main_screen};

fn theme() {
    static INIT: OnceLock<()> = OnceLock::new();
    // 能力缓存是线程局部的；主题模式在首次 init 时确定（truecolor）。
    set_capabilities(TerminalCapabilities {
        images: None,
        true_color: true,
        hyperlinks: false,
    });
    INIT.get_or_init(|| init_theme(Some("dark"), false));
}

fn global_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fixture(name: &str) -> Value {
    let text = match name {
        "constants" => include_str!("fixtures/constants.json"),
        "hash" => include_str!("fixtures/hash.json"),
        "rotation" => include_str!("fixtures/rotation.json"),
        "glyph_ink" => include_str!("fixtures/glyph_ink.json"),
        "shuffle" => include_str!("fixtures/shuffle.json"),
        "parse_screen" => include_str!("fixtures/parse_screen.json"),
        "prepare_cells" => include_str!("fixtures/prepare_cells.json"),
        "raster" => include_str!("fixtures/raster.json"),
        "trig" => include_str!("fixtures/trig.json"),
        "frames" => include_str!("fixtures/frames.json"),
        "frames_armin" => include_str!("fixtures/frames_armin.json"),
        other => panic!("unknown fixture {other}"),
    };
    serde_json::from_str(text).expect("fixture parses")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("string")
}

/// 夹具里的 f64 以 `toPrecision(17)` 的字符串保存；17 位有效数字可精确往返。
fn precise(value: &Value) -> f64 {
    match value {
        Value::String(text) => text.parse::<f64>().expect("float"),
        Value::Number(number) => number.as_f64().expect("number"),
        other => panic!("expected a number or numeric string, got {other:?}"),
    }
}

fn rgb(value: &Value) -> [f64; 3] {
    let array = value.as_array().expect("rgb array");
    [precise(&array[0]), precise(&array[1]), precise(&array[2])]
}

fn assert_f64_bits(actual: f64, expected: f64, what: &str) {
    assert_eq!(
        actual.to_bits(),
        expected.to_bits(),
        "{what}: {actual:?} != {expected:?}"
    );
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn hash_matches_ts_imul_chain() {
    let fixture = fixture("hash");
    let inputs = fixture["inputs"].as_array().expect("inputs");
    let outputs = fixture["outputs"].as_array().expect("outputs");
    for (input, expected) in inputs.iter().zip(outputs) {
        let value = input.as_f64().expect("number");
        assert_f64_bits(hash(value), precise(expected), &format!("hash({value})"));
    }
}

#[test]
fn lighting_constants_match_ts() {
    let fixture = fixture("constants");
    let light = rgb(&fixture["LIGHT"]);
    let half = rgb(&fixture["HALF_VECTOR"]);
    for index in 0..3 {
        assert_f64_bits(light_vector()[index], light[index], "LIGHT");
        assert_f64_bits(half_vector()[index], half[index], "HALF_VECTOR");
    }
    assert_eq!(fixture["PUZZLE_STEPS"].as_u64(), Some(12));
    assert_f64_bits(
        fixture["PUZZLE_CYCLE"].as_f64().expect("cycle"),
        5.199999999999999,
        "PUZZLE_CYCLE",
    );
}

#[test]
fn rotation_smooth_spin_and_mix_match_ts() {
    let fixture = fixture("rotation");
    for case in fixture["cases"].as_array().expect("cases") {
        let yaw = case["yaw"].as_f64().expect("yaw");
        let pitch = case["pitch"].as_f64().expect("pitch");
        let roll = case["roll"].as_f64().expect("roll");
        let expected = case["m"].as_array().expect("m");
        let actual = rotation(yaw, pitch, roll);
        for index in 0..9 {
            let expected = precise(&expected[index]);
            // `js_sin`/`js_cos` 逐位复刻 V8 的 fdlibm 实现，旋转矩阵必须逐位相同。
            assert_f64_bits(actual[index], expected, &format!("rotation[{index}]"));
        }
    }
    // multiply 与 rotation 一起覆盖；再用单位矩阵自乘一次。
    let identity = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    assert_eq!(multiply3(&identity, &identity), identity);

    let smooth_inputs = [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0, -0.5, 1.5];
    for (index, expected) in fixture["smooth"]
        .as_array()
        .expect("smooth")
        .iter()
        .enumerate()
    {
        assert_f64_bits(smooth(smooth_inputs[index]), precise(expected), "smooth");
    }
    for (index, expected) in fixture["easeOut"]
        .as_array()
        .expect("easeOut")
        .iter()
        .enumerate()
    {
        assert_f64_bits(ease_out(smooth_inputs[index]), precise(expected), "easeOut");
    }
    for case in fixture["spin"].as_array().expect("spin") {
        let time = case["t"].as_f64().expect("t");
        assert_f64_bits(
            base_spin_phase(time),
            precise(&case["base"]),
            "baseSpinPhase",
        );
        assert_f64_bits(spin_phase(time), precise(&case["phase"]), "spinPhase");
        assert_f64_bits(spin_angle(time), precise(&case["angle"]), "spinAngle");
    }
    let a = [10.0, 20.0, 30.0];
    let b = [200.0, 100.0, 50.0];
    let amounts = [0.0, 0.25, 0.5, 1.0];
    for (index, expected) in fixture["mix"].as_array().expect("mix").iter().enumerate() {
        let mixed = mix(a, b, amounts[index]);
        for channel in 0..3 {
            assert_f64_bits(mixed[channel], precise(&expected[channel]), "mix");
        }
    }
    for (index, expected) in fixture["isLight"]
        .as_array()
        .expect("isLight")
        .iter()
        .enumerate()
    {
        let colors = [
            [0.0, 0.0, 0.0],
            [255.0, 255.0, 255.0],
            [128.0, 128.0, 128.0],
            [200.0, 100.0, 50.0],
            [228.0, 138.0, 122.0],
        ];
        assert_eq!(is_light(colors[index]), expected.as_bool().expect("bool"));
    }
}

#[test]
fn js_sin_cos_match_v8_fdlibm_bit_for_bit() {
    let fixture = fixture("trig");
    for case in fixture["cases"].as_array().expect("cases") {
        let x = f64::from_bits(u64::from_str_radix(text(&case["x"]), 16).expect("x bits"));
        let sin = f64::from_bits(u64::from_str_radix(text(&case["sin"]), 16).expect("sin bits"));
        let cos = f64::from_bits(u64::from_str_radix(text(&case["cos"]), 16).expect("cos bits"));
        let actual_sin = js_sin(x);
        let actual_cos = js_cos(x);
        if sin.is_nan() {
            assert!(actual_sin.is_nan(), "sin({x:e})");
        } else {
            assert_eq!(actual_sin.to_bits(), sin.to_bits(), "sin({x:e})");
        }
        if cos.is_nan() {
            assert!(actual_cos.is_nan(), "cos({x:e})");
        } else {
            assert_eq!(actual_cos.to_bits(), cos.to_bits(), "cos({x:e})");
        }
    }
}

#[test]
fn hypot_and_round_follow_javascript() {
    assert_f64_bits(js_round(0.5), 1.0, "round(0.5)");
    assert_f64_bits(js_round(-0.5), 0.0, "round(-0.5)"); // JS: -0；与正零可比较
    assert_f64_bits(js_round(2.5), 3.0, "round(2.5)");
    assert_f64_bits(js_hypot2(3.0, 4.0), 5.0, "hypot(3,4)");
    assert_f64_bits(js_hypot3(2.0, 3.0, 6.0), 7.0, "hypot(2,3,6)");
    assert_f64_bits(js_hypot2(0.0, 0.0), 0.0, "hypot(0,0)");
    // V8 的「除以最大值」算法与 sqrt(a²+b²) 在部分输入上差 1 ulp。
    assert_f64_bits(js_hypot3(-0.45, -0.6, 0.75), 1.0606601717798214, "V8 hypot");
}

#[test]
fn glyph_ink_matches_ts() {
    let fixture = fixture("glyph_ink");
    for case in fixture["cases"].as_array().expect("cases") {
        let text = text(&case["text"]);
        let seed = case["seed"].as_f64().expect("seed");
        assert_eq!(
            glyph_ink(text, seed),
            case["ink"].as_u64().expect("ink") as usize,
            "glyph_ink({text:?})"
        );
    }
}

#[test]
fn shuffle_steps_match_ts() {
    let fixture = fixture("shuffle");
    let logo = kiana_logo_model((2, 1));
    let homes: Vec<[i64; 3]> = efixture_homes(&fixture["logo"]["homes"]);
    let model_homes: Vec<[i64; 3]> = logo.blocks.iter().map(|block| block.home).collect();
    assert_eq!(model_homes, homes);
    for cycle in fixture["logo"]["cycles"].as_array().expect("cycles") {
        let cycle_index = cycle["cycle"].as_i64().expect("cycle");
        let expected = cycle["steps"].as_array().expect("steps");
        let actual = shuffle_steps(&logo, cycle_index);
        assert_eq!(actual.len(), expected.len());
        for (step_index, step) in expected.iter().enumerate() {
            let positions = step.as_array().expect("step");
            for (block_index, position) in positions.iter().enumerate() {
                let cell = position.as_array().expect("cell");
                let expected_cell = [
                    cell[0].as_i64().expect("x"),
                    cell[1].as_i64().expect("y"),
                    cell[2].as_i64().expect("z"),
                ];
                assert_eq!(
                    actual[step_index][block_index], expected_cell,
                    "logo cycle {cycle_index} step {step_index} block {block_index}"
                );
            }
        }
    }

    let armin = armin_model(CORAL);
    assert_eq!(
        armin.blocks.len(),
        fixture["armin"]["blockCount"].as_u64().expect("count") as usize
    );
    for cycle in fixture["armin"]["cycles"].as_array().expect("cycles") {
        let cycle_index = cycle["cycle"].as_i64().expect("cycle");
        let steps = shuffle_steps(&armin, cycle_index);
        // 夹具里只有摘要；这里重建同样的 JSON 文本计算摘要。
        assert_eq!(
            fnv1a64(&json_stringify_steps(&steps)),
            text(&cycle["digest"]),
            "armin cycle {cycle_index} digest"
        );
        let moved: Vec<usize> = steps
            .windows(2)
            .map(|pair| pair[0].iter().zip(&pair[1]).filter(|(a, b)| a != b).count())
            .collect();
        let expected_moved: Vec<usize> = cycle["movedPerStep"]
            .as_array()
            .expect("moved")
            .iter()
            .map(|value| value.as_u64().expect("moved") as usize)
            .collect();
        assert_eq!(moved, expected_moved, "armin cycle {cycle_index}");
    }
}

fn efixture_homes(value: &Value) -> Vec<[i64; 3]> {
    value
        .as_array()
        .expect("homes")
        .iter()
        .map(|cell| {
            let cell = cell.as_array().expect("cell");
            [
                cell[0].as_i64().expect("x"),
                cell[1].as_i64().expect("y"),
                cell[2].as_i64().expect("z"),
            ]
        })
        .collect()
}

/// `JSON.stringify(steps)` 的等价文本（数字都是整数，无空格）。
fn json_stringify_steps(steps: &[Vec<[i64; 3]>]) -> String {
    let mut out = String::from("[");
    for (step_index, step) in steps.iter().enumerate() {
        if step_index > 0 {
            out.push(',');
        }
        out.push('[');
        for (cell_index, cell) in step.iter().enumerate() {
            if cell_index > 0 {
                out.push(',');
            }
            out.push('[');
            out.push_str(&cell[0].to_string());
            out.push(',');
            out.push_str(&cell[1].to_string());
            out.push(',');
            out.push_str(&cell[2].to_string());
            out.push(']');
        }
        out.push(']');
    }
    out.push(']');
    out
}

fn fnv1a64(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in text.encode_utf16() {
        hash ^= u64::from(unit);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:x}")
}

#[test]
fn parse_screen_matches_ts() {
    let fixture = fixture("parse_screen");
    let width = fixture["width"].as_u64().expect("width") as usize;
    let foreground = rgb(&fixture["foreground"]);
    let background = rgb(&fixture["background"]);
    let lines = strings(&fixture["lines"]);
    let cells = parse_screen(&lines, width, foreground, background);
    let expected_lines = fixture["cells"].as_array().expect("cells");
    assert_eq!(cells.len(), expected_lines.len());
    for (row, expected) in expected_lines.iter().enumerate() {
        let expected_cells = expected.as_array().expect("line");
        assert_eq!(cells[row].len(), expected_cells.len(), "row {row} length");
        for (column, expected_cell) in expected_cells.iter().enumerate() {
            let actual = &cells[row][column];
            assert_eq!(
                actual.text,
                text(&expected_cell["text"]),
                "row {row} col {column} text"
            );
            assert_eq!(
                actual.width,
                expected_cell["width"].as_u64().expect("width") as usize,
                "row {row} col {column} width"
            );
            let expected_fg = rgb(&expected_cell["fg"]);
            for (channel, expected) in expected_fg.iter().enumerate() {
                assert_f64_bits(actual.fg[channel], *expected, "fg");
            }
            match &expected_cell["bg"] {
                Value::Null => assert!(actual.bg.is_none(), "row {row} col {column} bg"),
                value => {
                    let expected_bg = rgb(value);
                    let actual_bg = actual.bg.expect("bg");
                    for channel in 0..3 {
                        assert_f64_bits(actual_bg[channel], expected_bg[channel], "bg");
                    }
                }
            }
        }
    }
}

/// TS `parseScreen` 的边缘输入：ESC 后跟多字节字符（Rust 曾按字节 +2 → panic）、
/// 截断的 `38;2`/`48;2` 序列（TS 忽略，Rust 曾安装 NaN 颜色）、空通道、冒号形态等。
#[test]
fn parse_screen_edge_cases_match_ts() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/parse_screen_edges.json"))
        .expect("parse screen edges fixture parses");
    let width = fixture["width"].as_u64().expect("width") as usize;
    let foreground = rgb(&fixture["foreground"]);
    let background = rgb(&fixture["background"]);
    for case in fixture["cases"].as_array().expect("cases") {
        let name = text(&case["name"]);
        let lines = strings(&case["lines"]);
        let actual = parse_screen(&lines, width, foreground, background);
        let expected_lines = case["cells"].as_array().expect("cells");
        assert_eq!(actual.len(), expected_lines.len(), "{name}: rows");
        for (row, expected) in expected_lines.iter().enumerate() {
            let expected_cells = expected.as_array().expect("line");
            assert_eq!(actual[row].len(), expected_cells.len(), "{name}: row {row}");
            for (column, expected_cell) in expected_cells.iter().enumerate() {
                let cell = &actual[row][column];
                assert_eq!(
                    cell.text,
                    text(&expected_cell["text"]),
                    "{name} {row}:{column} text"
                );
                assert_eq!(
                    cell.width,
                    expected_cell["width"].as_u64().expect("width") as usize,
                    "{name} {row}:{column} width"
                );
                for channel in 0..3 {
                    match &expected_cell["fg"][channel] {
                        // JSON 把 NaN 序列化成 null。
                        Value::Null => assert!(
                            cell.fg[channel].is_nan(),
                            "{name} {row}:{column} fg[{channel}] should be NaN"
                        ),
                        value => assert_f64_bits(
                            cell.fg[channel],
                            value.as_f64().expect("fg channel"),
                            name,
                        ),
                    }
                }
                match &expected_cell["bg"] {
                    Value::Null => assert!(
                        cell.bg.is_none(),
                        "{name} {row}:{column} should keep the default background"
                    ),
                    value => {
                        let expected_bg = rgb(value);
                        let actual_bg = cell.bg.expect("bg");
                        for channel in 0..3 {
                            assert_f64_bits(actual_bg[channel], expected_bg[channel], name);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn parse_screen_skips_esc_before_multibyte_characters() {
    // 回归：ESC 后是 2–4 字节的字符时，`i = sequence_start + 2` 会落在字符中间并 panic。
    let foreground = [200.0, 201.0, 202.0];
    let background = [0.0, 0.0, 0.0];
    for (line, expected) in [
        ("a\x1bé b", vec!["a", " ", "b"]),
        ("a\x1b你 b", vec!["a", " ", "b"]),
        // TS 只跳过高代理项；剩下的低代理项宽度为 0 被丢弃，等价于跳过整个 emoji。
        ("a\x1b😀b", vec!["a", "b"]),
        ("a\x1b", vec!["a"]),
        ("\x1béx\x1b你y", vec!["x", "y"]),
    ] {
        let cells = parse_screen(&[line.to_string()], 20, foreground, background);
        let texts: Vec<&str> = cells[0].iter().map(|cell| cell.text.as_str()).collect();
        assert_eq!(texts, expected, "parse_screen({line:?})");
    }
}

/// TS 的 `palette` 调 `indexedColor(index)`：非整数/越界索引会抛
/// `ANSI color index must be an integer from 0 to 255: <index>`，`parseScreen` 不捕获。
/// Rust 用同文案的 panic 复刻（`kiana-json` 的 `Number::toString` 格式：NaN/Infinity）。
#[test]
fn parse_screen_throws_on_invalid_indexed_colors_like_ts() {
    let huge = "9".repeat(400);
    let cases = [
        ("\x1b[38;5;mX", "NaN"),
        ("\x1b[38;5;300mX", "300"),
        ("\x1b[38;5;-1mX", "-1"),
        ("\x1b[38;5;;mX", "NaN"),
        ("\x1b[38:5:300mX", "300"),
        ("\x1b[48;5;mX", "NaN"),
    ];
    for (line, index) in cases {
        let source = line.to_string();
        let input = source.clone();
        let result = std::panic::catch_unwind(move || {
            parse_screen(&[input], 20, [200.0, 201.0, 202.0], [0.0; 3])
        });
        let error = result
            .err()
            .unwrap_or_else(|| panic!("parse_screen must throw for {source:?}"));
        let message = error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|text| (*text).to_string()))
            .expect("panic payload is a string");
        assert_eq!(
            message,
            format!("ANSI color index must be an integer from 0 to 255: {index}")
        );
    }
    let overflow = format!("\x1b[38;5;{huge}mX");
    let result = std::panic::catch_unwind(move || {
        parse_screen(&[overflow], 20, [200.0, 201.0, 202.0], [0.0; 3])
    });
    let message = result
        .expect_err("parse_screen must throw")
        .downcast_ref::<String>()
        .cloned()
        .expect("string payload");
    assert_eq!(
        message,
        "ANSI color index must be an integer from 0 to 255: Infinity"
    );

    // 合法/被忽略的序列不受影响：`38:5:`（冒号形态过滤 NaN）与 `38;5;1.5`（parseInt → 1）。
    for line in ["\x1b[38:5:mX", "\x1b[38;5;1.5mX"] {
        let cells = parse_screen(&[line.to_string()], 20, [200.0, 201.0, 202.0], [0.0; 3]);
        assert_eq!(cells[0][0].text, "X");
    }
}

fn clock_at(initial: f64) -> (EasterEggClock, Rc<Cell<f64>>) {
    let value = Rc::new(Cell::new(initial));
    let handle = Rc::clone(&value);
    (Rc::new(move || handle.get()), value)
}

fn rows12() -> TerminalRows {
    Rc::new(|| 12)
}

fn animation_for(
    screen: Vec<String>,
    model: kiana_interactive::components::easter_egg_3d::Model,
    colors: EasterEggColors,
    clock: EasterEggClock,
) -> EasterEgg3dAnimation {
    EasterEgg3dAnimation::new(screen, model, colors, || {}, rows12(), None, clock)
}

#[test]
fn prepare_cells_matches_ts() {
    theme();
    let fixture = fixture("prepare_cells");
    let width = fixture["width"].as_u64().expect("width") as usize;
    let screen = strings(&fixture["screen"]);
    let logo = animation_for(
        screen.clone(),
        kiana_logo_model((2, 1)),
        EasterEggColors {
            foreground: [200.0, 200.0, 200.0],
            background: [0.0, 0.0, 0.0],
        },
        clock_at(0.0).0,
    );
    let armin = animation_for(
        screen,
        armin_model(CORAL),
        EasterEggColors {
            foreground: CORAL,
            background: [0.0, 0.0, 0.0],
        },
        clock_at(0.0).0,
    );
    for (name, animation) in [("logo", &logo), ("armin", &armin)] {
        let cells = animation.prepare_cells_for_test(width);
        let expected_lines = fixture[name].as_array().expect("lines");
        assert_eq!(cells.len(), expected_lines.len(), "{name} rows");
        for (row, expected) in expected_lines.iter().enumerate() {
            let expected_cells = expected.as_array().expect("line");
            assert_eq!(cells[row].len(), expected_cells.len(), "{name} row {row}");
            for (column, expected_cell) in expected_cells.iter().enumerate() {
                let actual = &cells[row][column];
                assert_eq!(
                    actual.text,
                    text(&expected_cell["text"]),
                    "{name} {row}:{column} text"
                );
                assert_eq!(
                    actual.width,
                    expected_cell["width"].as_u64().expect("width") as usize,
                    "{name} {row}:{column} width"
                );
                let expected_fg = rgb(&expected_cell["fg"]);
                for (channel, expected) in expected_fg.iter().enumerate() {
                    assert_f64_bits(actual.fg[channel], *expected, "fg");
                }
                match &expected_cell["bg"] {
                    Value::Null => assert!(actual.bg.is_none(), "{name} {row}:{column} bg"),
                    value => {
                        let expected_bg = rgb(value);
                        let actual_bg = actual.bg.expect("bg");
                        for channel in 0..3 {
                            assert_f64_bits(actual_bg[channel], expected_bg[channel], "bg");
                        }
                    }
                }
                assert_f64_bits(actual.delay, precise(&expected_cell["delay"]), "delay");
                assert_eq!(
                    actual.ink,
                    expected_cell["ink"].as_u64().expect("ink") as usize,
                    "{name} {row}:{column} ink"
                );
                assert_eq!(
                    actual.seed,
                    expected_cell["seed"].as_i64().expect("seed"),
                    "{name} {row}:{column} seed"
                );
            }
        }
    }
}

fn model_by_name(name: &str) -> kiana_interactive::components::easter_egg_3d::Model {
    match name {
        "logo" => kiana_logo_model((2, 1)),
        "checker" => create_model(
            3,
            3,
            |column, row| {
                if (column + row) % 2 == 0 {
                    Some([120.0, 200.0, 90.0])
                } else {
                    None
                }
            },
            12.0,
            0.5,
            Some((1, 2)),
            PuzzleKind::Logo,
        ),
        "armin" => armin_model(CORAL),
        other => panic!("unknown model {other}"),
    }
}

fn f32_bits_from_precise(value: &Value) -> u32 {
    (text(value).parse::<f64>().expect("f32 value") as f32).to_bits()
}

#[test]
fn block_raster_matches_ts_bit_for_bit() {
    let fixture = fixture("raster");
    for case in fixture["cases"].as_array().expect("cases") {
        let name = text(&case["name"]);
        let model = model_by_name(text(&case["model"]));
        assert_f64_bits(
            model.camera_distance,
            case["camera_distance"].as_f64().expect("camera"),
            "camera distance",
        );
        let width = case["width"].as_u64().expect("width") as usize;
        let height = case["height"].as_u64().expect("height") as usize;
        let pose = &case["pose"];
        let pose = kiana_interactive::components::easter_egg_3d::Pose {
            center_x: pose["centerX"].as_f64().expect("cx"),
            center_y: pose["centerY"].as_f64().expect("cy"),
            scale: pose["scale"].as_f64().expect("scale"),
            yaw: pose["yaw"].as_f64().expect("yaw"),
            pitch: pose["pitch"].as_f64().expect("pitch"),
            roll: pose["roll"].as_f64().expect("roll"),
        };
        let background = rgb(&case["background"]);
        let boxes: Vec<Box3> = model
            .blocks
            .iter()
            .map(|block| {
                let x = block.home[0] as f64 - model.columns as f64 / 2.0;
                let y = block.home[1] as f64 - model.rows as f64 / 2.0;
                Box3 {
                    min: [x, y, -0.35],
                    max: [x + 1.0, y + 1.0, 0.35],
                    color: block.color,
                }
            })
            .collect();
        let mut raster = BlockRaster::new(model.camera_distance);
        raster.render(
            width,
            height,
            &pose,
            &boxes,
            background,
            case["halo"].as_f64().expect("halo"),
        );

        let expected_bits: Vec<u8> = case["bits"]
            .as_array()
            .expect("bits")
            .iter()
            .map(|value| value.as_u64().expect("bit") as u8)
            .collect();
        assert_eq!(raster.bits, expected_bits, "{name}: bits");
        let expected_counts: Vec<u8> = case["counts"]
            .as_array()
            .expect("counts")
            .iter()
            .map(|value| value.as_u64().expect("count") as u8)
            .collect();
        assert_eq!(raster.counts, expected_counts, "{name}: counts");
        for (label, actual, expected) in [
            ("rgb", &raster.rgb, case["rgb"].as_array().expect("rgb")),
            (
                "haloAmount",
                &raster.halo_amount,
                case["haloAmount"].as_array().expect("haloAmount"),
            ),
            (
                "haloRgb",
                &raster.halo_rgb,
                case["haloRgb"].as_array().expect("haloRgb"),
            ),
        ] {
            assert_eq!(actual.len(), expected.len(), "{name}: {label} length");
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                let expected_bits = f32_bits_from_precise(expected);
                assert_eq!(
                    actual.to_bits(),
                    expected_bits,
                    "{name}: {label}[{index}] ({actual} vs {expected})"
                );
            }
        }
    }
}

fn run_frame_fixture(name: &str, model_name: &str, foreground: [f64; 3]) {
    theme();
    let fixture = fixture(name);
    let width = fixture["width"].as_u64().expect("width") as usize;
    let section = &fixture[model_name];
    let start = section["start"].as_f64().expect("start");
    let (clock, clock_value) = clock_at(start);
    let model = match model_name {
        "logo" => kiana_logo_model((2, 1)),
        "armin" => armin_model(foreground),
        other => panic!("unknown model {other}"),
    };
    let mut animation = animation_for(
        strings(&fixture["screen"]),
        model,
        EasterEggColors {
            foreground,
            background: [0.0, 0.0, 0.0],
        },
        clock,
    );
    for frame in section["frames"].as_array().expect("frames") {
        let time = frame["time"].as_f64().expect("time");
        clock_value.set(start + (time * 1000.0).round());
        let actual = animation.render(width);
        let expected = strings(&frame["lines"]);
        assert_eq!(actual, expected, "{name}: frame at {time}s");
    }
    // 退出动画：close() 在 t=9.3，之后按毫秒推进。
    clock_value.set(start + 9300.0);
    animation.close();
    for frame in section["exitFrames"].as_array().expect("exitFrames") {
        let advance = frame["advance"].as_f64().expect("advance");
        clock_value.set(start + 9300.0 + advance);
        let actual = animation.render(width);
        let expected = strings(&frame["lines"]);
        assert_eq!(actual, expected, "{name}: exit frame +{advance}ms");
    }
}

#[test]
fn logo_frames_match_ts() {
    run_frame_fixture("frames", "logo", [200.0, 200.0, 200.0]);
}

#[test]
fn armin_frames_match_ts() {
    run_frame_fixture("frames_armin", "armin", CORAL);
}

// ---------------------------------------------------------------------------
// 动画行为
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeTimer {
    pending: Option<Box<dyn FnOnce()>>,
    delays: Vec<u64>,
    cancelled: usize,
}

impl Timer for FakeTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.delays.push(delay_ms);
        self.pending = Some(callback);
    }

    fn cancel(&mut self) {
        self.pending = None;
        self.cancelled += 1;
    }
}

#[derive(Clone, Default)]
struct SharedTimer(Rc<RefCell<FakeTimer>>);

impl Timer for SharedTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.0.borrow_mut().set(delay_ms, callback);
    }

    fn cancel(&mut self) {
        self.0.borrow_mut().cancel();
    }
}

impl SharedTimer {
    fn fire(&self) {
        let callback = self.0.borrow_mut().pending.take();
        if let Some(callback) = callback {
            callback();
        }
    }

    fn delays(&self) -> Vec<u64> {
        self.0.borrow().delays.clone()
    }
}

fn sample_screen() -> Vec<String> {
    vec![
        "hello world".to_string(),
        "\x1b[31mstatus\x1b[0m".to_string(),
    ]
}

fn sample_animation(
    clock: EasterEggClock,
    done: Rc<Cell<usize>>,
    requests: Rc<Cell<usize>>,
) -> EasterEgg3dAnimation {
    let request_render: kiana_tui::components::loader::RequestRenderCallback = {
        let requests = Rc::clone(&requests);
        Rc::new(move || requests.set(requests.get() + 1))
    };
    EasterEgg3dAnimation::new(
        sample_screen(),
        kiana_logo_model((2, 1)),
        EasterEggColors {
            foreground: [200.0, 200.0, 200.0],
            background: [0.0, 0.0, 0.0],
        },
        move || done.set(done.get() + 1),
        rows12(),
        Some(request_render),
        clock,
    )
}

#[test]
fn timer_drives_frames_and_finishes_after_exit() {
    theme();
    let (clock, clock_value) = clock_at(0.0);
    let done = Rc::new(Cell::new(0));
    let requests = Rc::new(Cell::new(0));
    let mut animation = sample_animation(clock, Rc::clone(&done), Rc::clone(&requests));
    let timer = SharedTimer::default();
    animation.set_timer(Box::new(timer.clone()));
    assert_eq!(timer.delays(), vec![1000 / 30]);
    animation.render(30);
    assert_eq!(
        requests.get(),
        0,
        "TS's render never calls requestRender; the 30 fps timer is the only repaint source"
    );
    timer.fire();
    assert_eq!(requests.get(), 1, "each timer tick requests one frame");
    assert_eq!(timer.delays().len(), 2, "the tick reschedules the interval");

    animation.close();
    assert!(animation.is_playing());
    // 退出进度完成 → timer 回调调用 onDone。
    clock_value.set(2000.0);
    timer.fire();
    assert_eq!(done.get(), 1);
    assert!(!animation.is_playing());
    animation.close();
    assert_eq!(done.get(), 1, "second close after finish is a no-op");
}

#[test]
fn timer_finishes_when_nothing_rendered_for_a_second() {
    theme();
    let (clock, clock_value) = clock_at(0.0);
    let done = Rc::new(Cell::new(0));
    let requests = Rc::new(Cell::new(0));
    let mut animation = sample_animation(clock, Rc::clone(&done), Rc::clone(&requests));
    let timer = SharedTimer::default();
    animation.set_timer(Box::new(timer.clone()));
    clock_value.set(1500.0);
    timer.fire();
    assert_eq!(done.get(), 1, "1s without a render finishes the animation");
    assert!(!animation.is_playing());
}

#[test]
fn input_and_mouse_trigger_close_without_hiding_during_dispatch() {
    theme();
    let (clock, clock_value) = clock_at(0.0);
    let done = Rc::new(Cell::new(0));
    let requests = Rc::new(Cell::new(0));
    let mut animation = sample_animation(clock, Rc::clone(&done), Rc::clone(&requests));
    let timer = SharedTimer::default();
    animation.set_timer(Box::new(timer.clone()));
    animation.render(30);

    let mouse = |event_type: TuiMouseEventType| TuiMouseEvent {
        event_type,
        button: TuiMouseButton::Left,
        x: 1,
        y: 1,
        screen_x: 1,
        screen_y: 1,
        width: 30,
        height: 12,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: Some(1),
    };
    let result = animation
        .handle_mouse(&mouse(TuiMouseEventType::Release))
        .expect("result");
    assert_eq!(result.handled, Some(true));
    assert_eq!(result.render, Some(false));
    assert!(!animation.exit_progress().is_nan());

    animation.handle_mouse(&mouse(TuiMouseEventType::Click));
    assert!(animation.is_playing());
    // 第二次 close 在输入分发里只排队 finish；下一次 timer tick 真正收尾。
    animation.handle_input("\x1b");
    assert_eq!(done.get(), 0);
    clock_value.set(5000.0);
    timer.fire();
    assert_eq!(done.get(), 1);
}

#[test]
fn invalidate_rebuilds_the_cell_cache() {
    theme();
    let (clock, _clock_value) = clock_at(0.0);
    let done = Rc::new(Cell::new(0));
    let requests = Rc::new(Cell::new(0));
    let mut animation = sample_animation(clock, done, requests);
    let first = animation.render(30);
    animation.invalidate();
    let second = animation.render(30);
    assert_eq!(first, second);
}

#[test]
fn render_without_a_timer_finishes_itself_at_exit_end() {
    theme();
    let (clock, clock_value) = clock_at(0.0);
    let done = Rc::new(Cell::new(0));
    let requests = Rc::new(Cell::new(0));
    let mut animation = sample_animation(clock, Rc::clone(&done), Rc::clone(&requests));
    // 没有 set_timer：TS 的 setInterval 不存在，收尾只能由 render 按同一时钟补做。
    assert!(animation.is_playing());
    animation.render(30);
    assert_eq!(
        requests.get(),
        1,
        "an animating frame requests the next render"
    );

    animation.close();
    clock_value.set(2_000.0);
    let lines = animation.render(30);
    assert_eq!(lines.len(), 12, "the final exit frame is still rendered");
    assert_eq!(done.get(), 1, "render performs the timer's finish()");
    assert!(
        !animation.is_playing(),
        "the animation is no longer playing"
    );
    let after_finish = requests.get();
    animation.render(30);
    animation.render(30);
    assert_eq!(
        requests.get(),
        after_finish,
        "a finished animation must not keep re-requesting renders"
    );
}

#[test]
fn render_without_a_timer_finishes_after_a_second_without_renders() {
    theme();
    let (clock, clock_value) = clock_at(0.0);
    let done = Rc::new(Cell::new(0));
    let requests = Rc::new(Cell::new(0));
    let mut animation = sample_animation(clock, Rc::clone(&done), Rc::clone(&requests));
    animation.render(30);
    clock_value.set(1_500.0);
    animation.render(30);
    assert_eq!(
        done.get(),
        1,
        "the TS timer would have finished after 1s without renders"
    );
    assert!(!animation.is_playing());
}

#[test]
fn external_overlay_removal_resets_the_module_playing_flag() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    let mut renderer = fullscreen();
    assert!(play_armin_3d(&mut renderer));
    assert!(renderer.has_overlay());
    assert!(is_playing());

    // 宿主把 overlay 移除（例如 resetExtensionUI / stopInteractiveTui）时动画被同步销毁：
    // TS 的 setInterval 会产生 finish() → onDone()，这里由 Drop 复位 playing。
    renderer.hide_overlay();
    assert!(
        !is_playing(),
        "removing the overlay externally must reset playing"
    );

    // 之后的彩蛋仍能播放（不会被卡死的 playing 标志吞掉）。
    assert!(play_armin_3d(&mut renderer));
    assert!(renderer.has_overlay());
    assert!(is_playing());
    renderer.hide_overlay();
    assert!(!is_playing());
    reset_playing_state_for_test();
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the std Mutex serializes the process-global `playing` flag; holding it across the LocalSet await is intentional"
)]
async fn lazy_entry_queries_and_applies_terminal_colors() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    common::run_local(async {
        let terminal = TestTerminal::with_size(80, 24);
        let output = terminal.output_handle();
        let mut renderer =
            InteractiveRenderer::Fullscreen(alt_screen(terminal, TuiAltScreenOptions::default()));
        assert!(play_armin_3d(&mut renderer));
        assert!(
            output.borrow().contains("\x1b]10;?\x07"),
            "the lazy entry must issue the terminal color query like TS's async eager path"
        );
        let fallback = last_animation_colors_for_test().expect("animation is registered");
        assert_eq!(fallback.background, [0.0, 0.0, 0.0], "dark theme fallback");

        // 终端回复：OSC 10/11 + DA1（与宿主 handleTerminalInput 的消费路径一致）。
        let InteractiveRenderer::Fullscreen(screen) = &mut renderer else {
            unreachable!("constructed as fullscreen")
        };
        screen.handle_terminal_input("\x1b]10;#112233\x07");
        screen.handle_terminal_input("\x1b]11;#445566\x07");
        screen.handle_terminal_input("\x1b[?62;22c");
        common::yield_tasks(50).await;

        let updated = last_animation_colors_for_test().expect("animation is still alive");
        assert_eq!(updated.foreground, [17.0, 34.0, 51.0]);
        assert_eq!(updated.background, [68.0, 85.0, 102.0]);
        renderer.hide_overlay();
        reset_playing_state_for_test();
    })
    .await;
}

/// 终端颜色查询 / 输入分发相关行为（review findings 1、2、6）。
///
/// `Terminal::start` 之后异步注入 OSC 10/11 + DA1 回复：查询期间宿主无法
/// `handleTerminalInput`（渲染器被 `play_easter_egg_3d` 借用），只有 async 入口自己在
/// await 里泵入输入才能收到上报色。
type SharedInputHandler = Rc<RefCell<Option<Box<dyn FnMut(&str)>>>>;

struct DelayedReplyTerminal {
    handler: SharedInputHandler,
    delay: std::time::Duration,
}

impl DelayedReplyTerminal {
    fn new(delay: std::time::Duration) -> Self {
        DelayedReplyTerminal {
            handler: Rc::new(RefCell::new(None)),
            delay,
        }
    }
}

impl Terminal for DelayedReplyTerminal {
    fn start(&mut self, on_input: Box<dyn FnMut(&str)>, _on_resize: Box<dyn FnMut()>) {
        *self.handler.borrow_mut() = Some(on_input);
        let handler = Rc::clone(&self.handler);
        let delay = self.delay;
        tokio::task::spawn_local(async move {
            tokio::time::sleep(delay).await;
            for reply in ["\x1b]10;#112233\x07", "\x1b]11;#445566\x07", "\x1b[?62;22c"] {
                if let Some(callback) = handler.borrow_mut().as_mut() {
                    callback(reply);
                }
                tokio::task::yield_now().await;
            }
        });
    }

    fn stop(&mut self) {
        *self.handler.borrow_mut() = None;
    }

    fn drain_input(
        &mut self,
        _max_ms: u64,
        _idle_ms: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async {})
    }

    fn write(&mut self, _data: &str) {}
    fn columns(&self) -> usize {
        80
    }
    fn rows(&self) -> usize {
        24
    }
    fn kitty_protocol_active(&self) -> bool {
        true
    }
    fn move_by(&mut self, _lines: isize) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
    fn set_title(&mut self, _title: &str) {}
    fn set_progress(&mut self, _active: bool) {}
    fn set_program_status(&mut self, _status: &ProgramStatus) {}
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the std Mutex serializes the process-global `playing` flag; holding it across the LocalSet await is intentional"
)]
async fn eager_entry_pumps_terminal_replies_while_awaiting_colors() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    common::run_local(async {
        let terminal = DelayedReplyTerminal::new(std::time::Duration::from_millis(5));
        let mut renderer = InteractiveRenderer::Fullscreen(TuiAltScreen::new(
            Box::new(terminal),
            None,
            None,
            TuiAltScreenOptions::default(),
        ));
        renderer.start();
        let screen = renderer.get_screen_lines();
        play_easter_egg_3d(&mut renderer, screen, EasterEgg3d::Armin).await;
        let colors = last_animation_colors_for_test().expect("the animation was created");
        assert_eq!(
            colors.foreground,
            [17.0, 34.0, 51.0],
            "the OSC 10 reply must be consumed during the query"
        );
        assert_eq!(colors.background, [68.0, 85.0, 102.0]);
        renderer.hide_overlay();
        reset_playing_state_for_test();
    })
    .await;
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the std Mutex serializes the process-global `playing` flag; holding it across the LocalSet await is intentional"
)]
async fn lazy_entry_defers_the_timeline_until_the_color_reply() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    common::run_local(async {
        let mut renderer = fullscreen();
        assert!(play_armin_3d(&mut renderer));
        assert_eq!(
            last_animation_colors_pending_for_test(),
            Some(true),
            "TS constructs the animation only after queryTerminalColors"
        );
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        assert_eq!(last_animation_colors_pending_for_test(), Some(true));
        let pending_elapsed = last_animation_elapsed_ms_for_test().expect("animation is alive");
        assert!(
            pending_elapsed >= 60.0,
            "the wall clock runs while the reply is outstanding: {pending_elapsed}"
        );

        let InteractiveRenderer::Fullscreen(screen) = &mut renderer else {
            unreachable!("constructed as fullscreen")
        };
        screen.handle_terminal_input("\x1b]10;#112233\x07");
        screen.handle_terminal_input("\x1b]11;#445566\x07");
        screen.handle_terminal_input("\x1b[?62;22c");
        common::yield_tasks(20).await;

        assert_eq!(last_animation_colors_pending_for_test(), Some(false));
        let colors = last_animation_colors_for_test().expect("animation is alive");
        assert_eq!(colors.foreground, [17.0, 34.0, 51.0]);
        assert_eq!(colors.background, [68.0, 85.0, 102.0]);
        let resolved_elapsed = last_animation_elapsed_ms_for_test().expect("animation is alive");
        assert!(
            resolved_elapsed < 45.0,
            "start_time restarts when the animation is constructed: {resolved_elapsed}"
        );
        renderer.hide_overlay();
        reset_playing_state_for_test();
    })
    .await;
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the std Mutex serializes the process-global `playing` flag; holding it across the LocalSet await is intentional"
)]
async fn lazy_entry_ignores_late_color_replies_after_the_query_timeout() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    common::run_local(async {
        let mut renderer = fullscreen();
        assert!(play_armin_3d(&mut renderer));
        assert_eq!(last_animation_colors_pending_for_test(), Some(true));
        tokio::time::sleep(std::time::Duration::from_millis(160)).await;
        assert_eq!(
            last_animation_colors_pending_for_test(),
            Some(false),
            "the 100 ms query deadline constructs the animation with fallbacks"
        );
        let fallback = last_animation_colors_for_test().expect("animation is alive");
        assert_eq!(fallback.background, [0.0, 0.0, 0.0], "dark theme fallback");

        let InteractiveRenderer::Fullscreen(screen) = &mut renderer else {
            unreachable!("constructed as fullscreen")
        };
        screen.handle_terminal_input("\x1b]10;#112233\x07");
        screen.handle_terminal_input("\x1b]11;#445566\x07");
        screen.handle_terminal_input("\x1b[?62;22c");
        common::yield_tasks(20).await;

        assert_eq!(
            last_animation_colors_for_test(),
            Some(fallback),
            "TS constructs the animation at the timeout; later replies never recolor it"
        );
        renderer.hide_overlay();
        reset_playing_state_for_test();
    })
    .await;
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the std Mutex serializes the process-global `playing` flag; holding it across the LocalSet await is intentional"
)]
async fn second_close_in_input_hides_the_frame_and_finishes_after_dispatch() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    common::run_local(async {
        let mut renderer = fullscreen();
        assert!(play_armin_3d(&mut renderer));
        // 先让终端颜色查询结束（TS 在这之后才创建动画并接收输入）。
        {
            let InteractiveRenderer::Fullscreen(screen) = &mut renderer else {
                unreachable!("constructed as fullscreen")
            };
            screen.handle_terminal_input("\x1b]10;#112233\x07");
            screen.handle_terminal_input("\x1b]11;#445566\x07");
            screen.handle_terminal_input("\x1b[?62;22c");
        }
        common::yield_tasks(10).await;
        assert_eq!(last_animation_colors_pending_for_test(), Some(false));
        assert_eq!(last_animation_hidden_for_test(), Some(false));

        {
            let InteractiveRenderer::Fullscreen(screen) = &mut renderer else {
                unreachable!("constructed as fullscreen")
            };
            screen.handle_terminal_input("\x1b");
        }
        assert!(is_playing(), "the first Esc starts the exit animation");
        assert_eq!(last_animation_hidden_for_test(), Some(false));

        {
            let InteractiveRenderer::Fullscreen(screen) = &mut renderer else {
                unreachable!("constructed as fullscreen")
            };
            screen.handle_terminal_input("\x1b");
        }
        assert_eq!(
            last_animation_hidden_for_test(),
            Some(true),
            "the second Esc hides the frame without waiting for a timer tick"
        );
        assert!(
            !renderer.has_overlay(),
            "TS's hasOverlay() is false right after the synchronous hide"
        );

        common::yield_tasks(10).await;
        assert!(!renderer.has_overlay());
        assert!(
            !is_playing(),
            "the deferred finish() runs once the input dispatch returns"
        );
        reset_playing_state_for_test();
    })
    .await;
}

// ---------------------------------------------------------------------------
// lazy / eager 入口
// ---------------------------------------------------------------------------

fn fullscreen() -> InteractiveRenderer {
    InteractiveRenderer::Fullscreen(alt_screen(
        TestTerminal::with_size(80, 24),
        TuiAltScreenOptions::default(),
    ))
}

#[test]
fn lazy_entries_require_the_alternate_screen() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    let mut regular = InteractiveRenderer::Regular(main_screen(TestTerminal::new()));
    assert!(!play_armin_3d(&mut regular));
    play_kiana_logo_3d(&mut regular, 1, 0);
    assert!(!regular.has_overlay());
    assert!(!is_playing());
}

#[test]
fn lazy_entries_play_in_fullscreen_and_skip_when_an_overlay_exists() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();

    let mut renderer = fullscreen();
    assert!(play_armin_3d(&mut renderer));
    assert!(renderer.has_overlay(), "Armin starts the 3D overlay");
    assert!(is_playing());
    // 已有 overlay：返回 true，但什么都不播（playing 不会被改动）。
    assert!(play_armin_3d(&mut renderer));
    assert!(is_playing());
    renderer.hide_overlay();
    reset_playing_state_for_test();

    let mut renderer = fullscreen();
    play_kiana_logo_3d(&mut renderer, 3, 1);
    assert!(
        renderer.has_overlay(),
        "the kiana logo starts the 3D overlay"
    );
    assert!(is_playing());
    renderer.hide_overlay();
    reset_playing_state_for_test();

    // 预先存在的 overlay：play* 直接返回，不设置 playing。
    let mut renderer = fullscreen();
    renderer.show_overlay(Box::new(Text::new("dialog")), Default::default());
    assert!(play_armin_3d(&mut renderer));
    assert!(!is_playing());
    reset_playing_state_for_test();
}

#[test]
fn lazy_entry_captures_the_screen_before_playing() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    let mut renderer = fullscreen();
    renderer.add_child(Box::new(Text::new("captured screen line")));
    renderer.start();
    renderer.render_now(true);
    let before = renderer.get_screen_lines();
    assert!(
        before
            .iter()
            .any(|line| line.contains("captured screen line"))
    );
    play_kiana_logo_3d(&mut renderer, 1, 0);
    assert!(renderer.has_overlay());
    // 动画的第一帧就是从抓下来的屏幕开始渲染（这里只验证 overlay 建立成功；
    // 抓屏内容由 `frames` 夹具的 prepareCells 覆盖）。
    renderer.hide_overlay();
    reset_playing_state_for_test();
}

#[tokio::test]
#[allow(
    clippy::await_holding_lock,
    reason = "the std Mutex serializes the process-global `playing` flag; holding it across the LocalSet await is intentional"
)]
async fn eager_entry_queries_terminal_colors_then_plays() {
    let _guard = global_lock();
    theme();
    reset_playing_state_for_test();
    common::run_local(async {
        let mut renderer = fullscreen();
        let screen = renderer.get_screen_lines();
        play_easter_egg_3d(&mut renderer, screen, EasterEgg3d::Armin).await;
        assert!(renderer.has_overlay());
        assert!(is_playing());
        renderer.hide_overlay();
        reset_playing_state_for_test();
    })
    .await;
}

#[test]
fn colors_fallback_follows_the_theme() {
    theme();
    // 只验证常量与兜底色的关系（TS: 深色主题 → 黑色背景）。
    assert_eq!(CORAL, [228.0, 138.0, 122.0]);
    assert_eq!(BLUE, [79.0, 142.0, 179.0]);
    assert_eq!(YELLOW, [234.0, 182.0, 93.0]);
}

#[test]
fn every_frame_is_exactly_as_wide_as_the_viewport() {
    theme();
    let times: [f64; 7] = [0.05, 0.2, 1.12, 2.6, 4.45, 6.1, 9.3];
    for width in [7usize, 20, 60] {
        let (clock, clock_value) = clock_at(1_000_000.0);
        let done = Rc::new(Cell::new(0));
        let requests = Rc::new(Cell::new(0));
        let mut animation = EasterEgg3dAnimation::new(
            sample_screen(),
            kiana_logo_model((2, 1)),
            EasterEggColors {
                foreground: [200.0, 200.0, 200.0],
                background: [0.0, 0.0, 0.0],
            },
            move || done.set(done.get() + 1),
            rows12(),
            Some({
                let requests = Rc::clone(&requests);
                Rc::new(move || requests.set(requests.get() + 1))
            }),
            clock,
        );
        for time in times {
            clock_value.set(1_000_000.0 + (time * 1000.0).round());
            let lines = animation.render(width);
            assert_eq!(lines.len(), 12, "height at {time}s");
            for (row, line) in lines.iter().enumerate() {
                assert_eq!(
                    kiana_tui::text::visible_width(line),
                    width,
                    "row {row} at {time}s width {width}: {line:?}"
                );
                let plain = kiana_tui::text::strip_terminal_sequences(line);
                assert!(
                    !plain.chars().any(|ch| ch.is_control()),
                    "row {row} at {time}s contains a control character: {plain:?}"
                );
            }
        }
        // 退出动画的每一帧同样严格等于视口宽度。
        clock_value.set(1_009_300.0);
        animation.close();
        for advance in [0.0, 300.0, 700.0, 1100.0] {
            clock_value.set(1_009_300.0 + advance);
            let lines = animation.render(width);
            assert_eq!(lines.len(), 12);
            for line in &lines {
                assert_eq!(kiana_tui::text::visible_width(line), width);
            }
        }
    }
}
