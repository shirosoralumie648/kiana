//! object/object.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 职责
//! `ObjectImpl` 是「一个地址上的对象」的运行时状态机与所有者：它持有引用、资源、声明集合
//! （`Decls`）、按 code version 安装的实现（`Installed`）、租约（root 对对象的占用）与在飞
//! 调用，并把对象从 `activating` 推进到 `live`，最后在无人使用时释放。spec §8.1 把
//! `start`/`run`/`open`/`initCells`/`unwind`/`install` 等状态机方法放在本文件；监听器/任务
//! 的运行期启停在 `declarations`，调用准入与释放入口在 `invoke`。
//! TS 对照：`spec/chord-core.md` §2.3.11-§2.3.19、§2.3.25-§2.3.26（源文件 `object.ts`）。
//!
//! # 状态机
//! `ObjectStatus` 五个状态（spec §2.3.11）：
//! - `activating`：构造后的初始态；`ready` future 正在跑激活事务，引用为 `loading`。
//! - `live`：激活成功，运行期副作用已启动；引用转 `live`，调用被接纳。
//! - `failed`：激活失败（仅当失败时仍是 `activating`）；`host.forget` 已调用，所有引用
//!   `gone`，错误由 `ready` 抛出。
//! - `disposing`：释放中；强制释放会立即把引用置 `gone` 并 abort 在飞调用，普通释放等待
//!   在飞调用结束。
//! - `disposed`：终态；资源已关闭、引用已释放，宿主收到 `disposed` 通知。
//!
//! 转换：`activating → live` 或 `activating → failed`（激活事务的成败）；
//! `activating`/`live`/`failed → disposing → disposed`（`dispose` 幂等，`#disposal` 缓存
//! 同一个 future，后续调用返回它）。
//!
//! # 激活事务（`#activate`）
//! 顺序不能随意调整（spec §2.3.12）：
//! 1. 等待同一地址前一次释放完成（`after`）。
//! 2. 查服务；未注册 → `unavailable/unknown_type`。
//! 3. 同步执行工厂（`run`），得到实现与 `Decls`。
//! 4. 打开资源（依赖资源先开）并等待引用 ready；所有打开动作都 settle 后才抛第一个错误。
//! 5. 等待状态源 hydrate（拿到初始值，含转发自远程引用的成员与派生值），受 `readyTimeout`
//!    限制。
//! 6. 并行执行各 cell 的 `init`（各自一个 root，以宿主 principal），abort 后不再写入。
//! 7. 校验注册表，把本次实现安装到 service 相同的所有保留 code version；若当前/pending
//!    都没有本次实现 → `superseded`（激活期间代码变了）。
//! 8. attach 派生、刷新 schema；若不是当前 code version 的新对象，则延迟运行期副作用到
//!    reload 的 commit（提交点）；否则现在启动监听器与任务。
//! 9. 再次检查 abort（监听器启动可能已同步释放最后一个引用，不能复活），置 `live`。
//!
//! 任何一步失败：abort `callAbort`、停监听器/任务、若已发布注册则撤回、清空实现与声明、
//! 等在飞调用结束、`unwind` 本次 run 创建的资源与引用，然后把错误交给 `start` 的失败分支。
//!
//! # 工厂运行（`run`）
//! - 工厂必须同步返回对象；返回 `Promise` 视为 `invalid/implementation`。
//! - 运行前把本次 `Decls` 加入 `pending`，让循环引用检查能看到尚未安装的声明。
//! - 运行后校验实现与契约匹配：remote 契约必须给出已声明的 variant 且成员不越界；method
//!   必须是函数，state 必须是 cell/派生/readable-like，opaque 不能缺失（spec §2.3.13、
//!   §2.3.26）。校验失败会 discard 并 unwind。
//!
//! # 释放与清理
//! `dispose` 的调用语义见 `invoke`；从状态机角度它只做一次：进入 `disposing`，等所有
//! invocation 结束，按 `openOrder` 逆序关闭资源、释放所有声明集的引用，最后置 `disposed`
//! 并通知宿主。`unwind(run)` 只清理一次 factory run 创建的派生、资源与引用，清理步骤用
//! `runAll` 语义全部执行后再统一报错。
//!
//! # 现状
//! 本文件目前只有这段说明，没有代码；Rust 侧的锁/并发模型尚未确定，实现前不要假设
//! `ObjectImpl` 可以跨线程共享。
