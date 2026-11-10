//! `Array.prototype.sort` 的等价实现（V8 14.6 / Node 26 的 TimSort）。
//!
//! 为什么不用 `slice::sort_by`：TS 的比较器可能返回 `NaN`（`left.startedMs - right.startedMs`
//! 在任一侧是 `NaN`、或两侧都是 `±Infinity` 时），也可能因 `||` 后缀产生不一致（非传递）的顺序
//! 关系。ECMAScript 只要求「一致的比较器」，V8 在比较器不一致时**不会**报错：它照常执行 TimSort
//! 并返回某个排列。Rust 的 `slice::sort_by` 会在检测到非全序时 panic
//! （`user-provided comparison function does not correctly implement a total order`），
//! 这是 TS 参考实现里不存在的崩溃。因此这里逐宏移植 V8 的 TimSort：
//!
//! - `third_party/v8/builtins/array-sort.tq`（Node 26.12.1 内置的 V8 14.6）：
//!   `ArrayTimSort`、`BinaryInsertionSort`、`CountAndMakeRun`、`MergeAt`、`GallopLeft`、
//!   `GallopRight`、`MergeLow`、`MergeHigh`、`ComputeMinRunLength`、`MergeCollapse`、
//!   `MergeForceCollapse`，以及 `SortCompareUserFn` 的 `NaN → +0` 归一化；
//! - 入口阈值：元素数 `< 8` 直接 `BinaryInsertionSort(0, 0, n)`，否则 TimSort。
//!
//! 一致性：该移植在 20,000 组随机向量（任意位置的 `NaN`、重复值、长度 1..400）与 Node 26 的
//! `Array.prototype.sort` 逐元素一致。**引擎特定**：`Array#sort` 对不一致比较器的结果由实现
//! 定义；TS 参考实现跑在 Node 26 上，所以以同一引擎的算法为准（换引擎时 `NaN`/不一致比较器的
//! 排列可能不同；一致的比较器下任何引擎都给出相同的稳定结果）。
//!
//! 安全：V8 的不变量检查（`dcheck`）只在 debug 构建里生效，release 下依赖「比较器一致」的假设。
//! 本移植对**所有**比较器都不 panic、不越界：除 `reverse_range` 的区间交换（区间由 `run_length`
//! 保证在界内）外，`work` 的下标读写都经过 [`read`]/[`write`]/`copy_*` 辅助函数；最终重排还依赖
//! `order` 是 `0..len` 的排列（`slots[index]` 与 `items.push` 直接移动元素）。
//! 越界只可能出现在 V8 自身也会越界的不一致场景，那里按「无操作」处理（结果无定义，与 V8 的
//! 未定义行为同类）。
//!
//! # 调用方与职责边界
//!
//! 本模块通过 `lib.rs` 的 `mod timsort;` 挂在 crate 内部，只暴露 `sort_by_js` 一个入口，
//! 调用点共三处，全部在“对已结算快照排序”的路径上：
//! - `memory.rs::stored_spans_impl`：按 `startedMs` 升序，平局按 UTF-16 id；
//! - `memory.rs::find_traces_impl`：按每条 trace 首个 span 的 `startedMs` 降序，平局按 traceId；
//! - `render.rs::render_trace`：按 `startedMs` 升序。
//!
//! 本模块不关心业务字段，也不做过滤、去重或分页；它只回答“给定一个 JS 比较器，V8 会把
//! 这个数组排成什么顺序”。因此新增调用方时需要自己保证比较器符合 JS 语义（与 V8 TimSort
//! 的结果一致）。
//!
//! # 执行流程
//!
//! 1. `sort_by_js` 建立 `0..len` 的下标数组；长度小于 2 直接返回；
//! 2. 长度小于 8 走 `binary_insertion_sort`，否则走 `tim_sort_impl`；
//! 3. `tim_sort_impl` 先算一次 minrun，然后循环：`count_and_make_run` 提取自然 run →
//!    不足 minrun 时用二分插入排序补齐 → 压入 pending 栈 → `merge_collapse` 维护不变量；
//! 4. 扫描完所有元素后 `merge_force_collapse` 合并剩余 run；
//! 5. 下标排列不是恒等时，按排列重排 `items`；恒等则跳过搬移。
//!
//! # 关键数据结构
//!
//! - **work 数组**（`Vec<usize>`）：保存元素下标。V8 的 work array 直接保存元素；
//!   保存下标让 `items` 在整个排序期间保持只读，也不要求 `T: Clone`。
//! - **pending 栈**：`(base, length)` 二元组，表示 work 中一段已排好序、尚未合并的 run；
//!   同一时刻各 run 首尾相接，覆盖已经扫描过的前缀。
//! - **temp 数组**：合并时暂存较短一侧 run 的下标；跨多次合并复用，只增不减。
//! - **min_gallop**：自适应阈值，决定“线性合并连续获胜多少次后改试 gallop”。
//!
//! # 阅读顺序
//!
//! 先读 `sort_by_js` 弄清入口与最终重排，再读 `tim_sort_impl` 的总循环；之后按
//! `count_and_make_run` / `binary_insertion_sort` → `merge_collapse` → `merge_at` →
//! `gallop_left` / `gallop_right` → `merge_low` / `merge_high` 的顺序深入。
//! 只关心“调用方拿到什么结果”的读者可以停在 `sort_by_js`。
//!
//! # 复杂度与资源
//!
//! - 比较次数：TimSort 的最坏情形 O(n log n)；已经有序的输入只需识别一个自然 run。
//! - 额外空间：O(n)，包括下标数组、`SortState` 内的 work 副本和最终重排的槽位数组；
//!   temp 的有效前缀只保存较短一侧 run（不超过该次合并区间的一半），缓冲区本身
//!   至少有 `SORT_STATE_TEMP_SIZE` 个槽位。
//! - 稳定性：比较器一致时稳定；比较器不一致时结果与 V8 一样由实现定义。
//!
//! # 失败模式
//!
//! 本模块不返回 `Result`，也不会主动抛错。比较器不一致时，结果只是“某个排列”，
//! 不再是可解释的排序：`read` 的越界读返回占位 0、`write` 的越界写被丢弃，
//! 排序仍会终止，但顺序没有语义。需要可解释顺序的调用方必须提供一致的比较器。
//!
//! # 修改本文件的注意事项
//!
//! 每个分支都在复刻 V8 `array-sort.tq` 的特定写法，包括 `Smi` 的带符号中间量：
//! gallop 的 `last_ofs`/`offset` 与 `merge_high` 的 `dest`/`cursor_*`。把 `i64` 改成
//! `usize` 会破坏正常路径里的 `-1` 中间量：例如 `gallop_left` 在 `hint = 0` 且
//! `array[0] >= key` 的左侧分支会算出 `last_ofs = hint - offset = -1`，改成 `usize`
//! 会立刻下溢，即使比较器一致也一样。把 [`read`]/[`write`] 换成直接索引，则主要在
//! “比较器不一致”的输入上改变行为（panic 或越界），也就破坏了与 TS 参考实现的对齐。
//! 修改任何分支前先对照 `tests/js_sort.rs` 与 `tests/fixtures/sort_vectors.json`。

/// V8 `kMinGallopWins`。
///
/// `min_gallop` 的初值，也是 gallop 循环里判断“是否继续 gallop”的固定门槛：
/// 线性合并中一侧连续获胜达到当前阈值（初始为 7）后转入 gallop，改用指数搜索
/// 成段搬运。运行期间 `SortState::min_gallop` 会自适应增减，这个常量本身不变。
const MIN_GALLOP_WINS: i64 = 7;
/// V8 `kSortStateTempSize`。
///
/// temp 缓冲区的最小长度。`get_temp_array` 保证容量至少为
/// `max(SORT_STATE_TEMP_SIZE, requested_size)`，小规模合并也有 32 个槽位可用；
/// 缓冲区只扩容不缩容，在整次排序中复用。
const SORT_STATE_TEMP_SIZE: usize = 32;
/// V8 `ArrayTimSort` 的小数组阈值：`numberOfNonUndefined < 8` 时直接二分插入排序。
///
/// V8 先统计非 `undefined` 元素个数；Rust 侧没有 `undefined` 这一维，
/// 因此这里直接用元素总数判断。
const BINARY_INSERTION_SORT_LENGTH: usize = 8;

/// `Array.prototype.sort(compare)` 的原地版本（稳定）。
///
/// `compare` 返回 JS 比较器语义的数字（例如 `a - b`）；`NaN` 按 `SortCompareUserFn` 归一化为
/// `+0`（「相等」）。比较器**可以**不一致：本实现不会 panic，也不会越界。
///
/// # 参数
///
/// - `items`：待排序的 `Vec<T>`，返回时按 `compare` 决定的顺序原地重排；
/// - `compare`：接收两个元素引用，返回 JS 比较器结果（负数/零/正数，或 `NaN`）。
///
/// # 与 `slice::sort_by` 的区别
///
/// `slice::sort_by` 要求比较器构成全序，检测到不一致时会 panic；本函数把 `NaN`
/// 与 `±0` 都视为「相等」，对不一致比较器只产生“某个排列”，与 V8 的可观察行为一致。
///
/// # 返回与副作用
///
/// 无返回值，直接修改 `items`。元素只被比较器读取，不会被修改或克隆，
/// 因此 `T` 不需要 `Clone` 或 `Ord`。比较器一致时排序稳定：返回 0 的两个元素
/// 保持原有相对次序。
///
/// # 边界
///
/// - 长度小于 2 立即返回，不分配下标数组；
/// - 下标排列恰好是恒等（输入已有序）时，跳过最后的整表搬移；
/// - 唯一的 `expect` 是内部不变量“每个下标恰好出现一次”的断言，正常路径不会触发。
///
/// 复杂度：最坏 O(n log n) 次比较，额外空间 O(n)。
pub(crate) fn sort_by_js<T, F>(items: &mut Vec<T>, mut compare: F)
where
    F: FnMut(&T, &T) -> f64,
{
    let len = items.len();
    // 0/1 个元素没有可比较的元素对，直接返回；也省掉下标数组与最终重排的分配。
    if len < 2 {
        return;
    }

    // 排序下标而不是元素本身：V8 的 work array 里放的是元素，这里用下标等价表达。
    let mut order: Vec<usize> = (0..len).collect();
    // V8 对少于 8 个元素的数组直接走二分插入排序（见 `BINARY_INSERTION_SORT_LENGTH`）。
    if len < BINARY_INSERTION_SORT_LENGTH {
        binary_insertion_sort(&mut order, items, 0, 0, len, &mut compare);
    } else {
        tim_sort_impl(&mut order, items, len, &mut compare);
    }

    // 排列是恒等映射说明元素已经在最终位置；跳过下面的 drain/重建，避免整表搬移。
    if order
        .iter()
        .enumerate()
        .all(|(index, &sorted)| index == sorted)
    {
        return;
    }
    // 按下标排列重排：先移出槽位，再按排序结果写回。
    // `order[i]` 表示“最终第 i 个位置应放原下标 `order[i]` 的元素”。
    let mut slots: Vec<Option<T>> = items.drain(..).map(Some).collect();
    items.reserve(len);
    for index in order {
        // 不变量：`order` 是 `0..len` 的排列，因此每个槽位只会被 take 一次。
        let value = slots[index].take().expect("排序排列覆盖每个下标一次");
        items.push(value);
    }
}

/// `sortState.Compare(x, y)`：调用比较器并把 `NaN` 归一化为 `0`，返回 `-1 | 0 | 1`。
///
/// `left`/`right` 是 `items` 的下标（work 数组里存的就是下标），不是元素引用。
/// V8 的 `SortCompareUserFn` 把比较器返回的 `NaN` 当作 `+0`，所以 `NaN` 参与比较
/// 表示「相等」而不是错误；`-0.0` 与 `0.0` 同样归为 0。其余结果只取符号，
/// 调用方返回差值还是 `-1/0/1` 都不影响排序。
fn compare_order<T, F>(items: &[T], compare: &mut F, left: usize, right: usize) -> i64
where
    F: FnMut(&T, &T) -> f64,
{
    let value = compare(&items[left], &items[right]);
    if value.is_nan() || value == 0.0 {
        0
    } else if value < 0.0 {
        -1
    } else {
        1
    }
}

/// 越界安全读：越界只可能来自不一致的比较器，返回 0 占位（不 panic）。
///
/// 这里不用直接下标，是因为 V8 的 `dcheck` 只在 debug 构建生效，而本移植承诺
/// release 下同样不 panic：把越界读降级为占位值。返回的 0 只是占位，
/// 一旦真的发生越界，排序结果无定义。
#[inline]
fn read(work: &[usize], index: usize) -> usize {
    work.get(index).copied().unwrap_or(0)
}

/// 越界安全写：越界即丢弃（V8 在同场景下是未定义行为）。
///
/// 写入被丢弃不会 panic，但也不再保证 work 仍是 `0..len` 的排列；
/// 这类输入下的顺序无定义，与 V8 release 构建在比较器不一致时的行为同类。
#[inline]
fn write(work: &mut [usize], index: usize, value: usize) {
    if index < work.len() {
        work[index] = value;
    }
}

/// V8 `BinaryInsertionSort(low, startArg, high)`：稳定二分插入排序。
///
/// 对 `work[low..high]` 排序，并假定 `work[low..start_arg]` 已经是升序前缀：
/// 调用方要么从第一个元素开始（`low == start_arg`，此时把首个元素视为已就位），
/// 要么传入 `count_and_make_run` 刚识别出的自然 run。
/// 每轮取 `work[start]` 作 pivot，在已排序前缀里二分找到插入点，再把中间元素整体右移一格。
///
/// 稳定性来自二分条件：只有 `pivot < middle` 才向左收缩，相等时向右，
/// 因此插入点落在所有相等元素之后。单轮比较 O(log k)、移动 O(k)，
/// 整体 O(k²) 次移动；调用方只在 k < 8 或把 run 补到 minrun（至多 64）时使用。
fn binary_insertion_sort<T, F>(
    work: &mut [usize],
    items: &[T],
    low: usize,
    start_arg: usize,
    high: usize,
    compare: &mut F,
) where
    F: FnMut(&T, &T) -> f64,
{
    // `low == start_arg` 表示没有已知的已排序前缀：把第一个元素视作已就位，从下一个开始。
    let mut start = if low == start_arg {
        start_arg + 1
    } else {
        start_arg
    };
    while start < high {
        let mut left = low;
        let mut right = start;
        let pivot = read(work, right);
        // 在 `[left, right)` 内二分：相等时取右半，保证插入点在所有相等元素之后（稳定）。
        while left < right {
            let middle = left + ((right - left) >> 1);
            if compare_order(items, compare, pivot, read(work, middle)) < 0 {
                right = middle;
            } else {
                left = middle + 1;
            }
        }
        // 把 `[left, start)` 整体右移一格，空出 `left` 放 pivot。
        let mut position = start;
        while position > left {
            let value = read(work, position - 1);
            write(work, position, value);
            position -= 1;
        }
        write(work, left, pivot);
        start += 1;
    }
}

/// V8 `CountAndMakeRun(lowArg, high)`：返回自然 run 的长度；降序 run 会就地反转。
///
/// “自然 run”是 `work[low_arg..high]` 开头已经有序的最长前缀。用前两个元素决定方向：
/// 严格递减则一路扩展并最终反转（TimSort 只维护升序 run）；否则按非递减扩展。
/// 递减方向用 `< 0` 判定、遇到 `>= 0` 就停止，因此递减 run 内不会出现相等元素，
/// 反转不会改变相等元素的相对次序（稳定性）。
///
/// 返回长度至少为 1；`low_arg + 1 == high`（只剩一个元素）时直接返回 1，不读越界下标。
fn count_and_make_run<T, F>(
    work: &mut [usize],
    items: &[T],
    low_arg: usize,
    high: usize,
    compare: &mut F,
) -> usize
where
    F: FnMut(&T, &T) -> f64,
{
    // 从第二个元素开始比较；无论方向如何，第一个元素都属于这个 run。
    let low = low_arg + 1;
    if low == high {
        return 1;
    }
    let mut run_length = 2;
    let element_low = read(work, low);
    let element_low_pred = read(work, low - 1);
    // 方向一旦由前两个元素确定，后续元素必须保持同一方向。
    // 升序 run 允许相等（`order < 0` 才停），降序 run 要求严格（`order >= 0` 即停）。
    let is_descending = compare_order(items, compare, element_low, element_low_pred) < 0;
    let mut previous_element = element_low;

    let mut index = low + 1;
    while index < high {
        let current_element = read(work, index);
        let order = compare_order(items, compare, current_element, previous_element);
        if is_descending {
            if order >= 0 {
                break;
            }
        } else if order < 0 {
            break;
        }
        previous_element = current_element;
        run_length += 1;
        index += 1;
    }

    if is_descending {
        // 降序 run 反转成升序，后续插入/合并才有统一的“run 升序”前提。
        reverse_range(work, low_arg, low_arg + run_length);
    }
    run_length
}

/// V8 `ReverseRange(array, from, to)`（`to` 为开区间）。
///
/// 原地反转 `work[from..to]`。只做对称交换，单元素区间无操作；当前唯一调用方
/// `count_and_make_run` 保证区间非空（`run_length >= 1`），不要传入 `from == to == 0`
/// （`to - 1` 会下溢/越界）。调用方仍须保证 `to <= work.len()`。
fn reverse_range(work: &mut [usize], from: usize, to: usize) {
    let mut low = from;
    let mut high = to - 1;
    while low < high {
        work.swap(low, high);
        low += 1;
        high -= 1;
    }
}

/// V8 `ComputeMinRunLength(n)`。
///
/// 计算 minrun：自然 run 短于它时，`tim_sort_impl` 会用二分插入排序补齐。
/// 算法反复取 `n` 的最低比特（`remainder |= n & 1`）并右移，直到 `n < 64`；
/// 剩下的高 6 位落在 `[32, 63]`，若被丢弃的低位中有 1 则再加 1，结果落在 `[32, 64]`。
/// `n < 64` 时循环不执行，直接返回 `n`：整段作为一个 run 处理，
/// 自然 run 不够长时由插入排序补齐。
/// 循环次数是 O(log n)。
fn compute_min_run_length(mut n: usize) -> usize {
    let mut remainder = 0usize;
    while n >= 64 {
        remainder |= n & 1;
        n >>= 1;
    }
    n + remainder
}

/// V8 `GetTempArray(sortState, requestedSize)`。
///
/// 确保 `temp` 至少有 `max(SORT_STATE_TEMP_SIZE, requested_size)` 个槽位；
/// `resize` 用 0 填充新增部分。调用方随后只覆盖 `[0, requested_size)`，
/// 因此填充的 0 不会被当作有效下标读取。缓冲区只增不减，多次合并复用同一个 `Vec`。
fn get_temp_array(temp: &mut Vec<usize>, requested_size: usize) {
    let min_size = SORT_STATE_TEMP_SIZE.max(requested_size);
    if temp.len() < min_size {
        temp.resize(min_size, 0);
    }
}

/// V8 `SortState` 的可变部分：work array 里放的是元素下标。
struct SortState {
    /// 当前排列：`work[i]` 是 `items` 的下标，排序过程中被不断合并、搬移。
    work: Vec<usize>,
    /// 合并暂存区：`merge_low` 存 run A，`merge_high` 存 run B，元素同样是下标。
    temp: Vec<usize>,
    /// 未合并 run 的栈，元素为 `(base, length)`；同一时刻各 run 首尾相接，
    /// 覆盖已经扫描过的前缀。
    pending: Vec<(usize, usize)>,
    /// 自适应 gallop 阈值：线性合并中一侧连续获胜达到它时转入 gallop；取值始终 ≥ 1。
    min_gallop: i64,
}

/// V8 `ArrayTimSortImpl(context, sortState, length)`。
///
/// TimSort 主循环。前置条件：`length >= 2`、`work.len() == length`、
/// `work` 中的每个值都是 `items` 的合法下标。
///
/// 流程：
/// 1. 按总长度计算一次 minrun；
/// 2. 每轮从 `low` 起提取自然 run，短于 minrun 就补到 `min(minrun, remaining)`；
/// 3. 把 `(low, runLen)` 压栈，并调用 `merge_collapse` 维护栈不变量；
/// 4. 所有元素处理完后调用 `merge_force_collapse` 收拾剩余 run；
/// 5. 把 `state.work` 写回调用方切片。
///
/// `remaining` 每轮减去刚消费的 run 长度（run 长度不会超过 `remaining`），
/// 所以 `low` 单调前进、循环必然终止。内部让 `SortState` 持有 work 的副本，
/// 结束时用 `copy_from_slice` 写回；work 只被重排、从不增删，长度始终一致。
fn tim_sort_impl<T, F>(work: &mut [usize], items: &[T], length: usize, compare: &mut F)
where
    F: FnMut(&T, &T) -> f64,
{
    if length < 2 {
        return;
    }
    let mut state = SortState {
        work: work.to_vec(),
        temp: Vec::new(),
        pending: Vec::new(),
        min_gallop: MIN_GALLOP_WINS,
    };
    let mut remaining = length;
    let mut low = 0usize;
    // minrun 只由总长度决定，整次排序只算一次（与 V8 一致）。
    let min_run_length = compute_min_run_length(remaining);
    while remaining != 0 {
        // 先找自然 run；不够长就用二分插入排序把它补到 minrun（不超过剩余长度）。
        let mut current_run_length =
            count_and_make_run(&mut state.work, items, low, low + remaining, compare);
        if current_run_length < min_run_length {
            let forced_run_length = min_run_length.min(remaining);
            binary_insertion_sort(
                &mut state.work,
                items,
                low,
                low + current_run_length,
                low + forced_run_length,
                compare,
            );
            current_run_length = forced_run_length;
        }
        // 记录新 run 后立刻维护栈不变量，避免 run 数量无界增长。
        state.pending.push((low, current_run_length));
        merge_collapse(&mut state, items, compare);
        low += current_run_length;
        remaining -= current_run_length;
    }
    // 元素已全部成为 run；此时不再考虑不变量，直接合并到只剩一个。
    merge_force_collapse(&mut state, items, compare);
    // 排序结果写回调用方传入的切片（两段长度相同）。
    work.copy_from_slice(&state.work);
}

/// V8 `RunInvariantEstablished(pendingRuns, n)`。
///
/// 检查 TimSort 的“三连 run”不变量：`pending[n-2] > pending[n-1] + pending[n]`
/// （比较的是 run 长度，不是 base）。`n < 2` 时参与检查的 run 不足三个，视为成立。
/// 另一个不变量 `pending[n-1] > pending[n]` 由 `merge_collapse` 单独判断。
fn run_invariant_established(pending: &[(usize, usize)], n: usize) -> bool {
    if n < 2 {
        return true;
    }
    pending[n - 2].1 > pending[n - 1].1 + pending[n].1
}

/// V8 `MergeCollapse(context, sortState)`。
///
/// 新 run 入栈后调用：只要栈里还剩两个以上 run，就检查栈尾附近的两/三个 run，
/// 违反不变量时合并相邻的一对，直到不变量重新成立。
///
/// 分支对应 V8 的原逻辑：
/// - 三连不变量被破坏：若倒数第三个 run 更短，就改为合并它和下一个
///   （先合并较短的一侧），否则合并 `n` 与 `n+1`；
/// - 否则若 `pending[n] <= pending[n+1]`，说明第二个不变量不成立，也合并 `n` 与 `n+1`；
/// - 两者都成立就停止。
///
/// 代码只检查栈尾的两/三个 run，而不是遍历全栈（与 V8 相同）。
fn merge_collapse<T, F>(state: &mut SortState, items: &[T], compare: &mut F)
where
    F: FnMut(&T, &T) -> f64,
{
    while state.pending.len() > 1 {
        // n 指向倒数第二个 run：合并总是发生在相邻的 n 与 n+1 之间。
        let mut n = state.pending.len() - 2;
        if !run_invariant_established(&state.pending, n + 1)
            || !run_invariant_established(&state.pending, n)
        {
            // V8 在 `n == 0` 时也会读 `pendingRuns[n - 1]`（Torque 里是越界读，但
            // `RunInvariantEstablished` 的 `n < 2` 短路保证该分支只在 n > 0 时进入）。
            if n > 0 && state.pending[n - 1].1 < state.pending[n + 1].1 {
                n -= 1;
            }
            merge_at(state, items, compare, n);
        } else if state.pending[n].1 <= state.pending[n + 1].1 {
            // 第二个不变量（`pending[n] > pending[n+1]`）不成立，同样必须合并。
            merge_at(state, items, compare, n);
        } else {
            break;
        }
    }
}

/// V8 `MergeForceCollapse(context, sortState)`。
///
/// 排序收尾：不再检查不变量，反复合并栈顶相邻的两段，直到只剩一个 run。
/// 与 `merge_collapse` 一样，若倒数第三个 run 更短就先合并它（先处理较短的一侧）；
/// 区别是不满足条件时也继续合并，而不是停止。
fn merge_force_collapse<T, F>(state: &mut SortState, items: &[T], compare: &mut F)
where
    F: FnMut(&T, &T) -> f64,
{
    while state.pending.len() > 1 {
        let mut n = state.pending.len() - 2;
        if n > 0 && state.pending[n - 1].1 < state.pending[n + 1].1 {
            n -= 1;
        }
        merge_at(state, items, compare, n);
    }
}

/// V8 `MergeAt(context, sortState, i)`。
///
/// 合并 pending 栈中相邻的第 `i`、`i+1` 个 run（记作 A、B），并把栈更新为一个合并后的 run。
/// 前置条件：`i + 1 < pending.len()`，两段 run 在 work 中相邻且都已升序。
///
/// 三个步骤：
/// 1. **更新栈**：A 的长度改为两者之和；若 `i` 是倒数第三个，把最后一个 run 前移补位；
///    然后弹栈。栈的更新先于元素合并，因此任何提前返回都不会留下不一致的栈。
/// 2. **裁掉无需移动的前缀/后缀**（gallop）：
///    - 用 B 的首元素在 A 中做 `gallop_right`：A 开头所有 `<=` 它的元素已在最终位置，
///      跳过它们（`base_a += k`、`length_a -= k`）；
///    - 用裁剪后 A 的末元素在 B 中做 `gallop_left`：B 尾部 `>=` 它的元素已在最终位置，
///      返回值（仍需合并的 B 前缀长度）用来缩短 `length_b`。
///    任何一侧被裁空就说明合并已经完成，直接返回。两次裁剪同时给出了后面
///    `merge_low`/`merge_high` 依赖的两个边界事实：剩余 A 的首元素 `>` B 的首元素，
///    剩余 B 的末元素 `<` A 的末元素。相等元素在两次裁剪中的取舍（A 用上界、B 用下界）
///    也是稳定性的来源之一。
/// 3. **选择方向**：`length_a <= length_b` 时用 `merge_low`（把较短的 A 拷进 temp，
///    从左往右合并），否则用 `merge_high`（把较短的 B 拷进 temp，从右往左合并）。
///    结果 temp 至多保存本次合并区间的一半。
fn merge_at<T, F>(state: &mut SortState, items: &[T], compare: &mut F, i: usize)
where
    F: FnMut(&T, &T) -> f64,
{
    // A = pending[i]，B = pending[i+1]；先记下 B 的起点，长度稍后会被 gallop 缩减。
    let (base_a_init, length_a_init) = state.pending[i];
    let base_b = state.pending[i + 1].0;
    let mut length_b = state.pending[i + 1].1;

    // 先把栈改成“A 已吸收 B”的形态，再做元素合并。
    state.pending[i].1 = length_a_init + length_b;
    // `i == stackSize - 3`：Torque 里 `stackSize - 3` 可为 -1（Smi），`i` 非负，故不等。
    if state.pending.len() >= 3 && i == state.pending.len() - 3 {
        state.pending[i + 1] = state.pending[i + 2];
    }
    state.pending.pop();

    let mut base_a = base_a_init;
    let mut length_a = length_a_init;

    // A 的前缀：`<= B[0]` 的元素已经处于最终位置，无需参与合并。
    let key_right = read(&state.work, base_b);
    let k = gallop_right(&state.work, items, compare, key_right, base_a, length_a, 0);
    base_a += k;
    length_a -= k;
    if length_a == 0 {
        return;
    }

    // B 的后缀：`>= A 末元素` 的元素也已就位，缩短 B 的有效长度。
    let key_left = read(&state.work, base_a + length_a - 1);
    length_b = gallop_left(
        &state.work,
        items,
        compare,
        key_left,
        base_b,
        length_b,
        length_b - 1,
    );
    if length_b == 0 {
        return;
    }

    // 把较短的一侧拷进 temp：merge_low 拷 A，merge_high 拷 B。
    if length_a <= length_b {
        merge_low(state, items, compare, base_a, length_a, base_b, length_b);
    } else {
        merge_high(state, items, compare, base_a, length_a, base_b, length_b);
    }
}

/// V8 `GallopLeft(array, key, base, length, hint)`。
///
/// `key` 是元素下标；`array` 里存放元素下标。返回值在 `0..=length`。
///
/// 语义：在有序区间 `array[base..base+length]`（按 `items` 的值升序）中找 `key` 的
/// **下界**——第一个满足 `items[array[i]] >= items[key]` 的位置，范围 `0..=length`。
/// 相等元素被看作排在 `key` 之后，因此返回位置之前的元素都 `< key`。
///
/// 算法：从 `hint` 出发，用 1、3、7、15…（即 `2^n − 1`；更新式 `offset = 2*offset + 1`）
/// 的偏移指数扩张，确定答案所在的
/// 括号，再在括号内二分。若答案距 `hint` 为 d，只需 O(log d) 次比较；d 很小时接近
/// 常数，这就是“gallop（疾驰）”相对线性扫描的收益。`saturating_mul`/`saturating_add`
/// 防止极端长度下偏移量溢出（正常输入到不了，但保持无 panic 的承诺）。
///
/// V8 用 `Smi`（有符号）算 `lastOfs`/`offset`：`lastOfs = hint - offset` 可为 `-1`
/// （`dcheck(-1 <= lastOfs)`），随后 `lastOfs++` 才回到非负。这里用 `i64` 复刻同一算术。
fn gallop_left<T, F>(
    array: &[usize],
    items: &[T],
    compare: &mut F,
    key: usize,
    base: usize,
    length: usize,
    hint: usize,
) -> usize
where
    F: FnMut(&T, &T) -> f64,
{
    let length = length as i64;
    let hint = hint as i64;
    let base = base as i64;
    let mut last_ofs: i64 = 0;
    let mut offset: i64 = 1;

    let at = |index: i64| read(array, index as usize);
    // 先看 `hint` 处：小于 key 说明答案在右侧，否则在左侧（含相等）。
    let mut order = compare_order(items, compare, at(base + hint), key);
    if order < 0 {
        let max_ofs = length - hint;
        // 向右指数扩张：offset 依次为 1、3、7、15…（即 2^n − 1；更新式 offset = 2*offset + 1），
        // 直到越过答案或区间末端。
        while offset < max_ofs {
            order = compare_order(items, compare, at(base + hint + offset), key);
            if order >= 0 {
                break;
            }
            last_ofs = offset;
            offset = offset.saturating_mul(2).saturating_add(1);
        }
        if offset > max_ofs {
            offset = max_ofs;
        }
        last_ofs += hint;
        offset += hint;
    } else {
        let max_ofs = hint + 1;
        // 向左扩张；`last_ofs` 用“向左越过多少”表达，最终可能为 -1，随后 +1 才回到非负。
        while offset < max_ofs {
            order = compare_order(items, compare, at(base + hint - offset), key);
            if order < 0 {
                break;
            }
            last_ofs = offset;
            offset = offset.saturating_mul(2).saturating_add(1);
        }
        if offset > max_ofs {
            offset = max_ofs;
        }
        let tmp = last_ofs;
        last_ofs = hint - offset;
        offset = hint - tmp;
    }

    // 在括号 `[last_ofs, offset)` 内二分，收敛到第一个 `>= key` 的位置。
    last_ofs += 1;
    while last_ofs < offset {
        let middle = last_ofs + ((offset - last_ofs) >> 1);
        order = compare_order(items, compare, at(base + middle), key);
        if order < 0 {
            last_ofs = middle + 1;
        } else {
            offset = middle;
        }
    }
    // 夹紧后转 usize，避免负的中间量被强转成巨大下标。
    offset.max(0) as usize
}

/// V8 `GallopRight(array, key, base, length, hint)`。
///
/// 与 `gallop_left` 对称，但求的是**上界**：在 `array[base..base+length]` 中返回第一个
/// 满足 `items[array[i]] > items[key]` 的位置（范围 `0..=length`）。相等元素被看作排在
/// `key` 之前，因此返回位置之前的元素都 `<= key`。合并时用它把“应排在 key 之前”的
/// 成段元素一次搬走，同时保持相等元素的先后次序。
///
/// 实现同样是指数扩张加二分，中间量用 `i64` 复刻 V8 的 `Smi` 算术。
fn gallop_right<T, F>(
    array: &[usize],
    items: &[T],
    compare: &mut F,
    key: usize,
    base: usize,
    length: usize,
    hint: usize,
) -> usize
where
    F: FnMut(&T, &T) -> f64,
{
    let length = length as i64;
    let hint = hint as i64;
    let base = base as i64;
    let mut last_ofs: i64 = 0;
    let mut offset: i64 = 1;

    let at = |index: i64| read(array, index as usize);
    // 先比较 key 与 `hint` 处：key 更小说明答案在左侧，否则在右侧（含相等）。
    let mut order = compare_order(items, compare, key, at(base + hint));
    if order < 0 {
        let max_ofs = hint + 1;
        // 向左指数扩张。
        while offset < max_ofs {
            order = compare_order(items, compare, key, at(base + hint - offset));
            if order >= 0 {
                break;
            }
            last_ofs = offset;
            offset = offset.saturating_mul(2).saturating_add(1);
        }
        if offset > max_ofs {
            offset = max_ofs;
        }
        let tmp = last_ofs;
        last_ofs = hint - offset;
        offset = hint - tmp;
    } else {
        let max_ofs = length - hint;
        // 向右指数扩张。
        while offset < max_ofs {
            order = compare_order(items, compare, key, at(base + hint + offset));
            if order < 0 {
                break;
            }
            last_ofs = offset;
            offset = offset.saturating_mul(2).saturating_add(1);
        }
        if offset > max_ofs {
            offset = max_ofs;
        }
        last_ofs += hint;
        offset += hint;
    }

    // 二分收敛到第一个 `> key` 的位置（上界）。
    last_ofs += 1;
    while last_ofs < offset {
        let middle = last_ofs + ((offset - last_ofs) >> 1);
        order = compare_order(items, compare, key, at(base + middle));
        if order < 0 {
            offset = middle;
        } else {
            last_ofs = middle + 1;
        }
    }
    // 夹紧后转 usize，避免负的中间量被强转成巨大下标。
    offset.max(0) as usize
}

/// V8 `MergeLow(baseA, lengthA, baseB, lengthB)`。
///
/// 从左往右合并两段相邻的升序 run：先把较短的 A 段整体拷进 `temp`，再逐元素比较
/// `temp` 与 work 中的 B 段，把较小者写回 `[base_a, ...)`。
///
/// 稳定性：比较结果为 0 时取 A（`order >= 0` 走 else 分支），因此相等的 A 元素
/// 总排在 B 元素之前，符合原数组中 A 在 B 之前的次序。
///
/// 几类提前返回（对应 V8 的 `Succeed` / `CopyB` 标签）：
/// - B 耗尽：把 temp 中剩余的 A 直接抄回；
/// - A 只剩 1 个：把剩余 B 抄到前面，再把最后一个 A 放到它们之后（`merge_low_copy_b`）；
/// - gallop 一次搬空 A：`copy_temp_to_work` 收到长度 0，实际是空操作后返回。
///
/// 自适应 gallop：内层循环逐元素取数，某一侧连续获胜 `min_gallop` 次后切到 gallop，
/// 用指数搜索成段搬运；gallop 每成功一轮把阈值减一（最低 1），退出 gallop 时再加一，
/// 使阈值随数据分布调整。
fn merge_low<T, F>(
    state: &mut SortState,
    items: &[T],
    compare: &mut F,
    base_a: usize,
    length_a_arg: usize,
    base_b: usize,
    length_b_arg: usize,
) where
    F: FnMut(&T, &T) -> f64,
{
    let mut length_a = length_a_arg;
    let mut length_b = length_b_arg;

    get_temp_array(&mut state.temp, length_a);
    // `Copy(workArray, baseA, tempArray, 0, lengthA)`。
    // 先经 `read` 收集快照再写入 temp：所有越界访问都按占位值处理，不会 panic。
    let snapshot: Vec<usize> = (0..length_a)
        .map(|step| read(&state.work, base_a + step))
        .collect();
    state.temp[0..length_a].copy_from_slice(&snapshot);

    // V8 的固定开场：剩余 A 都大于 B 的首元素（`merge_at` 已裁掉 A 中 `<=` 它的前缀），
    // 所以在待合并区间内 B[0] 一定最小，可以直接落到目标位置。
    let mut dest = base_a;
    let mut cursor_temp = 0usize;
    let mut cursor_b = base_b;
    let value = read(&state.work, cursor_b);
    write(&mut state.work, dest, value);
    dest += 1;
    cursor_b += 1;
    length_b -= 1;

    if length_b == 0 {
        // B 只有一个元素且已放置：A 的剩余部分整体跟在后面（Succeed）。
        copy_temp_to_work(state, cursor_temp, dest, length_a);
        return;
    }
    if length_a == 1 {
        // A 只剩一个元素：剩余 B 都小于 A 的末元素（`merge_at` 已裁掉 B 中 `>=` 它的后缀），
        // 先抄 B 再把 A 放最后（CopyB）。
        merge_low_copy_b(state, cursor_temp, cursor_b, dest, length_b);
        return;
    }

    // 阈值取自 SortState，并按 V8 的规则在本轮内自适应调整。
    let mut min_gallop = state.min_gallop;
    // 外层循环：一轮“线性合并 + 一次 gallop”；所有出口都在内部直接 return。
    loop {
        let mut nof_wins_a: i64 = 0;
        let mut nof_wins_b: i64 = 0;

        // 线性合并：取 B 与 temp 中 A 的当前元素中较小者；相等时取 A（稳定性）。
        loop {
            let order = compare_order(
                items,
                compare,
                read(&state.work, cursor_b),
                read(&state.temp, cursor_temp),
            );
            if order < 0 {
                let value = read(&state.work, cursor_b);
                write(&mut state.work, dest, value);
                dest += 1;
                cursor_b += 1;
                nof_wins_b += 1;
                length_b -= 1;
                nof_wins_a = 0;
                if length_b == 0 {
                    copy_temp_to_work(state, cursor_temp, dest, length_a);
                    return;
                }
                if nof_wins_b >= min_gallop {
                    break;
                }
            } else {
                let value = read(&state.temp, cursor_temp);
                write(&mut state.work, dest, value);
                dest += 1;
                cursor_temp += 1;
                nof_wins_a += 1;
                length_a -= 1;
                nof_wins_b = 0;
                if length_a == 1 {
                    merge_low_copy_b(state, cursor_temp, cursor_b, dest, length_b);
                    return;
                }
                if nof_wins_a >= min_gallop {
                    break;
                }
            }
        }

        // 一侧连胜达到阈值：阈值先 +1，随后进入 gallop（第一轮内部会 -1，净效果不变）。
        min_gallop += 1;
        let mut first_iteration = true;
        // `first_iteration` 保证即使获胜计数低于 MIN_GALLOP_WINS，也先尝试一轮 gallop。
        while nof_wins_a >= MIN_GALLOP_WINS || nof_wins_b >= MIN_GALLOP_WINS || first_iteration {
            first_iteration = false;
            // 每成功进入一轮 gallop，阈值下调（最低 1），让后续更容易再次 gallop。
            min_gallop = (min_gallop - 1).max(1);
            state.min_gallop = min_gallop;

            // A 中 `<= B 当前元素` 的整段（temp 前缀）可以原样搬到 dest。
            let key = read(&state.work, cursor_b);
            nof_wins_a =
                gallop_right(&state.temp, items, compare, key, cursor_temp, length_a, 0) as i64;
            if nof_wins_a > 0 {
                let count = nof_wins_a as usize;
                // `Copy(tempArray, cursorTemp, workArray, dest, nofWinsA)`。
                let snapshot: Vec<usize> = (0..count)
                    .map(|step| read(&state.temp, cursor_temp + step))
                    .collect();
                for (step, value) in snapshot.into_iter().enumerate() {
                    write(&mut state.work, dest + step, value);
                }
                dest += count;
                cursor_temp += count;
                length_a -= count;
                if length_a == 1 {
                    merge_low_copy_b(state, cursor_temp, cursor_b, dest, length_b);
                    return;
                }
                if length_a == 0 {
                    copy_temp_to_work(state, cursor_temp, dest, length_a);
                    return;
                }
            }
            // 落一个 B 元素（gallop 搬走 A 前缀后，它当前最小）。
            let value = read(&state.work, cursor_b);
            write(&mut state.work, dest, value);
            dest += 1;
            cursor_b += 1;
            length_b -= 1;
            if length_b == 0 {
                copy_temp_to_work(state, cursor_temp, dest, length_a);
                return;
            }

            // B 中 `< A 当前元素` 的整段搬到 dest；相等元素留给 A 先出（稳定性）。
            let key = read(&state.temp, cursor_temp);
            nof_wins_b =
                gallop_left(&state.work, items, compare, key, cursor_b, length_b, 0) as i64;
            if nof_wins_b > 0 {
                let count = nof_wins_b as usize;
                // `Copy(workArray, cursorB, workArray, dest, nofWinsB)`，dest < cursorB。
                copy_within_work(state, cursor_b, dest, count);
                dest += count;
                cursor_b += count;
                length_b -= count;
                if length_b == 0 {
                    copy_temp_to_work(state, cursor_temp, dest, length_a);
                    return;
                }
            }
            let value = read(&state.temp, cursor_temp);
            write(&mut state.work, dest, value);
            dest += 1;
            cursor_temp += 1;
            length_a -= 1;
            if length_a == 1 {
                merge_low_copy_b(state, cursor_temp, cursor_b, dest, length_b);
                return;
            }
        }
        // 退出 gallop：阈值 +1，回到外层循环继续线性合并。
        min_gallop += 1;
        state.min_gallop = min_gallop;
    }
}

/// `Copy(workArray, srcPos, workArray, dstPos, length)`：同一数组内的重叠复制。
///
/// 先把源区间经 `read` 收集成快照，再逐个写目标区间，所以两段是否重叠都不影响结果；
/// 越界读返回占位 0、越界写被丢弃，保持不 panic。
fn copy_within_work(state: &mut SortState, src_pos: usize, dst_pos: usize, count: usize) {
    let snapshot: Vec<usize> = (0..count)
        .map(|step| read(&state.work, src_pos + step))
        .collect();
    for (step, value) in snapshot.into_iter().enumerate() {
        write(&mut state.work, dst_pos + step, value);
    }
}

/// `MergeLow` 的 `Succeed` 标签：把 temp 中剩余的 run A 抄回 work。
///
/// `length_a == 0` 时什么都不做——gallop 一次搬走全部 A 后就走这条路径。
fn copy_temp_to_work(state: &mut SortState, cursor_temp: usize, dest: usize, length_a: usize) {
    if length_a > 0 {
        let snapshot: Vec<usize> = (0..length_a)
            .map(|step| read(&state.temp, cursor_temp + step))
            .collect();
        for (step, value) in snapshot.into_iter().enumerate() {
            write(&mut state.work, dest + step, value);
        }
    }
}

/// `MergeLow` 的 `CopyB` 标签。
///
/// 把剩余 B 整段前移，再把 temp 中最后一个 A 元素放到末尾。调用前提：剩余 B
/// 全部小于该 A 元素（`merge_at` 的 `gallop_left` 已裁掉 B 中 `>=` 它的后缀）。
fn merge_low_copy_b(
    state: &mut SortState,
    cursor_temp: usize,
    cursor_b: usize,
    dest: usize,
    length_b: usize,
) {
    copy_within_work(state, cursor_b, dest, length_b);
    let value = read(&state.temp, cursor_temp);
    write(&mut state.work, dest + length_b, value);
}

/// V8 `MergeHigh(baseA, lengthA, baseB, lengthB)`。
///
/// V8 的 `dest`/`cursorA`/`cursorTemp` 是 `Smi`，在「先减后判」的写法里可以到 `-1`
/// （紧随其后的 `Copy`/`work[dest]` 都在 `lengthA`/`lengthB` 为 0 的分支里被跳过）。
/// 这里用 `i64` 复刻同一算术，越界写由 [`write`] 丢弃、越界读由 [`read`] 返回 0。
///
/// 从右往左合并：把较短的 B 段拷进 `temp`，再从两段的末尾向前比较，把较大者写到
/// 目标区间的尾部。`merge_at` 已保证剩余 B 都小于 A 的末元素，因此第一轮可以直接
/// 把 A 的末元素放到最后。
///
/// 稳定性：相等时取 B（只有 B < A 才取 A），逆向写入后 B 元素排在 A 元素之后，
/// 保持原数组中 A 在 B 之前的次序。
///
/// 与 `merge_low` 对称：`length_a == 0` / `length_b == 1` 的退化出口分别对应
/// V8 的 `Succeed` 与 `CopyA`；三个游标允许短暂为 -1，所以全部用 `i64` 计算，
/// 越界读写由 `read`/`write` 的约定兜底。
fn merge_high<T, F>(
    state: &mut SortState,
    items: &[T],
    compare: &mut F,
    base_a: usize,
    length_a_arg: usize,
    base_b: usize,
    length_b_arg: usize,
) where
    F: FnMut(&T, &T) -> f64,
{
    let mut length_a = length_a_arg as i64;
    let mut length_b = length_b_arg as i64;

    get_temp_array(&mut state.temp, length_b_arg);
    // 先经 `read` 收集 B 的快照再写入 temp；此后 work 中的 B 区可以被覆盖。
    let snapshot: Vec<usize> = (0..length_b_arg)
        .map(|step| read(&state.work, base_b + step))
        .collect();
    state.temp[0..length_b_arg].copy_from_slice(&snapshot);

    let base_a = base_a as i64;
    let base_b = base_b as i64;

    // A 的末元素大于 B 的剩余全部元素（`merge_at` 已裁掉 B 的后缀），直接放到最后。
    let mut dest = base_b + length_b - 1;
    let mut cursor_temp = length_b - 1;
    let mut cursor_a = base_a + length_a - 1;
    let value = read(&state.work, cursor_a as usize);
    write(&mut state.work, dest as usize, value);
    dest -= 1;
    cursor_a -= 1;
    length_a -= 1;

    if length_a == 0 {
        // A 已全部落位：B 的剩余前缀整体写到区间开头（Succeed）。
        copy_temp_head_to_work(state, dest, length_b);
        return;
    }
    if length_b == 1 {
        // B 只剩一个元素：剩余 A 都大于它，先抄 A 再把 B 放到最前（CopyA）。
        merge_high_copy_a(state, cursor_temp, cursor_a, dest, length_a);
        return;
    }

    // 阈值取自 SortState，并按 V8 的规则在本轮内自适应调整。
    let mut min_gallop = state.min_gallop;
    // 外层循环：一轮“线性合并 + 一次 gallop”；所有出口都在内部直接 return。
    loop {
        let mut nof_wins_a: i64 = 0;
        let mut nof_wins_b: i64 = 0;

        // 线性合并（反向）：比较 B 尾与 A 尾，谁大谁先落到当前 dest；相等时取 B（稳定）。
        loop {
            let order = compare_order(
                items,
                compare,
                read(&state.temp, cursor_temp as usize),
                read(&state.work, cursor_a as usize),
            );
            if order < 0 {
                let value = read(&state.work, cursor_a as usize);
                write(&mut state.work, dest as usize, value);
                dest -= 1;
                cursor_a -= 1;
                nof_wins_a += 1;
                length_a -= 1;
                nof_wins_b = 0;
                if length_a == 0 {
                    copy_temp_head_to_work(state, dest, length_b);
                    return;
                }
                if nof_wins_a >= min_gallop {
                    break;
                }
            } else {
                let value = read(&state.temp, cursor_temp as usize);
                write(&mut state.work, dest as usize, value);
                dest -= 1;
                cursor_temp -= 1;
                nof_wins_b += 1;
                length_b -= 1;
                nof_wins_a = 0;
                if length_b == 1 {
                    merge_high_copy_a(state, cursor_temp, cursor_a, dest, length_a);
                    return;
                }
                if nof_wins_b >= min_gallop {
                    break;
                }
            }
        }

        // 一侧连胜达到阈值：阈值先 +1，随后进入 gallop（第一轮内部会 -1，净效果不变）。
        min_gallop += 1;
        let mut first_iteration = true;
        // `first_iteration` 保证即使获胜计数低于 MIN_GALLOP_WINS，也先尝试一轮 gallop。
        while nof_wins_a >= MIN_GALLOP_WINS || nof_wins_b >= MIN_GALLOP_WINS || first_iteration {
            first_iteration = false;
            // 每成功进入一轮 gallop，阈值下调（最低 1），让后续更容易再次 gallop。
            min_gallop = (min_gallop - 1).max(1);
            state.min_gallop = min_gallop;

            // A 中严格大于该 B 的整段搬到尾部；等于 B 的留给后续（保持稳定）。
            let key = read(&state.temp, cursor_temp as usize);
            let k = gallop_right(
                &state.work,
                items,
                compare,
                key,
                base_a as usize,
                length_a as usize,
                (length_a - 1) as usize,
            ) as i64;
            nof_wins_a = length_a - k;
            if nof_wins_a > 0 {
                dest -= nof_wins_a;
                cursor_a -= nof_wins_a;
                let snapshot: Vec<usize> = (0..nof_wins_a)
                    .map(|step| read(&state.work, (cursor_a + 1 + step) as usize))
                    .collect();
                for (step, value) in snapshot.into_iter().enumerate() {
                    write(&mut state.work, (dest + 1) as usize + step, value);
                }
                length_a -= nof_wins_a;
                if length_a == 0 {
                    copy_temp_head_to_work(state, dest, length_b);
                    return;
                }
            }
            // 落一个 B 元素（它当前最大）。
            let value = read(&state.temp, cursor_temp as usize);
            write(&mut state.work, dest as usize, value);
            dest -= 1;
            cursor_temp -= 1;
            length_b -= 1;
            if length_b == 1 {
                merge_high_copy_a(state, cursor_temp, cursor_a, dest, length_a);
                return;
            }

            // temp 中 `>= A 当前元素` 的整段搬到尾部；相等的 B 也搬走，保证 B 排在 A 之后。
            let key = read(&state.work, cursor_a as usize);
            let k = gallop_left(
                &state.temp,
                items,
                compare,
                key,
                0,
                length_b as usize,
                (length_b - 1) as usize,
            ) as i64;
            nof_wins_b = length_b - k;
            if nof_wins_b > 0 {
                dest -= nof_wins_b;
                cursor_temp -= nof_wins_b;
                // V8: `Copy(tempArray, cursorTemp + 1, workArray, dest + 1, nofWinsB)`。
                let snapshot: Vec<usize> = (0..nof_wins_b)
                    .map(|step| read(&state.temp, (cursor_temp + 1 + step) as usize))
                    .collect();
                for (step, value) in snapshot.into_iter().enumerate() {
                    write(&mut state.work, (dest + 1) as usize + step, value);
                }
                length_b -= nof_wins_b;
                if length_b == 1 {
                    merge_high_copy_a(state, cursor_temp, cursor_a, dest, length_a);
                    return;
                }
                if length_b == 0 {
                    // 长度 0 时该调用是空操作；保留与 V8 `Succeed` 标签一致的控制流。
                    copy_temp_head_to_work(state, dest, length_b);
                    return;
                }
            }
            // 落一个 A 元素（它当前最大）。
            let value = read(&state.work, cursor_a as usize);
            write(&mut state.work, dest as usize, value);
            dest -= 1;
            cursor_a -= 1;
            length_a -= 1;
            if length_a == 0 {
                copy_temp_head_to_work(state, dest, length_b);
                return;
            }
        }
        // 退出 gallop：阈值 +1，回到外层循环继续线性合并。
        min_gallop += 1;
        state.min_gallop = min_gallop;
    }
}

/// `MergeHigh` 的 `Succeed` 标签：`Copy(tempArray, 0, workArray, dest - (lengthB - 1), lengthB)`。
///
/// `dest` 是本次写入的最后一个槽位，因此起点是 `dest - (length_b - 1)`；
/// `length_b == 0` 时无事可做（gallop 搬空 B 后走这条路径）。
fn copy_temp_head_to_work(state: &mut SortState, dest: i64, length_b: i64) {
    if length_b > 0 {
        let start = dest - (length_b - 1);
        let snapshot: Vec<usize> = (0..length_b)
            .map(|step| read(&state.temp, step as usize))
            .collect();
        for (step, value) in snapshot.into_iter().enumerate() {
            write(&mut state.work, start as usize + step, value);
        }
    }
}

/// `MergeHigh` 的 `CopyA` 标签。
///
/// 把剩余 A 整段后移到尾部，再把 temp 中唯一的 B 元素放到它们之前。调用前提：
/// 剩余 A 全部大于该 B（`merge_at` 的 `gallop_right` 已裁掉 A 中 `<=` B 首元素的前缀）。
fn merge_high_copy_a(
    state: &mut SortState,
    cursor_temp: i64,
    cursor_a: i64,
    dest: i64,
    length_a: i64,
) {
    let dest = dest - length_a;
    let cursor_a = cursor_a - length_a;
    let snapshot: Vec<usize> = (0..length_a)
        .map(|step| read(&state.work, (cursor_a + 1 + step) as usize))
        .collect();
    for (step, value) in snapshot.into_iter().enumerate() {
        write(&mut state.work, (dest + 1) as usize + step, value);
    }
    let value = read(&state.temp, cursor_temp as usize);
    write(&mut state.work, dest as usize, value);
}
