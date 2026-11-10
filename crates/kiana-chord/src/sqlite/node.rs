//! `sqlite/node.ts`：基于 `rusqlite` 的 Node 驱动等价物（feature `sqlite-node`）。
//!
//! TS 用 `node:sqlite` 的 `DatabaseSync`；Rust 用 `rusqlite`（`bundled`）。所有可观察行为
//! 按 TS 逐条复刻：
//! - 一个串行操作队列：空闲时**同步**执行（返回已完成的 future），否则按调用顺序排队；
//! - 事务：`BEGIN IMMEDIATE` → 回调 → `COMMIT`；回调失败 → `ROLLBACK` 后原样重抛；
//!   `ROLLBACK` 也失败 → [`SqliteError::aggregate`]；
//! - 事务句柄在事务结算后失效；
//! - `close()` 幂等，先 `PRAGMA wal_checkpoint(TRUNCATE)` 再关闭连接；
//! - 打开时依次执行 `PRAGMA journal_mode = WAL`、`synchronous = NORMAL`、
//!   `wal_autocheckpoint = N`，并设置 `busy_timeout`。
//!
//! 调用方：`kiana-durable` 的 `storage::sqlite::node` 直接再导出
//! [`NodeSqliteDatabase`] / [`NodeSqliteDatabaseOptions`] / [`open_node_sqlite_database`]，
//! 它的 `open_node_sqlite_storage` 在调用打开器之前先校验 schema 前缀。
//!
//! # 执行模型：一个连接 + 一个串行队列
//! 同一个 [`NodeSqliteDatabase`]（包括它的所有 `clone`）共享 `Arc` 内的一个连接和一个
//! `SerialQueue`：
//! - 队列空闲时，操作在**调用方 poll 之前**就同步执行完，返回的 future 已完成；
//! - 队列忙（事务在跑或有操作排队）时，操作按调用顺序进入 FIFO，由前一个任务在 `leave`
//!   里急切执行，而不是等调用方逐个 poll——Rust 的 future 是惰性的，若等调用方依次
//!   await，乱序 await 会死锁；
//! - 事务是“异步任务”：登记时拿到一个 `TurnState`，轮到自己才 `BEGIN IMMEDIATE`，结算
//!   （COMMIT/ROLLBACK）后交还轮次并推进下一个任务。
//!
//! # 并发、锁与线程
//! - `Connection` 在 `Mutex<Option<Connection>>` 里；所有 SQL 都在持锁的同步代码中执行，
//!   锁不跨 `.await`（事务回调是在释放连接锁之后 await 的）。
//! - 队列锁只覆盖入队/出队本身；任务在锁外执行，因此任务内部再次访问队列不会自锁。
//!   队列锁与连接锁从不嵌套：`TurnGuard::drop` 也先释放连接锁，再取轮次锁。
//! - 不在后台 spawn 任务：空闲路径在当前 poll 线程执行，排队路径在交还轮次的那个任务
//!   里执行。因此一条长 SQL 会占用当前异步任务所在的执行线程。
//! - 事务句柄的 SQL 直接使用连接（不再过队列）；串行性由“事务持有轮次期间队列不派发其他
//!   任务”保证。回调里直接调用数据库本身会排在事务之后，从而永不结算（见
//!   [`SqliteDatabase::transaction`] 的契约）。
//!
//! # 取消与失败
//! - 事务 future 被丢弃：`TurnGuard::drop` 若发现 `BEGIN` 尚未结算就执行 `ROLLBACK`
//!   （回滚错误被忽略），并把轮次标记为已交还；排队中从未拿到轮次的 future 在 drop 时只
//!   标记 `Released`，队列之后会跳过这次通知。
//! - 事务回调 panic 会让 future 展开，`TurnGuard::drop` 走同一条回滚路径。
//! - 普通操作的 future 被丢弃不会取消已入队的闭包：闭包仍会执行，结果发送失败被忽略。
//! - 关闭后所有操作（含新事务）返回 `database is not open`；已经提交的写不受影响。
//!
//! # 与 `node:sqlite` 的兼容契约
//! 以下是当前稳定契约，改动需同步 `crates/kiana-chord/tests/sqlite_node.rs`：
//! SQL 文本按第一个 NUL 截断；`run`/`get`/`all` 只执行第一条语句：文本上能看出后续
//! 语句时只编译第一条，否则会先走 rusqlite 的多语句判定（可能编译尾部并暴露其错误，
//! 见 `prepare_first_cached` 的注释）；空语句报
//! `The SQL query contains no statements.`（`exec` 的空文本是 no-op）；多余位置参数报
//! `column index out of range`；超出 ±(2^53-1) 的整数返回
//! `Value is too large to be represented as a JavaScript number: <value>` 文案；rusqlite
//! 错误去掉语句/路径上下文；`mkdir` 失败用 Node 的 `CODE: message, syscall 'path'` 文本。
//! 逐项证据见下方辅助函数的注释与 `crates/kiana-chord/tests/sqlite_node.rs`。

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use indexmap::IndexMap;
use rusqlite::config::DbConfig;
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{CachedStatement, Connection, Statement, ToSql, ffi};
use tokio::sync::oneshot;

use crate::sqlite::database::{
    ErasedSqliteValue, SqliteBoxFuture, SqliteDatabase, SqliteError, SqliteExecutor, SqliteRow,
    SqliteValue, TransactionCallback,
};

/// SQLite 与适配器的 WAL 自动 checkpoint 默认页数（TS: `DEFAULT_WAL_AUTO_CHECKPOINT_PAGES`）。
const DEFAULT_WAL_AUTO_CHECKPOINT_PAGES: i64 = 1_000;
/// 适配器默认的忙等超时（TS: `DEFAULT_BUSY_TIMEOUT_MS`）。
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
/// 失效事务句柄的固定消息。
const INACTIVE_MESSAGE: &str = "SQLite transaction handle is no longer active";

/// Node SQLite 连接设置（TS: `NodeSqliteDatabaseOptions`）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodeSqliteDatabaseOptions {
    /// WAL 自动 checkpoint 阈值（页）；`None` 用 SQLite/适配器默认 1000，`0` 关闭。
    pub wal_auto_checkpoint_pages: Option<i64>,
    /// SQLite 等待竞争文件锁的毫秒数；`None` 用适配器默认 5000。
    pub busy_timeout_ms: Option<u64>,
}

/// 队列中的任务。同步任务由队列本身急切执行（与 TS 的调用时排队一致）；
/// 异步任务（事务）只是通知其 future 可以开始，并在它结算后归还轮次。
enum Job {
    Sync(Box<dyn FnOnce() + Send>),
    Async {
        signal: oneshot::Sender<()>,
        turn: Arc<Mutex<TurnState>>,
    },
}

/// 事务 future 对队首轮次的持有状态。
///
/// `Waiting`：轮次不在自己手里（队首是别的任务）；`Running`：轮次已交给自己；
/// `Released`：已经交还（或从未拿到就被丢弃）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnState {
    Waiting,
    Running,
    Released,
}

/// 串行执行队列：空闲时立即执行，否则按调用顺序急切执行。
///
/// TS 的 `run`/`runAsync` 在**调用时**入队；Rust 的 future 是惰性的，因此排队任务必须
/// 由队列驱动执行，而不是等调用方逐个 poll——否则调用方按任意顺序 await 会死锁。
#[derive(Default)]
struct SerialQueue {
    state: Mutex<QueueState>,
}

#[derive(Default)]
struct QueueState {
    /// 有一个任务（同步操作或事务）持有执行权；为 true 时新任务只能入队。
    busy: bool,
    /// 等待中的任务，按调用顺序排列（FIFO）。
    jobs: VecDeque<Job>,
}

impl SerialQueue {
    /// 空闲 → 占用并返回任务本身（调用方立即执行）；忙 → 入队并返回 `None`。
    fn admit(&self, job: Job) -> Option<Job> {
        let mut state = self.state.lock().expect("sqlite queue");
        if state.busy {
            state.jobs.push_back(job);
            None
        } else {
            state.busy = true;
            Some(job)
        }
    }

    /// 登记一个异步任务（事务）；返回轮次状态与需要等待的信号，空闲时信号为 `None`。
    ///
    /// 轮次状态在**登记时**就交给调用方保管：即使事务 future 在拿到轮次之前被丢弃，
    /// 守卫也能把状态标成 `Released`，队列随后跳过这次通知，不会永远卡在 `busy`。
    fn admit_async(&self) -> (Arc<Mutex<TurnState>>, Option<oneshot::Receiver<()>>) {
        let turn = Arc::new(Mutex::new(TurnState::Waiting));
        let mut state = self.state.lock().expect("sqlite queue");
        if state.busy {
            let (sender, receiver) = oneshot::channel();
            state.jobs.push_back(Job::Async {
                signal: sender,
                turn: Arc::clone(&turn),
            });
            (turn, Some(receiver))
        } else {
            state.busy = true;
            *turn.lock().expect("sqlite turn") = TurnState::Running;
            (turn, None)
        }
    }

    /// 交还执行权：急切执行排队的同步任务；遇到异步任务则通知它并返回。
    ///
    /// 出队只持有队列锁一小段；任务（`run()` / 发送通知）在锁外执行，因此任务内部再次
    /// 访问队列不会自锁。同步任务在这里直接跑完，它的 future 下一次 poll 就能拿到结果。
    fn leave(&self) {
        loop {
            let job = {
                let mut state = self.state.lock().expect("sqlite queue");
                match state.jobs.pop_front() {
                    Some(job) => job,
                    None => {
                        state.busy = false;
                        return;
                    }
                }
            };
            match job {
                Job::Sync(run) => run(),
                Job::Async { signal, turn } => {
                    let mut turn = turn.lock().expect("sqlite turn");
                    if *turn != TurnState::Waiting {
                        // 事务 future 在收到通知前就被丢弃：跳过它。
                        continue;
                    }
                    *turn = TurnState::Running;
                    if signal.send(()).is_ok() {
                        // 事务 future 会运行并在结算后再次 `leave`。
                        return;
                    }
                    // 事务 future 已丢弃：继续下一个任务。
                    *turn = TurnState::Released;
                }
            }
        }
    }
}

/// 事务的轮次守卫：future 被丢弃（取消）时回滚未结算的事务并交还轮次。
///
/// JS 的 promise 无法取消，被丢弃的 `transaction()` 会跑完并提交；Rust 的 future 可以
/// 被丢弃，回到「提交或回滚，且队列继续」的最接近语义——这里选择回滚。
///
/// 守卫与 future 一起被移动；即使调用方从未 poll 就被丢弃，`Drop` 也会把 `Waiting`
/// 轮次标记为 `Released`（队列在 `leave` 时跳过它），不会留下永久的 `busy`。
struct TurnGuard {
    inner: Arc<NodeSqliteInner>,
    turn: Arc<Mutex<TurnState>>,
    /// 事务句柄是否仍应可用。
    scope: Arc<AtomicBool>,
    /// `BEGIN IMMEDIATE` 已执行且尚未 COMMIT/ROLLBACK。
    open: std::cell::Cell<bool>,
}

impl TurnGuard {
    fn begin(&self) {
        self.open.set(true);
    }

    fn settle(&self) {
        self.open.set(false);
        self.scope.store(false, Ordering::SeqCst);
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if self.open.get() {
            // 取消：把连接从 BEGIN 里带出来，避免后续写入加入一个被遗弃的事务。
            // 这里是最后一道清理，回滚失败没有可报告的通道，忽略错误。
            if let Ok(guard) = self.inner.connection.lock()
                && let Some(connection) = guard.as_ref()
            {
                let _ = connection.execute_batch("ROLLBACK");
            }
        }
        // 句柄在事务结算后（或 future 被丢弃后）一律失效。
        self.scope.store(false, Ordering::SeqCst);
        let mut turn = self.turn.lock().expect("sqlite turn");
        match *turn {
            // Running：轮次在自己手里，交还并让队列继续。
            TurnState::Running => {
                *turn = TurnState::Released;
                drop(turn);
                self.inner.queue.leave();
            }
            // Waiting：还在排队；标记后 `leave` 出队时会跳过这次通知。
            TurnState::Waiting => *turn = TurnState::Released,
            TurnState::Released => {}
        }
    }
}

/// 数据库的共享状态（所有 `NodeSqliteDatabase` clone 指向同一份）。
///
/// - `queue`：所有公开操作与事务的串行化点；
/// - `connection`：`None` 表示已关闭（或事务开始前数据库已被关闭），否则所有 SQL 都经
///   `with_connection` 取锁执行；
/// - `closed`：只由 `close` 的 `swap` 使用，把重复关闭变成 no-op；连接是否可用以
///   `connection` 为准。
struct NodeSqliteInner {
    queue: Arc<SerialQueue>,
    connection: Mutex<Option<Connection>>,
    closed: AtomicBool,
}

impl NodeSqliteInner {
    /// 持连接锁执行一个同步操作；数据库已关闭时返回 `database is not open`。
    ///
    /// `operation` 不能是异步的：互斥锁在它返回后才释放，闭包里不允许 `.await`
    /// （本文件的调用点都只做同步的 rusqlite 调用）。
    fn with_connection<T>(
        &self,
        operation: impl FnOnce(&Connection) -> Result<T, SqliteError>,
    ) -> Result<T, SqliteError> {
        let guard = self.connection.lock().expect("sqlite connection");
        let Some(connection) = guard.as_ref() else {
            // node:sqlite：关闭后 exec/prepare/run/get/all 都报 `database is not open`。
            return Err(SqliteError::new("database is not open"));
        };
        operation(connection)
    }
}

/// node:sqlite 通过 `sqlite3_prepare_v3(..., -1, ...)` / `sqlite3_exec` 使用 **NUL 结尾**
/// 的 UTF-8 文本：JS 字符串里第一个 U+0000 之后的内容被忽略（`"\u0000"` 是空 SQL，
/// `"x\u0000y"` 只看到 `"x"`）。rusqlite 传的是显式长度，这里先按同样的规则截断。
/// NUL 字节 0x00 不可能出现在 UTF-8 多字节序列内部，所以按字节截断是安全的。
fn c_string_sql(sql: &str) -> &str {
    match sql.as_bytes().iter().position(|&byte| byte == 0) {
        Some(index) => &sql[..index],
        None => sql,
    }
}

/// `SqliteDatabase` 的 `node:sqlite` 等价物（TS: `NodeSqliteDatabase`）。
///
/// `Clone` 共享同一份 `NodeSqliteInner`（同一个连接与队列）：在任意 clone 上 `close`
/// 都会关闭共享连接，其他 clone 之后的操作同样得到 `database is not open`。类型本身是
/// `Send + Sync`，可以被多个任务/线程同时调用；串行化由内部队列负责。
#[derive(Clone)]
pub struct NodeSqliteDatabase {
    inner: Arc<NodeSqliteInner>,
}

impl std::fmt::Debug for NodeSqliteDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeSqliteDatabase").finish_non_exhaustive()
    }
}

impl NodeSqliteDatabase {
    /// 把操作登记进队列并返回其结果的 future。
    ///
    /// 空闲时同步执行（与 TS 的 `run` 相同），排队时由前一个操作在 `leave` 里急切执行。
    /// 执行发生在调用/交还轮次的线程上，不额外 spawn 任务；因此长 SQL 会占用当前异步
    /// 任务所在的执行线程。
    ///
    /// `operation` 的 panic 被 `catch_unwind` 转成 `"SQLite operation failed"`。返回的
    /// future 被丢弃只会让结果发送失败，**不会**取消已入队的闭包：它仍会执行完。
    fn run_owned<T, F>(&self, operation: F) -> SqliteBoxFuture<'static, Result<T, SqliteError>>
    where
        T: Send + 'static,
        F: FnOnce(&NodeSqliteInner) -> Result<T, SqliteError> + Send + 'static,
    {
        let inner = Arc::clone(&self.inner);
        let (sender, receiver) = oneshot::channel();
        let job = Job::Sync(Box::new({
            let inner = Arc::clone(&inner);
            move || {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| operation(&inner)))
                        .unwrap_or_else(|_| Err(SqliteError::new("SQLite operation failed")));
                let _ = sender.send(result);
            }
        }));
        if let Some(job) = inner.queue.admit(job) {
            if let Job::Sync(run) = job {
                run();
            }
            inner.queue.leave();
        }
        Box::pin(async move {
            match receiver.await {
                Ok(result) => result,
                Err(_) => Err(SqliteError::new("SQLite operation was abandoned")),
            }
        })
    }
}

impl SqliteExecutor for NodeSqliteDatabase {
    fn exec<'a>(&'a self, sql: &'a str) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        let sql = sql.to_owned();
        // exec 走 `execute_batch`：按顺序执行文本里的全部语句（NUL 截断后），空文本是
        // no-op——这与下面 run/get/all 只执行第一条语句不同：文本上能看出后续语句时只编译
        // 第一条，否则会先走 rusqlite 的多语句判定（可能编译尾部并暴露其错误，见
        // `prepare_first_cached` 的注释）。
        self.run_owned(move |inner| {
            inner.with_connection(|connection| {
                connection
                    .execute_batch(c_string_sql(&sql))
                    .map_err(SqliteError::from)
            })
        })
    }

    fn run<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        let sql = sql.to_owned();
        let params = params.to_vec();
        self.run_owned(move |inner| {
            inner.with_connection(|connection| {
                let mut statement = prepare_first_cached(connection, &sql)?;
                run_statement(&mut statement, &params)
            })
        })
    }

    fn get<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Option<SqliteRow>, SqliteError>> {
        let sql = sql.to_owned();
        let params = params.to_vec();
        self.run_owned(move |inner| {
            inner.with_connection(|connection| {
                query_rows(connection, &sql, &params, Some(1)).map(|mut rows| rows.pop())
            })
        })
    }

    fn all<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Vec<SqliteRow>, SqliteError>> {
        let sql = sql.to_owned();
        let params = params.to_vec();
        self.run_owned(move |inner| {
            inner.with_connection(|connection| query_rows(connection, &sql, &params, None))
        })
    }
}

impl SqliteDatabase for NodeSqliteDatabase {
    fn transaction<'a>(
        &'a self,
        callback: TransactionCallback<'a>,
    ) -> SqliteBoxFuture<'a, Result<ErasedSqliteValue, SqliteError>> {
        let inner = Arc::clone(&self.inner);
        // 登记时就拿到轮次状态：空闲则直接是 Running，忙则入队并在收到信号后变 Running。
        let (turn, queued) = inner.queue.admit_async();
        // 守卫在 future 之外创建：即使调用方从未 poll 就丢弃 future，轮次也会交还
        // （见 `TurnGuard` 与 `SerialQueue::leave`）。
        let scope = Arc::new(AtomicBool::new(true));
        let guard = TurnGuard {
            inner: Arc::clone(&inner),
            turn,
            scope: Arc::clone(&scope),
            open: std::cell::Cell::new(false),
        };
        let mut queued = queued;
        Box::pin(async move {
            let guard = guard;
            if let Some(receiver) = queued.take() {
                // 排队中：等前一个任务交还轮次。此刻 future 仍存活，队列不会把这次登记
                // 回收，所以正常路径一定会收到信号；`let _ =` 只是防御性写法。
                let _ = receiver.await;
            }
            {
                // BEGIN 在等文件锁时可能阻塞到 busy_timeout；此锁仅包住这一条同步语句，
                // 队列保证事务持有轮次期间没有别的任务会来抢连接。
                let connection_guard = inner.connection.lock().expect("sqlite connection");
                let Some(connection) = connection_guard.as_ref() else {
                    return Err(SqliteError::new("database is not open"));
                };
                connection
                    .execute_batch("BEGIN IMMEDIATE")
                    .map_err(SqliteError::from)?;
            }
            // 从这里到 settle 之间若 future 被丢弃或回调 panic，Drop 负责 ROLLBACK。
            guard.begin();
            let handle = NodeSqliteTransaction {
                inner: Arc::clone(&inner),
                scope: Arc::clone(&scope),
            };
            // 回调期间不持连接锁；句柄每次操作自行取锁。回调里直接调用数据库会排在当前
            // 事务之后（队列 busy），从而永远等不到结算。
            let result = callback(Box::new(handle)).await;
            let connection_guard = inner.connection.lock().expect("sqlite connection");
            let Some(connection) = connection_guard.as_ref() else {
                return Err(SqliteError::new("database is not open"));
            };
            // 结算规则：成功先 COMMIT，失败先 ROLLBACK。回滚成功时保留原来的
            // 错误（成功路径上是 COMMIT 错误，失败路径上是回调错误）；回滚也失败则报
            // 聚合错误，避免调用方误以为已回滚。
            let settled = match result {
                Ok(value) => match connection.execute_batch("COMMIT") {
                    Ok(()) => Ok(value),
                    Err(error) => match connection.execute_batch("ROLLBACK") {
                        Ok(()) => Err(SqliteError::from(error)),
                        Err(rollback_error) => Err(SqliteError::aggregate(vec![
                            SqliteError::from(error),
                            SqliteError::from(rollback_error),
                        ])),
                    },
                },
                Err(error) => match connection.execute_batch("ROLLBACK") {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(SqliteError::aggregate(vec![
                        error,
                        SqliteError::from(rollback_error),
                    ])),
                },
            };
            // 先标记句柄失效并清掉 open，再显式释放连接锁，最后 drop 守卫。`settle` 之后
            // `TurnGuard::drop` 已不需要回滚；保持“先解锁再 drop”的顺序，是为了避免将来
            // 改动 settle 时机时在 Drop 里重入不可重入的 `std::sync::Mutex`。drop 守卫会
            // 把轮次交还并推进队列。
            guard.settle();
            drop(connection_guard);
            drop(guard);
            settled
        })
    }

    fn close(&self) -> SqliteBoxFuture<'_, Result<(), SqliteError>> {
        self.run_owned(move |inner| {
            // 与 TS 的 `if (this.closed) return` 相同：第二次 close 是 no-op（返回 Ok）。
            // close 本身也走队列，因此不会和正在执行的事务/操作交错。
            if inner.closed.swap(true, Ordering::SeqCst) {
                return Ok(());
            }
            let mut guard = inner.connection.lock().expect("sqlite connection");
            let checkpoint = match guard.as_ref() {
                Some(connection) => connection
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                    .map_err(SqliteError::from),
                None => Ok(()),
            };
            // `try { … } finally { database.close() }`：checkpoint 的错误在关闭后照常抛出。
            // `take` 把连接移出：之后所有 `with_connection` 都会看到 `None`。注意即使
            // checkpoint 失败，`closed` 也已置位且连接已释放，再调用一次 close 会返回 Ok。
            guard.take();
            checkpoint
        })
    }
}

/// 事务句柄（TS: `NodeSqliteTransaction`；非导出）。结算后使用返回固定错误。
///
/// `scope` 与事务 future 的 `TurnGuard` 共享：`settle`（COMMIT/ROLLBACK 之后）或 future
/// 被丢弃时置为 false，句柄随即失效。句柄的 SQL 直接使用共享连接，不再经过队列。
struct NodeSqliteTransaction {
    inner: Arc<NodeSqliteInner>,
    scope: Arc<AtomicBool>,
}

impl NodeSqliteTransaction {
    /// 在事务句柄上执行一个同步操作；失效时返回固定的 `INACTIVE_MESSAGE`。
    ///
    /// 失效检查做两次：进入函数时一次，让已失效的句柄在调用点同步返回失败（不构造
    /// future、不取连接锁）；future 被 poll 时再一次，覆盖“调用之后、poll 之前事务恰好
    /// 结算”的窗口。
    fn run_operation<'a, T, F>(
        &'a self,
        operation: F,
    ) -> SqliteBoxFuture<'a, Result<T, SqliteError>>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> Result<T, SqliteError> + Send + 'a,
    {
        if !self.scope.load(Ordering::SeqCst) {
            return Box::pin(std::future::ready(Err(SqliteError::new(INACTIVE_MESSAGE))));
        }
        let inner = Arc::clone(&self.inner);
        let scope = Arc::clone(&self.scope);
        Box::pin(async move {
            if !scope.load(Ordering::SeqCst) {
                return Err(SqliteError::new(INACTIVE_MESSAGE));
            }
            inner.with_connection(operation)
        })
    }
}

impl SqliteExecutor for NodeSqliteTransaction {
    fn exec<'a>(&'a self, sql: &'a str) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.run_operation(move |connection| {
            connection
                .execute_batch(c_string_sql(sql))
                .map_err(SqliteError::from)
        })
    }

    fn run<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<(), SqliteError>> {
        self.run_operation(move |connection| {
            let mut statement = prepare_first_cached(connection, sql)?;
            run_statement(&mut statement, params)
        })
    }

    fn get<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Option<SqliteRow>, SqliteError>> {
        self.run_operation(move |connection| {
            query_rows(connection, sql, params, Some(1)).map(|mut rows| rows.pop())
        })
    }

    fn all<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [SqliteValue],
    ) -> SqliteBoxFuture<'a, Result<Vec<SqliteRow>, SqliteError>> {
        self.run_operation(move |connection| query_rows(connection, sql, params, None))
    }
}

/// `SqliteValue` 的绑定适配器。
///
/// 文本与 BLOB 直接借用原切片（`ToSqlOutput::Borrowed`）；空值/整数/浮点构造 owned 的
/// rusqlite 值。
struct BoundValue<'a>(&'a SqliteValue);

impl ToSql for BoundValue<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self.0 {
            SqliteValue::Null => ToSqlOutput::Owned(rusqlite::types::Value::Null),
            SqliteValue::Integer(value) => {
                ToSqlOutput::Owned(rusqlite::types::Value::Integer(*value))
            }
            SqliteValue::Real(value) => ToSqlOutput::Owned(rusqlite::types::Value::Real(*value)),
            SqliteValue::Text(value) => ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes())),
            SqliteValue::Blob(value) => ToSqlOutput::Borrowed(ValueRef::Blob(value.as_slice())),
        })
    }
}

/// 把 rusqlite 错误收敛成只有文本的 [`SqliteError`]（丢弃错误码与扩展信息）。
impl From<rusqlite::Error> for SqliteError {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            // rusqlite 把 `sqlite3_error_offset()` 的上下文拼进 `Display`
            // （`near "SELEC": syntax error in SELEC 1 at offset 0`）；node:sqlite 只暴露
            // `sqlite3_errmsg` 的文本（`near "SELEC": syntax error`）。
            rusqlite::Error::SqlInputError { msg, .. } => SqliteError::new(msg),
            other => SqliteError::new(other.to_string()),
        }
    }
}

/// 准备 `sql` 的第一条语句（TS: `DatabaseSync.prepare`）。
///
/// node:sqlite 只编译字符串里的第一条语句并忽略其余部分。rusqlite 的
/// `prepare`/`prepare_cached` 会以 `MultipleStatement` 拒绝多语句输入（而且它在判定
/// 时会**编译**尾部语句，尾部有语法错误时抛的是那个错误）。因此只要能看出还有后续
/// 语句，就先求出第一条语句的字节长度，再让 rusqlite 编译该前缀。
///
/// 使用 `prepare_cached`：命中连接的预编译语句缓存（按 SQL 文本做键），数据库与事务
/// 句柄共享同一个连接，因此也共享缓存，对应 TS 的 `Map<string, StatementSync>`。
fn prepare_first_cached<'c>(
    connection: &'c Connection,
    sql: &str,
) -> Result<CachedStatement<'c>, SqliteError> {
    // node:sqlite 传给 SQLite 的是 NUL 结尾的 C 字符串：第一个 U+0000 之后不可见。
    let sql = c_string_sql(sql);
    if !contains_statement(sql) {
        // node:sqlite 在 `prepare` 里对空串/纯注释/纯分号抛 TypeError。
        return Err(SqliteError::new("The SQL query contains no statements."));
    }
    if has_trailing_statement(sql) {
        // 文本上能看出后续语句：直接只把第一条的前缀交给 rusqlite，避免它去编译尾部。
        let prefix = first_statement_prefix(connection, sql)?;
        return connection.prepare_cached(prefix).map_err(SqliteError::from);
    }
    match connection.prepare_cached(sql) {
        Ok(statement) => Ok(statement),
        Err(rusqlite::Error::MultipleStatement) => {
            // 文本判断与实际解析不一致时的防御性兜底：让 SQLite 自己给出第一条语句的边界。
            let prefix = first_statement_prefix(connection, sql)?;
            connection.prepare_cached(prefix).map_err(SqliteError::from)
        }
        Err(error) => Err(SqliteError::from(error)),
    }
}

/// SQLite 认为文本里至少有一条语句：跳过空白、`--` 行注释、`/* … */` 块注释与
/// 空语句分隔符 `;` 后还有内容。
///
/// 只服务于“空 SQL 报错”这一个判定，不是完整的 SQL 词法器：它不解析字符串字面量，
/// 看到非注释字符就认为有语句，然后交给 SQLite 编译。
fn contains_statement(sql: &str) -> bool {
    let bytes = sql.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() || byte == b';' => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                // SQLite 词法器：`if( z[1]!='*' || z[2]==0 )` 返回 TK_SLASH。也就是
                // **恰好**以 "/*" 结束的输入是 `/` 记号，prepare 报 `near "/": syntax
                // error`，而不是"没有语句"（`"/* "`、`"/*x"` 仍是注释）。
                if index + 2 == bytes.len() {
                    return true;
                }
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            _ => return true,
        }
    }
    false
}

/// `sql` 的最后一个分号后面还有非空白内容（后续语句）。
///
/// 字符串字面量/注释里的分号会误判为 true，代价只是多一次 prepare（结果不变）。
fn has_trailing_statement(sql: &str) -> bool {
    match sql.rfind(';') {
        Some(index) => sql[index + 1..]
            .chars()
            .any(|character| !character.is_whitespace()),
        None => false,
    }
}

/// `sqlite3_prepare_v2` 实际编译掉的 `sql` 前缀（第一条语句，含结尾分号）。
///
/// 做法是先用一次原始 `sqlite3_prepare_v2` 只为拿 `tail` 指针：`tail` 指向第一条语句之后
/// 的第一个字节，用它减去缓冲区起点得到前缀长度；随后立即 `finalize` 这条语句，再由
/// rusqlite 重新 prepare 该前缀（未命中语句缓存时会再编译一次；命中缓存则直接复用）。
/// 错误路径只返回 `sqlite3_errmsg` 文本，与 node:sqlite 一致。
fn first_statement_prefix<'a>(
    connection: &Connection,
    sql: &'a str,
) -> Result<&'a str, SqliteError> {
    let bytes = sql.as_bytes();
    let length =
        c_int::try_from(bytes.len()).map_err(|_| SqliteError::new("string or blob too big"))?;
    let mut statement: *mut ffi::sqlite3_stmt = std::ptr::null_mut();
    let mut tail: *const c_char = std::ptr::null();
    // SAFETY: `handle()` 在连接存活期间有效；缓冲区是 `sql` 的字节；`length` 与之匹配。
    let code = unsafe {
        ffi::sqlite3_prepare_v2(
            connection.handle(),
            bytes.as_ptr().cast::<c_char>(),
            length,
            &mut statement,
            &mut tail,
        )
    };
    if code != ffi::SQLITE_OK {
        // 第一条语句本身编译失败：照 node 的方式只取 `sqlite3_errmsg`（不带
        // rusqlite 的 "... in <sql> at offset N" 上下文）。
        let message = unsafe {
            let pointer = ffi::sqlite3_errmsg(connection.handle());
            if pointer.is_null() {
                "unknown error".to_owned()
            } else {
                CStr::from_ptr(pointer).to_string_lossy().into_owned()
            }
        };
        if !statement.is_null() {
            // SAFETY: prepare 返回的错误路径不会交出可用语句，但防御性地释放。
            unsafe { ffi::sqlite3_finalize(statement) };
        }
        return Err(SqliteError::new(message));
    }
    let offset = if tail.is_null() {
        bytes.len()
    } else {
        (tail as usize).saturating_sub(bytes.as_ptr() as usize)
    };
    // SAFETY: 语句由上面的 prepare 产出，且随后不再被使用。
    unsafe { ffi::sqlite3_finalize(statement) };
    Ok(&sql[..offset.min(bytes.len())])
}

/// 按 node:sqlite 的语义绑定位置参数。
///
/// node 把位置参数按顺序绑定到语句的**匿名参数**（无名字段或 `?NNN`）；`:name`/
/// `@name`/`$name` 这类命名参数只能用对象绑定。多余的位置参数报
/// `column index out of range`；缺少的参数保持 SQLite 的默认值 NULL。
///
/// 目标索引按 `1..=parameter_count()` 升序收集，所以位置参数按 SQL 中匿名参数出现的
/// 索引顺序对应；`?NNN` 的编号也参与这个顺序（见测试
/// `binds_positional_parameters_to_anonymous_parameters_only`）。`raw_bind_parameter`
/// 跳过 rusqlite 按参数名做的那层检查，参数名判断由上面的 filter 完成。
fn bind_parameters(
    statement: &mut Statement<'_>,
    params: &[SqliteValue],
) -> Result<(), SqliteError> {
    let targets: Vec<usize> = (1..=statement.parameter_count())
        .filter(|&index| {
            statement
                .parameter_name(index)
                .is_none_or(|name| name.starts_with('?'))
        })
        .collect();
    if params.len() > targets.len() {
        return Err(SqliteError::new("column index out of range"));
    }
    for (value, target) in params.iter().zip(targets) {
        statement
            .raw_bind_parameter(target, BoundValue(value))
            .map_err(SqliteError::from)?;
    }
    Ok(())
}

/// TS `StatementSync.run`：执行语句并丢弃结果行（`RETURNING`、`PRAGMA` 也照常执行）。
fn run_statement(statement: &mut Statement<'_>, params: &[SqliteValue]) -> Result<(), SqliteError> {
    bind_parameters(statement, params)?;
    let mut rows = statement.raw_query();
    while rows.next().map_err(SqliteError::from)?.is_some() {}
    Ok(())
}

/// 准备第一条语句、绑定参数并读取行；`limit = Some(n)` 在读到 n 行后停止。
///
/// `get` 传 `Some(1)`，所以它只 step 一行；`all` 传 `None`（读到底）。行转换规则见
/// `read_row`。
fn query_rows(
    connection: &Connection,
    sql: &str,
    params: &[SqliteValue],
    limit: Option<usize>,
) -> Result<Vec<SqliteRow>, SqliteError> {
    let mut statement = prepare_first_cached(connection, sql)?;
    bind_parameters(&mut statement, params)?;
    let mut rows = statement.raw_query();
    let mut result = Vec::new();
    while let Some(row) = rows.next().map_err(SqliteError::from)? {
        result.push(read_row(row)?);
        if limit.is_some_and(|limit| result.len() >= limit) {
            break;
        }
    }
    Ok(result)
}

/// node:sqlite 未开启 `setReadBigInts`：超出 ±(2^53-1) 的 INTEGER 列报 `RangeError`。
const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// 把一行按列序读成 [`SqliteRow`]。
///
/// 列名取自语句的 `column_name`（表达式列得到 SQLite 生成的列名）。同一结果集里列名
/// 重复时（例如连接两张都有 `id` 的表），`IndexMap` 只保留一个键：后出现者的值覆盖
/// 先前的值，位置保持首次出现的位置。
///
/// 整数列超出 JavaScript 安全整数范围时返回错误而不是截断；TEXT 用
/// `String::from_utf8_lossy` 转换，非法 UTF-8 字节序列会变成替换字符。
fn read_row(row: &rusqlite::Row<'_>) -> Result<SqliteRow, SqliteError> {
    let statement = row.as_ref();
    let mut values = IndexMap::new();
    for index in 0..statement.column_count() {
        let name = statement
            .column_name(index)
            .map_err(SqliteError::from)?
            .to_owned();
        let value = match row.get_ref(index).map_err(SqliteError::from)? {
            ValueRef::Null => SqliteValue::Null,
            ValueRef::Integer(value) => {
                if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
                    return Err(SqliteError::new(format!(
                        "Value is too large to be represented as a JavaScript number: {value}"
                    )));
                }
                SqliteValue::Integer(value)
            }
            ValueRef::Real(value) => SqliteValue::Real(value),
            ValueRef::Text(bytes) => SqliteValue::Text(String::from_utf8_lossy(bytes).into_owned()),
            ValueRef::Blob(bytes) => SqliteValue::Blob(bytes.to_vec()),
        };
        values.insert(name, value);
    }
    Ok(SqliteRow(values))
}

/// 打开并配置一个 Node 等价 SQLite 数据库门面（TS: `openNodeSqliteDatabase`）。
///
/// 与 TS 一样是异步入口；文件路径不是 `:memory:` 时先递归创建父目录。
///
/// 实际工作由同步的 `open_inner` 完成，future 在返回时已经就绪：目录创建、打开连接与
/// 三条 PRAGMA 都在调用线程上同步执行。`options` 的两个字段只在打开时使用：
/// `wal_auto_checkpoint_pages` 缺省 1000、`0` 关闭自动 checkpoint；
/// `busy_timeout_ms` 缺省 5000。
pub fn open_node_sqlite_database(
    path: &str,
    options: NodeSqliteDatabaseOptions,
) -> SqliteBoxFuture<'static, Result<NodeSqliteDatabase, SqliteError>> {
    let result = open_inner(path, &options);
    Box::pin(std::future::ready(result))
}

/// Node `fs` 错误的文本：`"<CODE>: <libuv message>, <syscall> '<path>'"`。
///
/// TS 的 `openNodeSqliteDatabase` 在 `new DatabaseSync` 之前 `mkdir`，失败时把这个
/// 错误原样抛出（例如 `"EEXIST: file already exists, mkdir '/dev/null'"`）。
/// `std::io::Error` 的 Display（`"File exists (os error 17)"`）与之不同。
///
/// 只有 `node_errno` 认识的 errno 才会得到 Node 文本；其它情况（以及非 unix 平台）
/// 回退到 `std::io::Error` 的 Display，此时文案与 Node 不同。
fn node_fs_error(error: &std::io::Error, syscall: &str, path: &str) -> SqliteError {
    match node_errno(error) {
        Some((code, description)) => {
            SqliteError::new(format!("{code}: {description}, {syscall} '{path}'"))
        }
        None => SqliteError::new(error.to_string()),
    }
}

/// libuv 的 errno → `(code, description)` 文本（Node 的 fs 错误头）。
///
/// 只覆盖下面列出的 errno；没有 `raw_os_error()` 或不在表里的错误返回 `None`。
#[cfg(unix)]
fn node_errno(error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    let code = error.raw_os_error()?;
    let pair = match code {
        libc::EACCES => ("EACCES", "permission denied"),
        libc::EEXIST => ("EEXIST", "file already exists"),
        libc::ENOENT => ("ENOENT", "no such file or directory"),
        libc::ENOTDIR => ("ENOTDIR", "not a directory"),
        libc::EISDIR => ("EISDIR", "illegal operation on a directory"),
        libc::EROFS => ("EROFS", "read-only file system"),
        libc::ENOSPC => ("ENOSPC", "no space left on device"),
        libc::EFBIG => ("EFBIG", "file too large"),
        libc::ENOTEMPTY => ("ENOTEMPTY", "directory not empty"),
        libc::ELOOP => ("ELOOP", "too many symbolic links encountered"),
        libc::ENAMETOOLONG => ("ENAMETOOLONG", "name too long"),
        libc::EMFILE => ("EMFILE", "too many open files"),
        libc::ENFILE => ("ENFILE", "file table overflow"),
        libc::ENOMEM => ("ENOMEM", "out of memory"),
        libc::EIO => ("EIO", "i/o error"),
        libc::EINVAL => ("EINVAL", "invalid argument"),
        libc::EPERM => ("EPERM", "operation not permitted"),
        libc::ENODEV => ("ENODEV", "no such device"),
        libc::ENXIO => ("ENXIO", "no such device or address"),
        libc::EBUSY => ("EBUSY", "resource busy or locked"),
        libc::EXDEV => ("EXDEV", "cross-device link not permitted"),
        _ => return None,
    };
    Some(pair)
}

#[cfg(not(unix))]
fn node_errno(_error: &std::io::Error) -> Option<(&'static str, &'static str)> {
    None
}

/// `Connection::open` 的错误：rusqlite 在 `CannotOpen` 时把 `": <path>"` 拼在
/// `sqlite3_errmsg` 后面（`unable to open database file: /sys/x.db`）；node:sqlite
/// 只暴露 `sqlite3_errmsg` 本身（`unable to open database file`）。
///
/// 只在消息**恰好**以 `": <path>"` 结尾时剥离；其它 rusqlite 错误走
/// `From<rusqlite::Error>` 的通用转换。
fn open_error(error: rusqlite::Error, path: &str) -> SqliteError {
    if let rusqlite::Error::SqliteFailure(_, Some(message)) = &error
        && let Some(stripped) = message.strip_suffix(&format!(": {path}"))
    {
        return SqliteError::new(stripped.to_owned());
    }
    SqliteError::from(error)
}

/// 同步的打开流程：建父目录 → 打开连接 → 关闭 DQS → 设 busy_timeout → 三条 PRAGMA。
fn open_inner(
    path: &str,
    options: &NodeSqliteDatabaseOptions,
) -> Result<NodeSqliteDatabase, SqliteError> {
    let checkpoint_pages = options
        .wal_auto_checkpoint_pages
        .unwrap_or(DEFAULT_WAL_AUTO_CHECKPOINT_PAGES);
    let timeout = options.busy_timeout_ms.unwrap_or(DEFAULT_BUSY_TIMEOUT_MS);
    // `:memory:` 没有父目录；`foo.db` 的 parent 是空路径，也不应该拿它去 mkdir。
    if path != ":memory:"
        && let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| node_fs_error(&error, "mkdir", &parent.to_string_lossy()))?;
    }
    let connection = Connection::open(path).map_err(|error| open_error(error, path))?;
    // node:sqlite 的 `DatabaseSync` 默认 `enableDoubleQuotedStringLiterals = false`：
    // 双引号串在 DML 与 DDL 里都按标识符解析，解析不到就报
    // `no such column: "…" - should this be a string literal in single-quotes?`。
    // rusqlite 的 bundled SQLite 保留 legacy DQS，必须显式关掉。
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)
        .map_err(SqliteError::from)?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)
        .map_err(SqliteError::from)?;
    connection
        .busy_timeout(Duration::from_millis(timeout))
        .map_err(SqliteError::from)?;
    let database = NodeSqliteDatabase {
        inner: Arc::new(NodeSqliteInner {
            queue: Arc::new(SerialQueue::default()),
            connection: Mutex::new(Some(connection)),
            closed: AtomicBool::new(false),
        }),
    };
    let setup = database.inner.with_connection(|connection| {
        connection
            .execute_batch("PRAGMA journal_mode = WAL")
            .map_err(SqliteError::from)?;
        connection
            .execute_batch("PRAGMA synchronous = NORMAL")
            .map_err(SqliteError::from)?;
        // 插值，不是绑定参数（PRAGMA 不接受绑定）。
        connection
            .execute_batch(&format!("PRAGMA wal_autocheckpoint = {checkpoint_pages}"))
            .map_err(SqliteError::from)?;
        Ok(())
    });
    match setup {
        Ok(()) => Ok(database),
        Err(error) => {
            // 保留配置失败：关闭错误被吞掉。先做一次 TRUNCATE checkpoint 再丢弃连接，
            // 对应 TS 的 `catch` 里 `await adapter.close()`；这里的错误同样不覆盖原错误。
            if let Ok(mut guard) = database.inner.connection.lock()
                && let Some(connection) = guard.take()
            {
                let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            }
            Err(error)
        }
    }
}
