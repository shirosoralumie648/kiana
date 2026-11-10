//! `CountdownTimer` 的测试（对应 TS `status-indicator.test.ts` 里倒计时的行为参考）。

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kiana_ui_kit::CountdownTimer;

#[test]
fn ticks_each_second_and_expires_at_zero() {
    common::setup();
    let ticks: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let events: Rc<RefCell<Vec<&'static str>>> = Rc::new(RefCell::new(Vec::new()));
    let expired = Rc::new(Cell::new(false));
    let (request_render, renders) = common::request_render();

    let tick_sink = Rc::clone(&ticks);
    let tick_events = Rc::clone(&events);
    let expire_flag = Rc::clone(&expired);
    let expire_events = Rc::clone(&events);
    let render_sink = Rc::clone(&events);
    let request_render = {
        let original = request_render;
        Rc::new(move || {
            render_sink.borrow_mut().push("render");
            original();
        })
    };

    let mut timer = CountdownTimer::new(
        2500.0,
        Some(request_render),
        Box::new(move |seconds| {
            tick_sink.borrow_mut().push(seconds);
            tick_events.borrow_mut().push("tick");
        }),
        Box::new(move || {
            expire_flag.set(true);
            expire_events.borrow_mut().push("expire");
        }),
    );

    // 构造时立即 tick（ceil(2500/1000) = 3）。
    assert_eq!(*ticks.borrow(), vec![3.0]);
    assert_eq!(timer.remaining_seconds(), 3.0);

    let (fake, handle) = common::fake_timer();
    timer.set_timer(Box::new(fake));
    assert_eq!(handle.delays(), vec![1000]);

    assert!(handle.fire());
    assert_eq!(timer.remaining_seconds(), 2.0);
    assert!(handle.is_pending(), "re-armed while remaining > 0");
    assert_eq!(renders.get(), 1);

    assert!(handle.fire());
    assert_eq!(timer.remaining_seconds(), 1.0);

    assert!(handle.fire());
    assert_eq!(timer.remaining_seconds(), 0.0);
    assert!(expired.get());
    assert!(timer.is_disposed(), "expiry disposes the timer");
    assert!(!handle.is_pending(), "no further ticks after expiry");
    assert_eq!(*ticks.borrow(), vec![3.0, 2.0, 1.0, 0.0]);
    assert_eq!(renders.get(), 3);

    // 顺序：先 tick，再 requestRender，再 expire。
    let events = events.borrow();
    let tail = &events[events.len() - 3..];
    assert_eq!(tail, &["tick", "render", "expire"]);
}

#[test]
fn dispose_cancels_and_is_idempotent() {
    common::setup();
    let ticks: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let expired = Rc::new(Cell::new(false));
    let tick_sink = Rc::clone(&ticks);
    let expire_flag = Rc::clone(&expired);

    let mut timer = CountdownTimer::new(
        10_000.0,
        None,
        Box::new(move |seconds| tick_sink.borrow_mut().push(seconds)),
        Box::new(move || expire_flag.set(true)),
    );
    let (fake, handle) = common::fake_timer();
    timer.set_timer(Box::new(fake));
    assert!(handle.is_pending());

    timer.dispose();
    assert!(timer.is_disposed());
    assert!(!handle.is_pending());
    let cancels = handle.cancel_count();

    timer.dispose();
    assert_eq!(handle.cancel_count(), cancels, "second dispose is a no-op");
    assert!(!handle.fire(), "no callback after dispose");
    assert!(!expired.get());
    assert_eq!(*ticks.borrow(), vec![10.0], "only the construction tick");
}

#[test]
fn zero_timeout_ticks_zero_then_expires() {
    common::setup();
    let ticks: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
    let expired = Rc::new(Cell::new(false));
    let tick_sink = Rc::clone(&ticks);
    let expire_flag = Rc::clone(&expired);

    let mut timer = CountdownTimer::new(
        0.0,
        None,
        Box::new(move |seconds| tick_sink.borrow_mut().push(seconds)),
        Box::new(move || expire_flag.set(true)),
    );
    assert_eq!(*ticks.borrow(), vec![0.0]);

    let (fake, handle) = common::fake_timer();
    timer.set_timer(Box::new(fake));
    assert!(handle.fire(), "the interval still fires once");
    assert_eq!(*ticks.borrow(), vec![0.0, -1.0]);
    assert!(expired.get());
    assert!(!handle.is_pending());
}

#[test]
fn ceil_rounds_up_fractions() {
    common::setup();
    for (timeout_ms, expected) in [
        (1.0, 1.0),
        (999.0, 1.0),
        (1000.0, 1.0),
        (1001.0, 2.0),
        (1500.0, 2.0),
        (2500.0, 3.0),
        (-1.0, -0.0),
    ] {
        let seen: Rc<RefCell<Vec<f64>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let timer = CountdownTimer::new(
            timeout_ms,
            None,
            Box::new(move |seconds| sink.borrow_mut().push(seconds)),
            Box::new(|| {}),
        );
        let expected = if expected == 0.0 { 0.0 } else { expected };
        assert_eq!(seen.borrow()[0], expected, "timeoutMs={timeout_ms}");
        drop(timer);
    }
}

#[test]
fn replacing_the_timer_cancels_the_old_one() {
    common::setup();
    let mut timer = CountdownTimer::new(3.0, None, Box::new(|_| {}), Box::new(|| {}));
    let (first, first_handle) = common::fake_timer();
    timer.set_timer(Box::new(first));
    assert!(first_handle.is_pending());

    let (second, second_handle) = common::fake_timer();
    timer.set_timer(Box::new(second));
    assert_eq!(first_handle.cancel_count(), 1);
    assert!(!first_handle.is_pending());
    assert_eq!(second_handle.delays(), vec![1000]);
}

#[test]
fn works_without_any_timer() {
    common::setup();
    let mut timer = CountdownTimer::new(1000.0, None, Box::new(|_| {}), Box::new(|| {}));
    timer.dispose();
    assert!(timer.is_disposed());
    // 再安装定时器不会重新排程。
    let (fake, handle) = common::fake_timer();
    timer.set_timer(Box::new(fake));
    assert!(!handle.is_pending());
}
