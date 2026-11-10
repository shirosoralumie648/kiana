//! connection/relay.ts。
//!
//! TODO: 按 TS 源逐文件移植；本 crate 内不得出现第二份实现。
//!
//! # 现状
//!
//! 空模块：没有任何条目，只由 `crate::connection` 的 `pub mod relay;` 声明。它不导出可调用
//! 的 API，本 crate 内也没有代码引用它；`connection` 目录下尚未有连接实现。
//!
//! # 预期职责（尚未实现）
//!
//! `ARCHITECTURE.md` 把本文件登记为 TS `connection/relay.ts` 的对应物，但规格
//! `spec/chord-core.md` 文首“约束”下的源文件范围（未编号）/§2.4 把连接实现当作单文件
//! `connection.ts`。按规格：
//!
//! - §2.4.3 `RelayRef`：它保存 origin 连接与原始 hold 帧，`open(down)` 时在 down 连接上
//!   分配 `downId` 并转发 hold，同时维护 `originId`、`awaiting`（丢弃过 ops、等 reset 的
//!   成员）与 `calls`（中继调用表）。
//! - §2.4.4 `RelayCall`：一次经中继的调用，在 origin 与 down 两侧各有一个 id；`finish`
//!   只结算一次，并同时删除 `relay.calls[originId]` 与 `relay.down.relayedCalls[downId]`。
//! - §2.4.9 `#onHold`：host 收到 `frame.at !== host.name` 的 hold 时，除 upstream 角色
//!   直接回 `gone` 外，会登记一个 `RelayRef` 而不是本地 serve；本地 serve 与 relay 的分支
//!   就在一处判断；建立后由 `host.relay(relay)` 接管。
//!
//! `Served` 的联合类型也体现了两种形态：
//!
//! ```text
//! type Served = ServedLocal | { kind: "relay"; relay: RelayRef }
//! ```
//!
//! # 注意
//!
//! 两个编号分别属于两条 `Connection`，因此任何结束路径都必须两侧同时清理：§2.4.3 的
//! `close` 会删除 origin 的 served 项、down 的 `relayedRefs[downId]`，并按参数决定是否向
//! down 发 `release`；§2.4.4 的 `cancel` 在需要转发时向 down 发 `cancel`。若只清一侧，另一侧
//! 会留下无法结算的 hold/call。移植时不要在 `host` 或 `remote` 里另写一套中继逻辑，这与文件
//! 头的 TODO 是同一约束。
