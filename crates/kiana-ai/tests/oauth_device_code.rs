//! Port of `packages/ai/test/oauth-device-code.test.ts`.
//!
//! TS 用 vitest 的 fake timers；Rust 侧用 `tokio::time::start_paused` + 可注入墙钟
//! （[`AuthClock`]）复刻同一条时间线。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiana_ai::auth::oauth::device_code::{
    AuthClock, OAuthDeviceCodePollOptions, OAuthDeviceCodePollResult, abortable_sleep,
    poll_oauth_device_code_flow_with_clock,
};
use kiana_ai::auth::types::AuthError;
use kiana_ai::utils::abort::AbortScope;

/// 用 tokio 虚拟时间实现的 `Date.now()`。
struct TokioClock {
    start: tokio::time::Instant,
}

impl TokioClock {
    fn new() -> Arc<Self> {
        Arc::new(TokioClock {
            start: tokio::time::Instant::now(),
        })
    }
}

impl AuthClock for TokioClock {
    fn now_ms(&self) -> f64 {
        self.start.elapsed().as_secs_f64() * 1000.0
    }
}

fn options<T>(
    interval_seconds: Option<f64>,
    expires_in_seconds: Option<f64>,
    wait_before_first_poll: bool,
    poll: Arc<
        dyn Fn() -> kiana_ai::auth::oauth::device_code::OAuthDeviceCodePollFuture<T> + Send + Sync,
    >,
) -> OAuthDeviceCodePollOptions<T> {
    OAuthDeviceCodePollOptions {
        interval_seconds,
        expires_in_seconds,
        wait_before_first_poll,
        poll,
        signal: AbortScope::new(),
    }
}

/// 记录每次轮询时刻并按脚本返回结果的 poll 回调。
fn scripted_poll(
    clock: Arc<TokioClock>,
    times: Arc<Mutex<Vec<f64>>>,
    results: Arc<Mutex<Vec<OAuthDeviceCodePollResult<&'static str>>>>,
) -> Arc<
    dyn Fn() -> kiana_ai::auth::oauth::device_code::OAuthDeviceCodePollFuture<&'static str>
        + Send
        + Sync,
> {
    Arc::new(move || {
        let clock = Arc::clone(&clock);
        let times = Arc::clone(&times);
        let results = Arc::clone(&results);
        Box::pin(async move {
            times.lock().unwrap().push(clock.now_ms());
            let mut results = results.lock().unwrap();
            if results.is_empty() {
                return Err(AuthError::new("Unexpected extra poll"));
            }
            Ok(results.remove(0))
        })
    })
}

fn completion(value: &'static str) -> OAuthDeviceCodePollResult<&'static str> {
    OAuthDeviceCodePollResult::Complete { value }
}

#[tokio::test(start_paused = true)]
async fn polls_immediately_and_returns_the_completed_value() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::Pending,
        completion("token"),
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let result = poll_oauth_device_code_flow_with_clock(
        options(Some(2.0), Some(30.0), false, poll),
        &*clock,
    )
    .await
    .expect("complete");
    assert_eq!(result, "token");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 2000.0]);
}

#[tokio::test(start_paused = true)]
async fn can_wait_before_the_first_poll() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![completion("token")]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let result =
        poll_oauth_device_code_flow_with_clock(options(Some(2.0), Some(30.0), true, poll), &*clock)
            .await
            .expect("complete");
    assert_eq!(result, "token");
    assert_eq!(*times.lock().unwrap(), vec![2000.0]);
}

#[tokio::test(start_paused = true)]
async fn increases_the_interval_by_5_seconds_after_slow_down_without_a_server_interval() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::SlowDown {
            interval_seconds: None,
        },
        completion("token"),
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let result = poll_oauth_device_code_flow_with_clock(
        options(Some(2.0), Some(900.0), false, poll),
        &*clock,
    )
    .await
    .expect("complete");
    assert_eq!(result, "token");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 7000.0]);
}

#[tokio::test(start_paused = true)]
async fn honors_a_server_provided_slow_down_interval() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::SlowDown {
            interval_seconds: Some(30.0),
        },
        completion("token"),
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let result = poll_oauth_device_code_flow_with_clock(
        options(Some(2.0), Some(900.0), false, poll),
        &*clock,
    )
    .await
    .expect("complete");
    assert_eq!(result, "token");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 30000.0]);
}

#[tokio::test(start_paused = true)]
async fn cancels_an_in_flight_wait() {
    let clock = TokioClock::new();
    let signal = AbortScope::new();
    let aborter = signal.clone();
    let options = OAuthDeviceCodePollOptions {
        interval_seconds: Some(5.0),
        expires_in_seconds: Some(30.0),
        wait_before_first_poll: false,
        poll: Arc::new(|| {
            Box::pin(async { Ok(OAuthDeviceCodePollResult::<&'static str>::Pending) })
        }),
        signal,
    };
    let handle = tokio::spawn({
        let clock = Arc::clone(&clock);
        async move { poll_oauth_device_code_flow_with_clock(options, &*clock).await }
    });
    // 让 poll 先跑一次并进入等待。
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
    aborter.abort();
    let error = handle.await.unwrap().unwrap_err();
    assert_eq!(error.message, "Login cancelled");
    assert_eq!(error.name, "Error", "abortableSleep throws a plain Error");
}

#[tokio::test(start_paused = true)]
async fn times_out_with_the_short_message_without_slow_down() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::Pending,
        OAuthDeviceCodePollResult::Pending,
        OAuthDeviceCodePollResult::Pending,
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let error =
        poll_oauth_device_code_flow_with_clock(options(Some(2.0), Some(5.0), false, poll), &*clock)
            .await
            .unwrap_err();
    assert_eq!(error.message, "Device flow timed out");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 2000.0, 4000.0]);
}

#[tokio::test(start_paused = true)]
async fn times_out_with_the_slow_down_message_after_a_slow_down() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![OAuthDeviceCodePollResult::SlowDown {
        interval_seconds: None,
    }]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let error =
        poll_oauth_device_code_flow_with_clock(options(Some(2.0), Some(4.0), false, poll), &*clock)
            .await
            .unwrap_err();
    assert_eq!(
        error.message,
        "Device flow timed out after one or more slow_down responses. This is often caused by clock drift in WSL or VM environments. Please sync or restart the VM clock and try again."
    );
    // 最后一次睡眠被 remaining (4s) 截短：0 + min(7000, 4000) = 4s，循环条件立刻为假。
    assert_eq!(*times.lock().unwrap(), vec![0.0]);
}

#[tokio::test(start_paused = true)]
async fn defaults_to_a_five_second_interval() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::Pending,
        OAuthDeviceCodePollResult::Pending,
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let error =
        poll_oauth_device_code_flow_with_clock(options(None, Some(6.0), false, poll), &*clock)
            .await
            .unwrap_err();
    assert_eq!(error.message, "Device flow timed out");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 5000.0]);
}

#[tokio::test(start_paused = true)]
async fn enforces_the_one_second_minimum_interval() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::Pending,
        OAuthDeviceCodePollResult::Pending,
        OAuthDeviceCodePollResult::Pending,
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let error =
        poll_oauth_device_code_flow_with_clock(options(Some(0.0), Some(2.5), false, poll), &*clock)
            .await
            .unwrap_err();
    assert_eq!(error.message, "Device flow timed out");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 1000.0, 2000.0]);
}

#[tokio::test(start_paused = true)]
async fn propagates_failed_results_and_poll_errors() {
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![OAuthDeviceCodePollResult::Failed {
        message: "access_denied".into(),
    }]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);
    let error = poll_oauth_device_code_flow_with_clock(
        options(Some(2.0), Some(30.0), false, poll),
        &*clock,
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "access_denied");

    let poll: Arc<
        dyn Fn() -> kiana_ai::auth::oauth::device_code::OAuthDeviceCodePollFuture<&'static str>
            + Send
            + Sync,
    > = Arc::new(|| Box::pin(async { Err(AuthError::new("network down")) }));
    let error = poll_oauth_device_code_flow_with_clock(
        options(Some(2.0), Some(30.0), false, poll),
        &*clock,
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, "network down");
}

#[tokio::test(start_paused = true)]
async fn an_already_aborted_signal_cancels_before_the_first_poll() {
    let clock = TokioClock::new();
    let polls = Arc::new(AtomicUsize::new(0));
    let signal = AbortScope::new();
    signal.abort();
    let counter = Arc::clone(&polls);
    let options = OAuthDeviceCodePollOptions {
        interval_seconds: Some(1.0),
        expires_in_seconds: Some(30.0),
        wait_before_first_poll: false,
        poll: Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(completion("token")) })
        }),
        signal,
    };
    let error = poll_oauth_device_code_flow_with_clock(options, &*clock)
        .await
        .unwrap_err();
    assert_eq!(error.message, "Login cancelled");
    assert_eq!(polls.load(Ordering::SeqCst), 0);
}

#[tokio::test(start_paused = true)]
async fn abortable_sleep_rejects_with_the_configured_message() {
    let signal = AbortScope::new();
    signal.abort();
    let error = abortable_sleep(10_000, &signal, "Login cancelled")
        .await
        .unwrap_err();
    assert_eq!(error.message, "Login cancelled");
    assert_eq!(error.name, "Error");
}

#[tokio::test(start_paused = true)]
async fn abortable_sleep_resolves_after_the_delay() {
    let signal = AbortScope::new();
    abortable_sleep(0, &signal, "Login cancelled")
        .await
        .expect("sleep");
}

#[tokio::test(start_paused = true)]
async fn a_nan_interval_sleeps_one_millisecond_like_set_timeout() {
    // TS: intervalMs = Math.max(1000, Math.floor(NaN * 1000)) = NaN，随后
    // Math.min(NaN, remaining) = NaN，setTimeout(NaN) 取 1ms。Rust 的 f64::min 会丢掉 NaN，
    // 必须用 Math.min 语义。
    let clock = TokioClock::new();
    let times = Arc::new(Mutex::new(Vec::new()));
    let results = Arc::new(Mutex::new(vec![
        OAuthDeviceCodePollResult::Pending,
        completion("token"),
    ]));
    let poll = scripted_poll(Arc::clone(&clock), Arc::clone(&times), results);

    let result = poll_oauth_device_code_flow_with_clock(
        options(Some(f64::NAN), Some(30.0), false, poll),
        &*clock,
    )
    .await
    .expect("complete");
    assert_eq!(result, "token");
    assert_eq!(*times.lock().unwrap(), vec![0.0, 1.0]);
}
