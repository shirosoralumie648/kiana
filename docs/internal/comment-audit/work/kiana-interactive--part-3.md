# 注释清理工作单：kiana-interactive--part-3

- 待处理条目：19
- 涉及文件：19

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

- `crates/kiana-interactive/src/components/custom_entry.rs`
- `crates/kiana-interactive/src/components/extension_editor.rs`
- `crates/kiana-interactive/src/components/extension_input.rs`
- `crates/kiana-interactive/src/components/first_time_setup.rs`
- `crates/kiana-interactive/src/components/footer.rs`
- `crates/kiana-interactive/src/components/oauth_selector.rs`
- `crates/kiana-interactive/src/components/show_images_selector.rs`
- `crates/kiana-interactive/src/components/theme_selector.rs`
- `crates/kiana-interactive/src/components/user_message.rs`
- `crates/kiana-interactive/src/interactive_mode/commands/auth.rs`
- `crates/kiana-interactive/src/interactive_mode/commands/bash_compact.rs`
- `crates/kiana-interactive/src/interactive_mode/commands/session.rs`
- `crates/kiana-interactive/src/interactive_mode/events.rs`
- `crates/kiana-interactive/src/interactive_mode/render.rs`
- `crates/kiana-interactive/tests/components_mermaid.rs`
- `crates/kiana-interactive/tests/components_model_selector.rs`
- `crates/kiana-interactive/tests/components_tree_selector.rs`
- `crates/kiana-interactive/tests/external_editor.rs`
- `crates/kiana-interactive/tests/interactive_mode_extension_ui.rs`

## 待处理条目

| # | 位置 | 类别 | 命中 | 摘要 | 建议 |
|---:|---|---|---|---|---|
| 1 | `crates/kiana-interactive/src/components/custom_entry.rs:76` | A | 逐字一致 | /// `Box<dyn Any>`。因此错误框里的文本不保证与 TS 逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 2 | `crates/kiana-interactive/src/components/extension_editor.rs:11` | A | 与TS一致 | //! `app.editor.external`；取消键仍走全局 `getKeybindings()`，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 3 | `crates/kiana-interactive/src/components/extension_input.rs:274` | A | 与TS一致 | // 分发顺序（与 TS 一致）：确认 → 取消 → 其余交给 Input。确认分支在回调前就把 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 4 | `crates/kiana-interactive/src/components/first_time_setup.rs:5` | A | 与TS一致 | //! 与 TS 一致：每次变化都重建整个子树（主题预览要重着色所有文本）；选择移动 **clamp** | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 5 | `crates/kiana-interactive/src/components/footer.rs:194` | A | 逐字一致 | /// 实现 `js_to_fixed`；两者都是为了与 TS 输出逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 6 | `crates/kiana-interactive/src/components/oauth_selector.rs:5` | A | 与TS一致 | //! 与 TS 一致：up/down **clamp**（不环绕）；选项由宿主投影，组件**不做**任何 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 7 | `crates/kiana-interactive/src/components/show_images_selector.rs:5` | A | 与TS一致 | //! 与 TS 一致：`DynamicBorder` + `SelectList(5)` + `DynamicBorder`，**无 Spacer**； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 8 | `crates/kiana-interactive/src/components/theme_selector.rs:5` | A | 与TS一致 | //! 与 TS 一致：只有 `DynamicBorder` + `SelectList` + `DynamicBorder`，**无 Spacer**； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 9 | `crates/kiana-interactive/src/components/user_message.rs:59` | A | 逐字节一致 | // 控制序列，以 BEL 结尾；不含可见字符，因此不参与宽度计算。常量与 TS 逐字节一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 10 | `crates/kiana-interactive/src/interactive_mode/commands/auth.rs:64` | A | 逐字一致 | //! - **文案是契约**：`show_status` / `show_error` 的字符串与 TS 逐字对应，集成测试直接 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 11 | `crates/kiana-interactive/src/interactive_mode/commands/bash_compact.rs:267` | A | 与TS一致 | /// 重复提示（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 12 | `crates/kiana-interactive/src/interactive_mode/commands/session.rs:117` | A | 逐字一致 | /// （`js_to_locale_string` / `js_to_fixed`）保持与 TS 输出逐字一致。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 13 | `crates/kiana-interactive/src/interactive_mode/events.rs:236` | A | 与TS一致 | // 条目不在上下文头部，直接忽略（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 14 | `crates/kiana-interactive/src/interactive_mode/render.rs:101` | A | 与TS一致 | /// `theme.fg(color, text)`（主题必须已初始化，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 15 | `crates/kiana-interactive/tests/components_mermaid.rs:331` | D | 逐字一致 | // 直接对比 plain 行：边框、箭头与宽度都逐字一致。 | 人工判断：确认是否仍有独立技术原因；没有则删除 |
| 16 | `crates/kiana-interactive/tests/components_model_selector.rs:1216` | A | 与TS一致 | // selectedIndex 保持 -1：下一次渲染同样抛错（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 17 | `crates/kiana-interactive/tests/components_tree_selector.rs:140` | A | 与TS一致 | /// `buildTree(entries)`：按 `parentId` 关系建树（缺失父节点的条目被丢弃，与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 18 | `crates/kiana-interactive/tests/external_editor.rs:3` | A | 与TS一致 | //! 与 TS 一致：用 `fake-external-editor.mjs`（逐字复制）作为外部编辑器，通过 node 启动； | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |
| 19 | `crates/kiana-interactive/tests/interactive_mode_extension_ui.rs:888` | A | 与TS一致 | /// `resetExtensionUI` 只 dispose 对话框，等待中的 Promise 保持挂起（与 TS 一致）。 | 改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由 |

## 完成后返回

返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。
