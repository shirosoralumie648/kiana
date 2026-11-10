//! object/resource.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 职责
//! 资源是工厂通过 `self.resource(name, declaration)` 声明的外部句柄（TS 注释举的例子：
//! 数据库、watch、socket），带 `open`/`close` 回调。spec §8.1 把 `ResourceImpl`（真实状态
//! 与值）、`ResourceFacade`（工厂内可见的包装）以及 `open`/`close`/`adopt`/
//! `markUnavailable` 放在本文件；资源的打开/关闭由 `object_impl` 的激活与释放流程驱动。
//! `self.expose` 创建的是 `ExposedImpl`（§2.3.3）；引用读取成员时的解包
//! （`value instanceof ExposedImpl ? value.resource.value : value`）在 `RefImpl::member`
//! （§2.3.9）。
//!
//! # 状态机
//! `state` 有四个值（spec §2.3.3）：
//! - `closed`：未打开，或已正常关闭；
//! - `opening`：正在等待 `spec.open`；
//! - `open`：值可用，且已记录到对象的打开顺序表 `openOrder`；
//! - `unavailable`：reload 回滚时重新打开失败，值永久丢失。
//!
//! 转换：`closed → opening → open`；打开失败 `opening → closed`；打开成功但 signal 已 abort
//! 时 `opening → closed`（并立即关闭刚得到的值）；`open → closed`（close）；`open` 在非
//! `closed` 状态直接返回，因此重复调用是幂等的；`markUnavailable` 置 `unavailable` 并丢弃值。
//!
//! # 关键约束
//! - **abort 安全**：`spec.open` 完成后必须再检查 signal。若已 abort，这个值不能交给调用方：
//!   先把状态退回 `closed`，再 `spec.close(value)`，最后抛 `ChordError("aborted", ...)`。
//!   否则会出现无人拥有、也无人关闭的资源。
//! - **关闭顺序**：成功的 open 把资源 push 到对象的 `openOrder`；关闭按逆序进行
//!   （`unwind`/`dispose` 都如此），保证依赖者先于被依赖者关闭。
//! - **跨 reload 复用**：只有 `version`、`exclusive` 与依赖列表（长度、顺序、引用身份）都
//!   没变时，资源才被保留；保留的资源在 commit 时 `adopt` 新声明，使其后的 `close` 调用
//!   新代码的回调。否则本次 run 创建新的资源（spec §2.3.3、`Self.resource`）。
//! - **工厂期不可读**：`ResourceFacade::value` 在 `host.inFactory > 0` 时一律 `not_ready`，
//!   即使底层资源已 open；这样 reload 的 prepare 重跑不会碰到旧代码仍在使用的资源。
//! - **值不可用**：`unavailable` 读值报 `ChordError("unavailable", "resource <key> could not
//!   be restored")`；其他非 `open` 状态报 `not_ready`（TS 的 `notReady`）。
//!
//! # 依赖
//! 资源可以依赖引用（先等 `ready`）或其他资源（递归先打开）。依赖在 `Self.resource` 声明时
//! 解析，只能是本对象的资源或引用；打开顺序由递归保证（spec §2.3.14）。
//!
//! # 现状
//! 本文件目前只有这段说明，没有代码。
