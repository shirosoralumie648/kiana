//! `output.rs`：stdout 的唯一写线程。control 帧（结果、错误、ping、watch 事件）永远先于 bulk 命令输出；
//! bulk 有界——没有输出窗口的命令在未发送量过多时等待，像写满的管道一样拖慢命令。
//!
//! TS 源：packages/env/daemon/src/output.rs（原 daemon 本身就是 Rust；逐字移植）。
//! The single writer of stdout. Control frames (results, errors, pings, watch events) always go before bulk command
//! output, so a slow link delays output but never a cancel's result or a ping. Bulk output is bounded: readers of
//! commands without an output window wait while too much of it is unsent, which slows the command down like a full
//! pipe would.
//!
//! # 在系统中的位置
//! `main.rs` 先向 stdout 写一行 `KIANA-ENV <token>` 握手，然后创建一个 [`Output`] 并起一个线程跑
//! [`Output::run`]；此后所有帧都由这个线程写出。请求线程（`exec` / `pty` 的主循环、`watch`）和 ping 线程通过
//! [`Output::control`] / [`Output::bulk`] 把 `Frame` 交给它；命令管道的读取线程只调用 [`Output::wait_for_room`]
//! 施加背压，不直接入队。因此不同请求的帧不会在字节层面交错。
//! `frame.rs` 负责把 `Frame` 编码成线协议字节。
//!
//! # 两条队列与顺序保证
//! `Queues` 里有两条 FIFO：`control`（结果、错误、ping、watch 事件）和 `bulk`（命令与 PTY 输出）。`run` 每次
//! 先取 `control`，只有它为空时才取 `bulk`。于是慢链路只会延迟大块输出，不会延迟结果、错误、取消的响应或
//! ping；两条队列各自内部保持先进先出。优先级只发生在“选择下一帧”时：已经开始写的帧会写完，不能被抢占。
//!
//! # 有界性与背压
//! `bulk_bytes` 只统计 bulk 帧的 payload 字节。超过 `BULK_LIMIT`（4 MiB）时，没有输出窗口的命令读取线程在
//! [`Output::wait_for_room`] 里等待，效果等同“管道写满”：命令被拖慢，而不是把未发送数据无限堆在内存里。带
//! 输出窗口的 `exec` 不走这条路：它用 `unsent` 计数保证上一批帧全部写完（计数归零）后才投递下一批；一批可能
//! 包含多个事件帧（见 exec.rs）。
//!
//! # 锁与唤醒（并发约束）
//! - `queues` 互斥锁只保护两条队列、`bulk_bytes` 与 `closed`；`run` 在锁外写帧，所以慢速 stdout 不会阻塞
//!   `control` / `bulk` 入队（队列本身仍可增长，见下方注意事项）。
//! - `queued` 在入队时唤醒写线程；`drained` 在每帧写完（或写失败）以及 `close` 时唤醒等待者。
//! - [`Output::wait_for_room`] 每 100 ms 超时醒来重新检查：`stop` 是外部 `AtomicBool`，置位它不会触发条件
//!   变量通知，只能靠超时和后续的 `drained` 通知让等待者观察到。
//!
//! # 关闭与失败
//! `close` 把 `closed` 置位并唤醒两个条件变量。写线程会先把队列里已有的帧写完再退出（循环里先取帧，最后才
//! 检查 `closed`）。如果写 stdout 失败，`run` 自己调用 `close` 并返回；错误不向上传播，因为连接已经不可用。
//! `close` 之后仍可调用 `control` / `bulk`（不会 panic），但写线程退出后入队的帧不会再被写出。
//!
//! # 注意事项
//! - `bulk_bytes` 在帧**出队时**就减少（早于真正写出去），因此 [`Output::wait_for_room`] 看到的余量不包含
//!   正在写的那一帧；多个发送方各自在入队后才等待，所以未写出总量可能短暂超过上限。
//! - `bulk` 的 `unsent` 计数在写失败时也会减一，避免等待该帧的发送方永久等待。
//! - `run` 设计上只应有一个（`main.rs` 只起一个写线程）；多个 `run` 会竞争同一队列，帧的顺序将不再可预期。

use crate::frame::{self, Frame};
use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Unsent bulk bytes above which unwindowed command output waits.
const BULK_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Default)]
struct Queues {
    /// 优先级队列：结果、错误、ping、watch 事件，payload 通常很小。
    control: VecDeque<Frame>,
    /// 大块输出队列；第二项是该帧发送方共享的 `unsent` 计数（没有则为 `None`）。
    bulk: VecDeque<(Frame, Option<Arc<AtomicUsize>>)>,
    /// 仅统计 bulk 帧的 payload 字节，用于背压；不含帧头与 JSON 的长度。
    bulk_bytes: usize,
    /// `close` 之后为 true；写线程在两条队列都排空后退出。
    closed: bool,
}

/// 唯一的 stdout 写队列：所有线程把 `Frame` 投进来，一个写线程按优先级取出并写出。
///
/// 字段都是私有的，外部只能通过 `control` / `bulk` 入队、`wait_for_room` 施压、`close` 结束。所有方法都通过
/// `Mutex` 串行访问内部队列，因此 `Output` 可以被多个线程共享（用 `Arc`）。`Default` 得到空且未关闭的队列。
#[derive(Default)]
pub struct Output {
    queues: Mutex<Queues>,
    /// Signalled when a frame is queued or the writer stops.
    queued: Condvar,
    /// Signalled when bulk bytes were written.
    drained: Condvar,
}

impl Output {
    /// 入队一个 control 帧：结果、错误、ping 或 watch 事件。
    ///
    /// control 帧不受 `BULK_LIMIT` 限制，也不会阻塞；`run` 每次优先取它们，所以小回复不会被大输出堵住。
    /// 由于队列无界，高频事件（例如大量 watch 变更）会累积在内存里，调用方需自行控制生产速率。
    pub fn control(&self, frame: Frame) {
        let mut queues = self.queues.lock().unwrap();
        queues.control.push_back(frame);
        self.queued.notify_one();
    }

    /// Queue bulk output; `unsent` counts this sender's frames until they are written.
    ///
    /// 入队时把 `frame.payload.len()` 计入 `bulk_bytes`；`unsent` 计数器同时加一，写线程写完（或写失败）该帧时
    /// 减一。带输出窗口的 `exec` 用这个计数保证“上一批帧全部写完（计数归零）后才投递下一批”；一次 `take` 可能
    /// 产生多个事件帧，因此一批里可以有多帧同时在途。
    ///
    /// 本方法不会因为积压而阻塞：背压由发送方随后调用 [`Output::wait_for_room`] 实现（无窗口的 exec 与 pty
    /// 读取线程就是这么做的）。只调用 `bulk` 而不等待的发送方需要自己限制在途数据量。
    pub fn bulk(&self, frame: Frame, unsent: Option<Arc<AtomicUsize>>) {
        let mut queues = self.queues.lock().unwrap();
        queues.bulk_bytes += frame.payload.len();
        if let Some(counter) = &unsent {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        queues.bulk.push_back((frame, unsent));
        self.queued.notify_one();
    }

    /// Wait while more than the bulk limit is unsent, unless `stop` is set.
    ///
    /// 阻塞到“未写出的 bulk 字节”不超过 `BULK_LIMIT`，或者 `close` / `stop` 结束等待。
    ///
    /// 条件在锁内判断；等待使用 100 ms 超时，因为 `stop` 是一个普通 `AtomicBool`，置位它不会发条件变量通知，
    /// 超时让等待者能定期复查。正常情况下由写线程写完一帧后的 `drained.notify_all` 唤醒。`stop` 只影响等待，
    /// 不会清空队列；调用方返回后应结合自己的 `stop` 决定是否继续读取。
    pub fn wait_for_room(&self, stop: &AtomicBool) {
        let mut queues = self.queues.lock().unwrap();
        while queues.bulk_bytes > BULK_LIMIT && !queues.closed && !stop.load(Ordering::SeqCst) {
            queues = self
                .drained
                .wait_timeout(queues, Duration::from_millis(100))
                .unwrap()
                .0;
        }
    }

    /// 关闭队列：置 `closed`，唤醒写线程和所有等待者。
    ///
    /// 写线程会把已经入队的帧写完再退出；[`Output::wait_for_room`] 立即返回。`close` 之后仍可调用
    /// `control` / `bulk`（方法不会拒绝，也不会 panic），但写线程退出后入队的帧不会再被写出。`main.rs` 在
    /// 输入结束、清理完子进程后调用它并 join 写线程。
    pub fn close(&self) {
        let mut queues = self.queues.lock().unwrap();
        queues.closed = true;
        self.queued.notify_all();
        self.drained.notify_all();
    }

    /// Write frames to `writer` until closed or a write fails.
    ///
    /// 写线程主体：每轮先取 control 的最早一帧，没有才取 bulk 的最早一帧；取帧时持锁，真正写帧在锁外，
    /// 所以阻塞的 stdout 不会卡住入队者。写完对 bulk 的 `unsent` 减一并唤醒 `wait_for_room` 的等待者。
    ///
    /// 写失败（对端关闭、broken pipe 等）时关闭队列并返回；错误不向上传播。队列中尚未写出的帧留在内存里，
    /// 随后进程退出时一并丢弃。
    pub fn run(&self, writer: &mut impl Write) {
        loop {
            let (frame, unsent) = {
                let mut queues = self.queues.lock().unwrap();
                loop {
                    // 优先 control：慢链路上，大块输出不能挡住结果、错误、ping 与 watch 事件。
                    if let Some(frame) = queues.control.pop_front() {
                        break (frame, None);
                    }
                    // control 为空才取 bulk。出队时立刻减少 bulk_bytes，让发送方尽快恢复；真正的写可能仍要耗时。
                    if let Some((frame, unsent)) = queues.bulk.pop_front() {
                        queues.bulk_bytes -= frame.payload.len();
                        break (frame, unsent);
                    }
                    // 两条队列都空且已关闭：所有已入队的帧都写完了，退出。
                    if queues.closed {
                        return;
                    }
                    // 队列为空且未关闭：释放锁，等待入队或 close 的通知。
                    queues = self.queued.wait(queues).unwrap();
                }
            };
            let failed = frame::write_frame(writer, &frame).is_err();
            // 无论成功还是失败都要减计数并唤醒：等待这一帧的发送方不能被永久挂起。
            if let Some(counter) = unsent {
                counter.fetch_sub(1, Ordering::SeqCst);
            }
            self.drained.notify_all();
            if failed {
                // 连接已不可用：关闭队列让其他等待者退出，然后结束写线程。
                self.close();
                return;
            }
        }
    }
}

// 这些测试用真实线程和 Vec<u8> 作为 writer，锁定 control 优先、条件变量唤醒与 unsent 计数；不依赖 stdout。
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output_frame(kind: u8, id: u32) -> Frame {
        Frame::with_payload(kind, id, json!({}), vec![0u8; 4])
    }

    #[test]
    fn control_frames_go_before_queued_bulk_output() {
        let output = Output::default();
        output.bulk(output_frame(frame::EVENT, 1), None);
        output.bulk(output_frame(frame::EVENT, 2), None);
        // A result queued behind a long bulk stream still goes first.
        output.control(Frame::new(frame::RESULT, 3, json!({ "ok": true })));
        let mut written = Vec::new();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| output.run(&mut written));
            // Wait until the writer drained the queues, then stop it.
            for _ in 0..100 {
                if output.queues.lock().unwrap().bulk.is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(50));
            output.close();
            writer.join().unwrap();
        });
        let mut reader = written.as_slice();
        let first = crate::frame::read_frame(&mut reader).unwrap().unwrap();
        assert_eq!((first.kind, first.id), (frame::RESULT, 3));
    }

    #[test]
    fn wait_for_room_returns_once_the_bulk_bytes_are_written() {
        let output = Output::default();
        // A bulk frame larger than the 4 MiB limit blocks senders until the writer takes it.
        let big = Frame::with_payload(frame::EVENT, 1, json!({}), vec![0u8; BULK_LIMIT + 1]);
        output.bulk(big, None);
        let stop = AtomicBool::new(false);
        let waiting = std::thread::scope(|scope| {
            let ready = scope.spawn(|| {
                output.wait_for_room(&stop);
                true
            });
            // The writer takes the frame (bulk_bytes drops at dequeue), then room is available.
            let writer = scope.spawn(|| {
                let mut written = Vec::new();
                output.run(&mut written)
            });
            std::thread::sleep(Duration::from_millis(200));
            output.close();
            let ready = ready.join().unwrap();
            writer.join().unwrap();
            ready
        });
        assert!(waiting);
    }

    #[test]
    fn wait_for_room_stops_when_the_flag_is_set() {
        let output = Output::default();
        output.bulk(
            Frame::with_payload(frame::EVENT, 1, json!({}), vec![0u8; BULK_LIMIT + 1]),
            None,
        );
        let stop = AtomicBool::new(true);
        let started = std::time::Instant::now();
        output.wait_for_room(&stop);
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn close_wakes_a_waiting_writer() {
        let output = Output::default();
        let mut written = Vec::new();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| output.run(&mut written));
            std::thread::sleep(Duration::from_millis(50));
            output.close();
            writer.join().unwrap();
        });
        assert!(written.is_empty());
    }

    #[test]
    fn run_drains_control_and_bulk_in_order_and_decrements_unsent() {
        let output = Output::default();
        let counter = Arc::new(AtomicUsize::new(0));
        output.bulk(output_frame(frame::EVENT, 1), Some(counter.clone()));
        output.bulk(output_frame(frame::EVENT, 2), Some(counter.clone()));
        output.control(Frame::new(frame::RESULT, 9, json!({})));
        assert_eq!(counter.load(Ordering::SeqCst), 2);
        let mut written = Vec::new();
        std::thread::scope(|scope| {
            let writer = scope.spawn(|| output.run(&mut written));
            // Wait until everything queued was written, then close.
            for _ in 0..100 {
                if counter.load(Ordering::SeqCst) == 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            output.close();
            writer.join().unwrap();
        });
        assert_eq!(counter.load(Ordering::SeqCst), 0);
        let mut reader = written.as_slice();
        let mut ids = Vec::new();
        while let Ok(Some(next)) = crate::frame::read_frame(&mut reader) {
            ids.push(next.id);
        }
        assert_eq!(ids, [9, 1, 2], "control first, then bulk in order");
    }
}
