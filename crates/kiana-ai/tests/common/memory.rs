//! `test/memory-helpers.ts` 的 Rust 形态。
//!
//! TS 侧用 V8 的 `GCProfiler`/`getHeapStatistics` 量“一次操作分配了多少字节”以及
//! “WeakRef 是否被回收”（用于成本守卫与泄漏测试）。Rust 没有等价的堆统计，但有两个可移植的
//! 近似，且不引入新依赖：
//!
//! - [`allocated_bytes`]：用 `std::alloc::System` 全局分配器统计期间的**净分配字节**
//!   （分配增量，不含回收）。这正是 TS `allocatedBytes` 的主要用途（“这次调用的分配量级”）。
//! - [`is_collected`]：用 `Weak` 判断引用是否已被释放（对应 `WeakRef.deref() === undefined`）。
//!
//! 与 TS 的差异（如实记录）：
//! - TS 的 `allocatedBytes` 还加上“运行期间每次 GC 释放的字节”，Rust 侧只统计分配；
//!   因此 Rust 数值只会**更小**，断言必须写成“量级/上界”而不是逐字节相等。
//! - TS 需要 `--expose-gc` 才能强制回收；Rust 的 [`is_collected`] 需要对象先被 `drop`
//!   （没有强制 GC 的公开 API）。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 计数用分配器：包一层 `System`，只累计分配字节。
///
/// 只有显式安装它的测试二进制才会用到（`#[global_allocator]` 是每二进制独立的选择）。
pub struct CountingAllocator;

/// 累计分配字节（进程级）。
pub static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);

/// 当前存活字节（分配减释放）。
pub static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

/// 观测窗口内的存活字节峰值。
pub static PEAK_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

fn record_alloc(size: usize) {
    ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
    let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
    PEAK_LIVE_BYTES.fetch_max(live, Ordering::Relaxed);
}

fn record_dealloc(size: usize) {
    // 与 `System` 的分配/释放成对出现；`fetch_update` 防止计数在异常路径下回绕。
    let _ = LIVE_BYTES.try_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
        Some(live.saturating_sub(size))
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            record_alloc(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record_dealloc(layout.size());
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            if new_size > layout.size() {
                record_alloc(new_size - layout.size());
            } else {
                record_dealloc(layout.size() - new_size);
            }
        }
        new_ptr
    }
}

/// 开始一个存活字节峰值窗口：返回基线（随后用 [`peak_delta`] 读峰值增量）。
pub fn begin_peak() -> usize {
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    PEAK_LIVE_BYTES.store(baseline, Ordering::Relaxed);
    baseline
}

/// 窗口内的存活字节峰值增量。
pub fn peak_delta(baseline: usize) -> usize {
    PEAK_LIVE_BYTES
        .load(Ordering::Relaxed)
        .saturating_sub(baseline)
}

/// 当前存活字节。
pub fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// 期间总分配字节：`allocatedBytes(run)` 的近似。
///
/// 调用方需要在自己的测试二进制里安装 [`CountingAllocator`]（`#[global_allocator]`），
/// 否则返回值恒为 0——这也是与 TS 的差异之一（TS 无需安装，V8 自带统计）。
pub fn allocated_bytes(run: impl FnOnce()) -> usize {
    let before = ALLOCATED_BYTES.load(Ordering::Relaxed);
    run();
    ALLOCATED_BYTES
        .load(Ordering::Relaxed)
        .saturating_sub(before)
}

/// `isCollected(reference)`：目标是否已无强引用。
///
/// TS 会先强制 GC 再 `deref()`；Rust 侧由调用方 `drop` 强引用后再调用（见模块文档）。
pub fn is_collected<T>(reference: &std::sync::Weak<T>) -> bool {
    reference.strong_count() == 0
}

/// 构造一个 `Weak`，便于 `is_collected` 的调用方少写两行。
pub fn weak_pair<T>(value: T) -> (Arc<T>, std::sync::Weak<T>) {
    let strong = Arc::new(value);
    let weak = Arc::downgrade(&strong);
    (strong, weak)
}
