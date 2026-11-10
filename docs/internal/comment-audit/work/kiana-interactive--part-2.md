# 注释清理工作单：kiana-interactive--part-2

- 待处理条目：40
- 涉及文件：17

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

- `crates/kiana-interactive/src/components/armin.rs`
- `crates/kiana-interactive/src/components/assistant_message.rs`
- `crates/kiana-interactive/src/components/custom_editor.rs`
- `crates/kiana-interactive/src/components/extension_selector.rs`
- `crates/kiana-interactive/src/components/radius_login_selector.rs`
- `crates/kiana-interactive/src/components/scoped_models_selector.rs`
- `crates/kiana-interactive/src/components/settings_selector.rs`
- `crates/kiana-interactive/src/components/status_indicator.rs`
- `crates/kiana-interactive/src/components/trust_selector.rs`
- `crates/kiana-interactive/src/components/user_message_selector.rs`
- `crates/kiana-interactive/src/interactive_mode.rs`
- `crates/kiana-interactive/src/interactive_mode/commands/mod.rs`
- `crates/kiana-interactive/src/interactive_mode/extension_ui/mod.rs`
- `crates/kiana-interactive/src/model_search.rs`
- `crates/kiana-interactive/src/program_status_reporter.rs`
- `crates/kiana-interactive/src/session_share.rs`
- `crates/kiana-interactive/src/tui_renderer.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-interactive/src/tui_renderer.rs:15` | A | TS等价物, TS的 | //! 暴露"取出的方法"，语义等价于 TS 的 `const m = tui.m`：句柄每次调用都重新解析 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-interactive/src/tui_renderer.rs:1163` | A | TS等价物 | /// 每次调用都重新解析当前渲染器（等价于 TS Proxy 的方法缓存失效检查）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-interactive/src/tui_renderer.rs:1615` | A | 与TS一致 | /// 与 TS 一致，焦点解析失败（句柄无法按地址定位）时仍会请求渲染；容器只在这个同步 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-interactive/src/tui_renderer.rs:1631` | A | 与TS一致 | /// 与 TS 一致，这里**不**请求渲染：`bug-report.ts` 的 `restoreEditor` 末尾有 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-interactive/src/components/armin.rs:25` | A | 与TS一致 | //! 一帧**（与 TS 一致，不按墙钟补帧）；动画在未被渲染时也照常推进，重绘由定时器请求。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-interactive/src/components/armin.rs:700` | A | 与TS一致 | /// 两个与 TS 一致的边界：`width == 0` 时 `available_width == -1`，`js_slice_take` 复刻 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-interactive/src/components/armin.rs:919` | A | 与TS一致 | // 构造顺序与 TS 一致：选效果 → 建最终网格 → 空网格 → initEffect()。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-interactive/src/components/settings_selector.rs:28` | A | 与TS一致 | //! 构造顺序与 TS 一致，顺序本身就是契约（黄金渲染逐行比对）： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-interactive/src/components/settings_selector.rs:1623` | A | 逐字一致 | // 基础数组（24 项，顺序与 TS 逐字一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-interactive/src/components/settings_selector.rs:1930` | A | 与TS一致, 完全一致 | // 图片相关项（级联 splice，与 TS 完全一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-interactive/src/components/status_indicator.rs:10` | A | 与TS一致 | //! [`StatusIndicator`] trait + 共享 [`Loader`] 的四个具体类型；trait 的方法名与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-interactive/src/components/status_indicator.rs:537` | A | 与TS一致 | /// `dispose()` 先停倒计时再停 loader；顺序与 TS 一致（`countdown?.dispose()` 在 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-interactive/src/components/status_indicator.rs:125` | D | 逐字一致 | /// `${seconds}` 逐字一致；集中在此转换，避免调用方各自用 Rust 默认格式化产生漂移。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 14 | `crates/kiana-interactive/src/components/trust_selector.rs:5` | A | 与TS一致 | //! 与 TS 一致：up/down **clamp**（不环绕）；选中项 label 用 accent，未选中用 text。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-interactive/src/components/trust_selector.rs:130` | D | 逐字一致 | /// 之后不再查询信任库——"Saved decision:" 是打开瞬间的快照。布局子项顺序逐字对应 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-interactive/src/components/trust_selector.rs:258` | D | 完全一致 | // 完全一致。不比较 project_trusted，也不识别 session-only 选项（本组件不启用）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 17 | `crates/kiana-interactive/src/components/user_message_selector.rs:160` | A | 与TS一致 | /// 不做截断，宽度可能超出调用方给的 `width`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-interactive/src/components/user_message_selector.rs:317` | A | 与TS一致 | // 列表先构造，回调作为公开字段装上；构造函数的入参顺序与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-interactive/src/components/user_message_selector.rs:322` | A | 必须与TS, 与TS一致 | // 布局顺序必须与 TS 一致：标题/说明各带 1 列水平 padding，边框每次 render 自适应 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 20 | `crates/kiana-interactive/src/interactive_mode/commands/mod.rs:165` | A | TS等价物, TS的 | /// N 通过 `for_each_container_child` 计数（会穿过共享句柄代理，等价于 TS 的直接 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 21 | `crates/kiana-interactive/src/interactive_mode/commands/mod.rs:697` | A | 与TS一致 | /// 只处理第一个命中的分支，顺序与 TS 一致： | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 22 | `crates/kiana-interactive/src/interactive_mode/commands/mod.rs:1240` | A | 逐字一致 | /// 空串。命令分派里的 7/10/4/9 等硬编码长度与 TS 逐字一致（spec §3.2），修改分支时 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 23 | `crates/kiana-interactive/src/components/extension_selector.rs:8` | A | 与TS一致 | //! * 不实现 `Focusable`（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 24 | `crates/kiana-interactive/src/components/extension_selector.rs:301` | A | 与TS一致 | // 分发顺序（与 TS 一致）：展开键 → 移动 → 确认 → 取消。移动分支截断而非环绕， | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 25 | `crates/kiana-interactive/src/components/radius_login_selector.rs:274` | D | 逐字一致 | /// 选中判定依赖内层对选中行的渲染结果与本函数用 `→ ` + `label` 重建的文本逐字相同 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 26 | `crates/kiana-interactive/src/components/radius_login_selector.rs:340` | D | 完全一致 | /// 必须与 `options` 中的某一项完全一致，否则不会出现微光（`render` 按文本匹配）。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 27 | `crates/kiana-interactive/src/components/scoped_models_selector.rs:751` | A | 与TS一致, TS的 | // `None`，这里刻意复刻 TS 的 `undefined.model` 崩溃：不静默降级，行为与 TS 一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 28 | `crates/kiana-interactive/src/components/scoped_models_selector.rs:785` | D | 完全一致 | /// 完全一致；可选的状态行只在构造时创建，顺序在组件生命周期内不变。每次调用都重新 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 29 | `crates/kiana-interactive/src/interactive_mode.rs:1499` | A | 与TS一致 | /// 对应 spec §2.50.3 的 11 步，顺序与 TS 一致（observable）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 30 | `crates/kiana-interactive/src/interactive_mode.rs:3426` | A | 完全一致, TS的 | /// `/reload` 后仍用旧绑定（TS 的共享可变单例没有这个限制）。要完全一致需要给 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 31 | `crates/kiana-interactive/src/interactive_mode/extension_ui/mod.rs:73` | A | 与TS一致 | //! responder——与 TS 一致，被 reset 掉的对话框 Promise 永远挂起（测试 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 32 | `crates/kiana-interactive/src/interactive_mode/extension_ui/mod.rs:1772` | A | 与TS一致 | // 与 TS 一致：只 dispose 对话框组件，等待中的对话框 Promise 保持挂起。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 33 | `crates/kiana-interactive/src/model_search.rs:4` | A | 逐字一致, 对应TS | //! 的判定。公式逐字对应 TS，空白与 Unicode 分词敏感。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 34 | `crates/kiana-interactive/src/model_search.rs:15` | A | 逐字一致, 对应TS | //! 所有字符串公式逐字对应 TS（包括重复出现的 provider、空 `name` 不追加空格、JS 空白 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 35 | `crates/kiana-interactive/src/program_status_reporter.rs:287` | A | TS等价物, TS的 | // 去重键包含 `app` 与键序，等价于 TS 的 `JSON.stringify(status)`。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 36 | `crates/kiana-interactive/src/program_status_reporter.rs:47` | D | 完全一致 | //! - `last_report` 不随 `reset` 清空，所以重置后的状态若与上次完全一致会被去重吞掉。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 37 | `crates/kiana-interactive/src/session_share.rs:36` | A | 保真, TS源 | //! - 本文件以 TS 源码为规范，保真优先于惯用写法；改文案或分支前必须先对照 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 38 | `crates/kiana-interactive/src/session_share.rs:286` | D | 保真 | /// 调用方再回退到 `io::Error` 自身的 Display。映射表是文案保真用的**子集**，不是全部 errno。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 39 | `crates/kiana-interactive/src/components/assistant_message.rs:61` | A | 逐字节一致 | // 常量与 TS 逐字节一致；消费方见上面的模块文档（alt_screen 的滚动定位与 layout 的剥除）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 40 | `crates/kiana-interactive/src/components/custom_editor.rs:246` | A | TS等价物 | /// 编辑器一致）。这里的目标动作处理器同样在调用时回查 `source`，等价于 TS 复制同一个函数对象。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
