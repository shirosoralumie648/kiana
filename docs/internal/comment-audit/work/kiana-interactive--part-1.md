# 注释清理工作单：kiana-interactive--part-1

- 待处理条目：40
- 涉及文件：8

## 规则（必须遵守）

1. **只改注释/文档文字，不改代码、字符串字面量、测试断言、类型签名或行为。**
2. 目标是把“因为 TS 这样，所以必须这样”改成“当前设计的技术原因”，或者删除纯 TS 理由。
3. 如果注释描述的是用户可见契约（错误文案、字段顺序、落盘格式、ANSI 输出），保留契约，但不要再用 TS 作为权威；改成“当前稳定契约，改动需同步测试”。
4. 如果注释只是出处（`TS 源：...`），保留出处，把约束性措辞去掉；不要删除整个出处。
5. D 类没有明确提到 TS：先判断是否仍有独立技术原因；有就保留并补技术原因，没有就删除。
6. 不要重写无关注释，不要扩大 diff；一个文件尽量只动列出的行附近。
7. 不要运行 `cargo fmt` / `cargo check` / 测试；统一由 verifier 执行。
8. 不要修改本工作单或 `docs/internal/comment-audit/scan*.csv`。

## 文件清单

- `crates/kiana-interactive/src/assets/mod.rs`
- `crates/kiana-interactive/src/bug_report.rs`
- `crates/kiana-interactive/src/components/config_selector.rs`
- `crates/kiana-interactive/src/components/easter_egg_3d.rs`
- `crates/kiana-interactive/src/components/model_selector.rs`
- `crates/kiana-interactive/src/components/session_selector.rs`
- `crates/kiana-interactive/src/components/settings_submenu.rs`
- `crates/kiana-interactive/src/components/tree_selector.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:22` | A | TS等价物 | //! `play_easter_egg_3d_now`，立即建立 overlay（原样显示抓屏，等价于 TS 尚未创建 overlay | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:377` | A | 逐字一致, TS的 | // 数学工具（与原 TS 的运算顺序逐字一致） | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:1068` | A | 与TS一致, 完全一致 | // * BMP 字符（é、你）与 TS 完全一致； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:1287` | A | 与TS一致 | /// （光栅化的射线方向），也用于取相机空间法线（矩阵的列）。乘法顺序与 TS 一致，改顺序会 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:2097` | A | TS等价物, TS的 | /// Rust 里 overlay 条目是唯一的所有者，销毁即等价于 TS 的 finish，但此时可能正持有 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:3245` | A | 与TS一致 | /// 与 TS 一致：只清尺寸标记与 `ansi_cache`，不直接清 `cells`/`stars`（它们由尺寸变化 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:3509` | A | 与TS一致 | // 只有这一处 await（与 TS 一致：`queryTerminalColors({ timeoutMs: 100 })`）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:3703` | A | TS等价物, TS的 | // 没有事件循环可以消费回复：等价于 TS 的 100 ms 超时（用兜底色构造）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:409` | D | 逐位一致 | // Math.sin / Math.cos（V8 的 fdlibm 端口，逐位一致） | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 10 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:415` | D | 逐位一致 | /// 1 ulp（旋转矩阵/姿态因此不是逐位一致）。这里逐行移植 V8 `src/base/ieee754.cc` | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 11 | `crates/kiana-interactive/src/components/easter_egg_3d.rs:2894` | D | 完全一致 | /// 完全一致的规则： | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 12 | `crates/kiana-interactive/src/components/session_selector.rs:15` | A | TS等价物, TS的 | //! core 取走），行为上等价于 TS 的 `isActive()` 身份比较；槽位的 generation 是创建 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-interactive/src/components/session_selector.rs:714` | A | 与TS一致 | // 两行提示的优先级与 TS 一致：删除确认态 > 临时状态消息 > 常规快捷键。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-interactive/src/components/session_selector.rs:1551` | A | 与TS一致 | // 与 TS 一致：头部更新由组件安装的回调完成（测试可替换该回调）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-interactive/src/components/session_selector.rs:1692` | A | 与TS一致 | // 颜色优先级与 TS 一致：删除确认 > 当前会话 > 有名称；选中再加粗。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 16 | `crates/kiana-interactive/src/components/session_selector.rs:1978` | A | 与TS一致 | /// 构造末尾立即开始加载 current scope（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-interactive/src/components/session_selector.rs:901` | D | 强约束 | /// 后台预加载完成不能改变用户正在看的界面。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 18 | `crates/kiana-interactive/src/components/session_selector.rs:1231` | D | 逐字一致 | /// 表项文案逐字对应 libuv；只按 `raw_os_error()` 的数值匹配，未列出的 errno 返回 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 19 | `crates/kiana-interactive/src/components/tree_selector.rs:16` | A | 与TS一致 | //! * `TreeSelectorComponent` 的 `focused` setter 与 TS 一致：**只**下发给当前 `LabelInput`， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-interactive/src/components/tree_selector.rs:928` | A | 与TS一致 | /// 已折叠节点的条目 id 集合；任何过滤模式或搜索词变化都会清空（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-interactive/src/components/tree_selector.rs:1113` | A | 逐字一致 | /// 缩进/连接符规则（与 TS 逐字对应）：多根时根挂在虚拟根下（indent 1、自身不画 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-interactive/src/components/tree_selector.rs:2747` | A | 逐字一致, TS的, 对应TS | /// 帮助行的条目与顺序逐字对应 TS 的 `TREE_HELP_ITEMS`；改顺序或标签会改变 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-interactive/src/components/tree_selector.rs:2838` | D | 完全一致 | /// 两个及以上按键且修饰键前缀完全一致时，只写一次前缀、后缀用 `/` 连接 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 24 | `crates/kiana-interactive/src/assets/mod.rs:50` | A | 逐字一致, TS源 | /// 集成测试 `constants_are_verbatim` 锁定该字面量与 TS 源逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-interactive/src/assets/mod.rs:19` | D | 逐字节一致 | //! `Buffer.toString("base64")` 逐字节一致；这里不引 `base64` 依赖（只有这一处需要）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-interactive/src/assets/mod.rs:40` | D | 保真 | //! * 字节保真由测试锁定（长度、base64 长度与 PNG magic 前缀）；换资源前必须逐项核对， | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-interactive/src/assets/mod.rs:90` | D | 逐字节一致 | /// 它与 Node 的 `Buffer.toString("base64")` 输出逐字节一致，并由 RFC 4648 向量测试覆盖 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 28 | `crates/kiana-interactive/src/components/config_selector.rs:515` | A | 与TS一致 | /// 分支顺序与 TS 一致，后面的分支只处理前面未命中的来源： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 29 | `crates/kiana-interactive/src/components/config_selector.rs:2221` | A | 与TS一致 | // 分派顺序与 TS 一致：方向键 → 翻页 → 取消 → 退出 → Tab → 空格/确认 → | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-interactive/src/components/config_selector.rs:14` | D | 逐字一致 | //! crate 私有）。POSIX 输出与 Node 逐字一致；Windows 分支只做 `cfg!(windows)` 的词法近似 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 31 | `crates/kiana-interactive/src/components/config_selector.rs:305` | D | 逐字一致 | /// 只保证 POSIX 逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 32 | `crates/kiana-interactive/src/components/model_selector.rs:55` | A | 与TS一致 | //! - `dispose` 只置 `closed` 并取消刷新令牌，不清理回调；与 TS 一致，之后到来的按键仍会 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-interactive/src/components/model_selector.rs:631` | A | 逐字一致 | // 行格式与 TS 逐字一致：光标 + 当前标记 + id + (distinct name) + [provider] + ·default。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-interactive/src/components/model_selector.rs:732` | A | 逐字一致 | /// 分支顺序与 TS 逐字一致：Tab → up → down → confirm → cancel → save → 其它。每个分支 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-interactive/src/components/model_selector.rs:1147` | A | 与TS一致 | /// （与 TS 一致），之后到达的刷新结果会被丢弃。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-interactive/src/components/settings_submenu.rs:202` | A | TS等价物, TS的 | /// `submenu_options` 缺省时等价于 TS 的 `submenuOptions?: undefined`：布局取 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 37 | `crates/kiana-interactive/src/components/settings_submenu.rs:707` | A | 与TS一致 | /// 步骤数大于 1 时把 `Step {n}/{total} · ` 前缀拼进 description（与 TS 一致，标题行不受 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-interactive/src/components/settings_submenu.rs:777` | A | 与TS一致 | /// `on_cancel`（与 TS 一致，完成后面板走关闭路径）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 39 | `crates/kiana-interactive/src/components/settings_submenu.rs:1032` | A | 与TS一致 | // Esc 只删除**当前步**的键：第 0 步已经写入的 "a" 保留（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-interactive/src/bug_report.rs:337` | D | 逐字一致 | /// Rust 用 `SessionManagerError::Io` 承载这条 rejection（`Display` 与错误消息逐字一致）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
