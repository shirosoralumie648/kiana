//! `test/wheel-scroll.test.ts` 的逐条移植（6 个 it）。

use kiana_tui::wheel::{WheelScrollAccelerator, WheelScrollLines};

fn scroll(accelerator: &mut WheelScrollAccelerator, times: &[f64], direction: i32) -> Vec<usize> {
    times
        .iter()
        .map(|time| accelerator.next(direction, *time))
        .collect()
}

/// `uses fixed line counts regardless of timing`
///
/// (#9758: fullscreen wheel scrolling was one line per notch on terminals that do not
/// accelerate wheels.)
#[test]
fn uses_fixed_line_counts_regardless_of_timing() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Fixed(3.0), true);
    assert_eq!(
        scroll(&mut accelerator, &[0.0, 10.0, 20.0, 1000.0], 1),
        vec![3, 3, 3, 3]
    );
    accelerator.set_lines(WheelScrollLines::Fixed(0.5));
    assert_eq!(accelerator.next(1, 2000.0), 1);
}

/// `keeps one line per event in auto mode when the terminal already accelerates`
#[test]
fn keeps_one_line_per_event_in_auto_mode_when_the_terminal_already_accelerates() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Auto, false);
    assert_eq!(
        scroll(&mut accelerator, &[0.0, 10.0, 20.0, 30.0], 1),
        vec![1, 1, 1, 1]
    );
}

/// `scales auto mode with wheel velocity`
#[test]
fn scales_auto_mode_with_wheel_velocity() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Auto, true);
    assert_eq!(
        scroll(&mut accelerator, &[0.0, 150.0, 300.0, 450.0], 1),
        vec![1, 1, 1, 1]
    );
    assert_eq!(
        scroll(&mut accelerator, &[1000.0, 1050.0, 1100.0, 1150.0], 1),
        vec![1, 2, 2, 2]
    );
    assert_eq!(
        scroll(&mut accelerator, &[2000.0, 2020.0, 2040.0, 2060.0], 1),
        vec![1, 5, 5, 5]
    );
    assert_eq!(
        scroll(&mut accelerator, &[3000.0, 3010.0, 3020.0, 3030.0], 1),
        vec![1, 6, 6, 6]
    );
}

/// `does not accelerate bursts of events for a single notch`
#[test]
fn does_not_accelerate_bursts_of_events_for_a_single_notch() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Auto, true);
    assert_eq!(
        scroll(&mut accelerator, &[0.0, 3.0, 6.0, 9.0], 1),
        vec![1, 1, 1, 1]
    );
}

/// `resets acceleration on direction changes and pauses`
#[test]
fn resets_acceleration_on_direction_changes_and_pauses() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Auto, true);
    assert_eq!(
        scroll(&mut accelerator, &[0.0, 20.0, 40.0], 1),
        vec![1, 5, 5]
    );
    assert_eq!(accelerator.next(-1, 60.0), 1);
    assert_eq!(scroll(&mut accelerator, &[500.0, 520.0], 1), vec![1, 5]);
}

/// `carries fractional lines between events`
#[test]
fn carries_fractional_lines_between_events() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Auto, true);
    assert_eq!(
        scroll(&mut accelerator, &[0.0, 40.0, 80.0, 120.0, 160.0], 1),
        vec![1, 2, 3, 2, 3]
    );
}

/// Rust 侧补充（TS 无对应）：非有限的固定行数回退为 1 行。
///
/// `WheelScrollLines::Fixed` 在 TS 里是 `number`，`NaN`/`Infinity` 会传播到
/// `Math.max(1, ...)`；这里固定同一行为。
#[test]
fn non_finite_fixed_lines_fall_back_to_one() {
    let mut accelerator = WheelScrollAccelerator::new(WheelScrollLines::Fixed(f64::NAN), true);
    assert_eq!(accelerator.next(1, 0.0), 1);
    accelerator.set_lines(WheelScrollLines::Fixed(f64::INFINITY));
    assert_eq!(accelerator.next(1, 0.0), 1);
}
