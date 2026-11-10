//! `components/armin.ts` 的黄金对拍与不变量测试。
//!
//! TS 侧没有自动化测试；`tests/fixtures/armin.json` 由
//! `/home/ctyun/kiana-work/scratch/anim/gen.ts` 导入真实 TS 模块生成：
//!
//! * `pixels`：`isArminPixel(x, y)` 的 36×32 网格（含 x=31 的 XBM padding 位）。
//! * `effects`：7 种效果各跑 N 帧，记录每帧网格的 FNV-1a 摘要（每 10 帧与首尾另存整格），
//!   以及最终状态在 7 个宽度下的 `render(width)` 完整 ANSI 行。
//! * `randoms`：TS 的 `Math.random()` 调用序列（逐次记录），Rust 用 [`SequenceRng`] 重放。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::OnceLock;

use kiana_interactive::components::armin::{
    ARMIN_HEIGHT, ARMIN_WIDTH, ArminClock, ArminComponent, ArminRng, Effect, is_armin_pixel,
};
use kiana_theme::init_theme;
use kiana_tui::terminal_image::{TerminalCapabilities, set_capabilities};
use kiana_tui::tui::{Component, Timer};
use serde_json::Value;

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

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/armin.json")).expect("armin fixture parses")
}

struct SequenceRng {
    values: Vec<f64>,
    index: usize,
}

impl SequenceRng {
    fn new(values: Vec<f64>) -> Self {
        SequenceRng { values, index: 0 }
    }

    fn remaining(&self) -> usize {
        self.values.len() - self.index
    }
}

impl ArminRng for SequenceRng {
    fn next_f64(&mut self) -> f64 {
        let value = self.values[self.index];
        self.index += 1;
        value
    }
}

fn digest(grid: &[Vec<char>]) -> String {
    let lines: Vec<String> = grid.iter().map(|row| row.iter().collect()).collect();
    let text = lines.join("\n");
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in text.encode_utf16() {
        hash ^= u64::from(unit);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:x}")
}

fn grid_strings(grid: &[Vec<char>]) -> Vec<String> {
    grid.iter().map(|row| row.iter().collect()).collect()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_string())
        .collect()
}

fn numbers(value: &Value) -> Vec<f64> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_f64().expect("number"))
        .collect()
}

/// 固定时钟 + 序号 RNG 的组件（构造函数会消耗第 1 个随机数来选择效果）。
fn component(
    randoms: Vec<f64>,
) -> (
    ArminComponent,
    Rc<Cell<f64>>,
    Rc<std::cell::RefCell<SequenceRng>>,
) {
    theme();
    let clock_value = Rc::new(Cell::new(0.0));
    let clock_handle = Rc::clone(&clock_value);
    let clock: ArminClock = Rc::new(move || clock_handle.get());
    let rng = Rc::new(std::cell::RefCell::new(SequenceRng::new(randoms)));
    struct SharedRng(Rc<std::cell::RefCell<SequenceRng>>);
    impl ArminRng for SharedRng {
        fn next_f64(&mut self) -> f64 {
            self.0.borrow_mut().next_f64()
        }
    }
    let component =
        ArminComponent::with_rng_and_clock(None, Box::new(SharedRng(Rc::clone(&rng))), clock);
    (component, clock_value, rng)
}

#[test]
fn is_armin_pixel_matches_ts_bitmap() {
    let fixture = fixture();
    let pixels = strings(&fixture["pixels"]);
    assert_eq!(pixels.len(), ARMIN_HEIGHT);
    for (y, row) in pixels.iter().enumerate() {
        for x in 0..=31 {
            let expected = row.as_bytes()[x] == b'1';
            assert_eq!(is_armin_pixel(x, y), expected, "is_armin_pixel({x}, {y})");
        }
    }
    for x in 0..32 {
        assert!(!is_armin_pixel(x, ARMIN_HEIGHT), "y beyond the bitmap");
        assert!(!is_armin_pixel(x, ARMIN_HEIGHT + 5));
    }
    // TS 的越界语义：`BITS[byteIndex]` 是 undefined → `undefined >> n === 0` → 前景（true）。
    assert!(
        is_armin_pixel(32, 35),
        "out-of-bounds byte reads as foreground"
    );
    assert!(
        is_armin_pixel(1200, 0),
        "far out-of-bounds reads as foreground"
    );
    assert_eq!(ARMIN_WIDTH, 31);
}

#[test]
fn final_grid_matches_ts() {
    let fixture = fixture();
    for effect in fixture["effects"].as_array().expect("effects") {
        let randoms = numbers(&effect["randoms"]);
        let (component, _clock, _rng) = component(randoms);
        let expected = strings(&effect["finalGrid"]);
        assert_eq!(grid_strings(&component.final_grid()), expected);
    }
}

#[test]
fn every_effect_matches_ts_frame_by_frame() {
    let fixture = fixture();
    for effect in fixture["effects"].as_array().expect("effects") {
        let name = effect["effect"].as_str().expect("effect name");
        let randoms = numbers(&effect["randoms"]);
        let (component, _clock, rng) = component(randoms.clone());
        let expected_effect = match name {
            "typewriter" => Effect::Typewriter,
            "scanline" => Effect::Scanline,
            "rain" => Effect::Rain,
            "fade" => Effect::Fade,
            "crt" => Effect::Crt,
            "glitch" => Effect::Glitch,
            "dissolve" => Effect::Dissolve,
            other => panic!("unknown effect {other}"),
        };
        assert_eq!(component.effect(), expected_effect);

        let samples: std::collections::HashMap<usize, Vec<String>> = effect["sampleFrames"]
            .as_array()
            .expect("sampleFrames")
            .iter()
            .map(|sample| {
                (
                    sample["index"].as_u64().expect("index") as usize,
                    strings(&sample["grid"]),
                )
            })
            .collect();
        for (index, frame) in effect["frames"]
            .as_array()
            .expect("frames")
            .iter()
            .enumerate()
        {
            let done = component.tick();
            let current = component.current_grid();
            let expected_digest = frame["digest"].as_str().expect("digest");
            assert_eq!(
                digest(&current),
                expected_digest,
                "{name}: frame {index} digest"
            );
            assert_eq!(
                done,
                frame["done"].as_bool().expect("done"),
                "{name}: frame {index} done"
            );
            if let Some(expected) = samples.get(&index) {
                assert_eq!(
                    &grid_strings(&current),
                    expected,
                    "{name}: frame {index} grid"
                );
            }
        }
        assert_eq!(
            rng.borrow().remaining(),
            0,
            "{name}: the TS random stream must be fully consumed"
        );
    }
}

#[test]
fn render_matches_ts_at_every_width() {
    let fixture = fixture();
    for effect in fixture["effects"].as_array().expect("effects") {
        let name = effect["effect"].as_str().expect("effect name");
        let randoms = numbers(&effect["randoms"]);
        let (mut component, _clock, _rng) = component(randoms);
        for _ in 0..effect["frames"].as_array().expect("frames").len() {
            component.tick();
        }
        for rendered in effect["rendered"].as_array().expect("rendered") {
            let width = rendered["width"].as_u64().expect("width") as usize;
            let expected = strings(&rendered["lines"]);
            let actual = component.render(width);
            assert_eq!(actual, expected, "{name}: render({width})");
        }
    }
}

#[test]
fn render_width_overflow_matches_ts_message_rules() {
    theme();
    // Typewriter 效果、零随机消耗：最终消息行在 width < 14 时溢出（TS 既有行为）。
    let (mut component, _clock, _rng) = component(vec![0.0]);
    let lines = component.render(0);
    let visible = |line: &str| kiana_tui::text::visible_width(line);
    assert_eq!(
        visible(&lines[0]),
        31,
        "width=0 keeps 30 cells + 1 leading space"
    );
    assert_eq!(visible(&lines[18]), 14, "ARMIN SAYS HI overflows width 0");
    for width in [1usize, 2, 10, 13, 14, 15, 20, 31, 32, 40] {
        let lines = component.render(width);
        assert_eq!(lines.len(), 19);
        assert_eq!(visible(&lines[0]), if width == 0 { 31 } else { width });
        assert_eq!(
            visible(&lines[18]),
            if width < 14 { 14 } else { width },
            "message line at width {width}"
        );
    }
    // invalidate() 只清宽度缓存，下一次 render 重建同样的内容。
    let before = component.render(20);
    component.invalidate();
    assert_eq!(component.render(20), before);
}

#[test]
fn dispose_is_idempotent_and_stops_advancing() {
    theme();
    let (mut component, clock, _rng) = component(vec![0.0]);
    clock.set(10_000.0);
    component.dispose();
    component.dispose();
    let lines = component.render(20);
    assert_eq!(lines.len(), 19);
    assert_eq!(
        component.current_grid(),
        vec![vec![' '; ARMIN_WIDTH]; 18],
        "disposed components never advance"
    );
}

#[test]
fn finished_or_disposed_animation_stops_requesting_renders() {
    theme();
    let clock_value = Rc::new(Cell::new(0.0));
    let clock_handle = Rc::clone(&clock_value);
    let clock: ArminClock = Rc::new(move || clock_handle.get());
    let requests = Rc::new(Cell::new(0usize));
    let request: kiana_tui::components::loader::RequestRenderCallback = {
        let requests = Rc::clone(&requests);
        Rc::new(move || requests.set(requests.get() + 1))
    };
    // Math.random() = 0 → EFFECTS[0] = typewriter（18 × 31 / 3 = 186 帧后结束）。
    let mut component = ArminComponent::with_rng_and_clock(
        Some(request),
        Box::new(SequenceRng::new(vec![0.0])),
        clock,
    );
    assert_eq!(component.effect(), Effect::Typewriter);
    component.render(20);
    assert_eq!(
        requests.get(),
        1,
        "an animating render asks the host for the next frame"
    );

    clock_value.set(10_000.0);
    component.render(20);
    assert!(component.is_done());
    assert_eq!(
        requests.get(),
        1,
        "the finishing render must not re-arm the host render loop (TS stopAnimation)"
    );
    for _ in 0..5 {
        component.render(20);
    }
    assert_eq!(requests.get(), 1, "a finished animation is fully idle");

    component.dispose();
    assert!(component.tick());
    for _ in 0..5 {
        component.render(20);
    }
    assert_eq!(requests.get(), 1, "a disposed animation is fully idle");
}

// ---------------------------------------------------------------------------
// 定时器（TS 的 setInterval 节拍）
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
struct SharedTimer(Rc<std::cell::RefCell<FakeTimer>>);

impl Timer for SharedTimer {
    fn set(&mut self, delay_ms: u64, callback: Box<dyn FnOnce()>) {
        self.0.borrow_mut().set(delay_ms, callback);
    }

    fn cancel(&mut self) {
        self.0.borrow_mut().cancel();
    }
}

impl SharedTimer {
    fn fire(&self) -> bool {
        let callback = self.0.borrow_mut().pending.take();
        match callback {
            Some(callback) => {
                callback();
                true
            }
            None => false,
        }
    }

    fn delays(&self) -> Vec<u64> {
        self.0.borrow().delays.clone()
    }

    fn cancelled(&self) -> usize {
        self.0.borrow().cancelled
    }
}

/// TS 的 `setInterval` 每次回调只 `tickEffect()` 一次：墙钟跳变不会补帧，动画也不依赖
/// `render` 才推进（review finding 3/7）。
#[test]
fn timer_advances_exactly_one_frame_per_tick() {
    theme();
    let (mut component, clock, _rng) = component(vec![0.0]); // typewriter
    let timer = SharedTimer::default();
    component.set_timer(Box::new(timer.clone()));
    assert_eq!(
        timer.delays(),
        vec![33],
        "1000 / 30 truncated to integer ms"
    );
    assert_eq!(component.frames_done(), 0);

    clock.set(10_000.0);
    assert!(timer.fire());
    assert_eq!(
        component.frames_done(),
        1,
        "a wall-clock jump must not replay missed intervals"
    );
    assert!(timer.fire());
    assert_eq!(component.frames_done(), 2);

    // 定时器在跑时 render 只读，不推进状态、也不再请求下一帧。
    component.render(20);
    assert_eq!(component.frames_done(), 2);
}

#[test]
fn timer_advances_without_render_and_requests_each_frame() {
    theme();
    let requests = Rc::new(Cell::new(0usize));
    let request: kiana_tui::components::loader::RequestRenderCallback = {
        let requests = Rc::clone(&requests);
        Rc::new(move || requests.set(requests.get() + 1))
    };
    let mut component = ArminComponent::with_rng_and_clock(
        Some(request),
        Box::new(SequenceRng::new(vec![0.0])),
        Rc::new(|| 0.0),
    );
    let timer = SharedTimer::default();
    component.set_timer(Box::new(timer.clone()));
    for _ in 0..3 {
        assert!(timer.fire());
    }
    assert_eq!(
        component.frames_done(),
        3,
        "the animation advances even when it is never rendered"
    );
    assert_eq!(requests.get(), 3, "each interval tick requests a frame");

    component.render(20);
    assert_eq!(
        requests.get(),
        3,
        "render never re-arms the render loop while the timer runs"
    );
}

#[test]
fn timer_final_tick_requests_a_frame_then_stops() {
    theme();
    let requests = Rc::new(Cell::new(0usize));
    let request: kiana_tui::components::loader::RequestRenderCallback = {
        let requests = Rc::clone(&requests);
        Rc::new(move || requests.set(requests.get() + 1))
    };
    let mut component = ArminComponent::with_rng_and_clock(
        Some(request),
        Box::new(SequenceRng::new(vec![0.0])),
        Rc::new(|| 0.0),
    );
    let timer = SharedTimer::default();
    component.set_timer(Box::new(timer.clone()));
    // typewriter：558 个格子每帧 3 个，第 187 次回调在 `row >= 18` 时返回 done。
    let mut ticks = 0;
    while !component.is_done() {
        assert!(
            timer.fire(),
            "the interval is always rescheduled until done"
        );
        ticks += 1;
        assert!(ticks <= 200, "typewriter finishes in 187 ticks");
    }
    assert_eq!(ticks, 187);
    assert_eq!(
        requests.get(),
        187,
        "TS requestRender() runs on the final tick too"
    );
    assert_eq!(
        timer.cancelled(),
        1,
        "tickEffect() === true calls stopAnimation()"
    );
    assert!(!timer.fire(), "no callback remains after stopAnimation()");
    assert_eq!(
        component.frames_done(),
        187,
        "finishing does not run extra ticks"
    );
}

#[test]
fn glitch_uses_sixty_fps_and_other_effects_thirty() {
    theme();
    // Math.random() = 0.8 → EFFECTS[5] = glitch.
    let (mut glitch, _clock, _rng) = component(vec![0.8]);
    assert_eq!(glitch.effect(), Effect::Glitch);
    let timer = SharedTimer::default();
    glitch.set_timer(Box::new(timer.clone()));
    assert_eq!(
        timer.delays(),
        vec![16],
        "1000 / 60 truncated to integer ms"
    );

    // 只选 initEffect 不额外消耗随机数的效果（fade/dissolve 需要完整随机序列）。
    for random in [0.0, 0.2, 0.6] {
        let (mut other, _clock, _rng) = component(vec![random]);
        assert_ne!(other.effect(), Effect::Glitch);
        let timer = SharedTimer::default();
        other.set_timer(Box::new(timer.clone()));
        assert_eq!(timer.delays(), vec![33], "{:?}", other.effect());
    }
}
