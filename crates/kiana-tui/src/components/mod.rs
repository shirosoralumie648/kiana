//! components 目录（48 个组件中属于 tui 包的部分；对应 `packages/tui/src/components/**`）。
//!
//! 本 crate 内不得出现第二份实现：每个文件都是对应 TS 模块的唯一 Rust 版本。
//!
//! # 职责与边界
//!
//! 这里放的是 TS `packages/tui` 自己拥有的基础组件（文本、输入、布局栈、列表、图片、
//! Markdown 等）。上层包（`kiana-interactive`、`kiana-ui-kit`、`kiana-extensions`）在这些
//! 组件之上组合界面，不重复实现它们；`crate::index` 把常用类型再导出到 crate 根
//! （例如 `kiana_tui::Input`），模块路径 `kiana_tui::components::input::Input` 同样可用。
//!
//! 目录本身只是组织单位：本文件不定义类型、不做再导出、没有 `use`。新增组件时在下面追加
//! `pub mod`，并按 COMMENT_STYLE §4.2 把模块文档写进新文件的 `//!`——**不要**在 `pub mod`
//! 上方写外层 `///`（外层文档与文件内 `//!` 合并后会在父模块作用域解析链接，导致 `//!` 里
//! 指向本模块条目的短链接失效）。
//!
//! # 模块索引
//!
//! | 模块 | 对应 TS | 作用 |
//! | --- | --- | --- |
//! | `box_component` | `box.ts` | 给子项加留白与可选背景色的容器，缓存渲染结果 |
//! | `cancellable_loader` | `cancellable-loader.ts` | 带取消令牌的加载组件（Esc 触发取消） |
//! | `editor` | `editor.ts` | 多行编辑器：换行、粘贴、历史、kill ring、撤销、补全、鼠标 |
//! | `h_stack` | `h-stack.ts` | 水平栈：先测量再分配宽度，按列合成 |
//! | `image` | `image.ts` | 按终端能力输出 Kitty/iTerm2 图片，不支持时文本兜底 |
//! | `input` | `input.ts` | 单行输入：水平滚动、伪光标、括号粘贴、kill/yank、撤销 |
//! | `loader` | `loader.ts` | 带动画指示器的加载组件（按经过时间惰性取帧） |
//! | `markdown` | `markdown.ts` | 把 Markdown token 流渲染成带 ANSI 样式的行 |
//! | `mouse_region` | `mouse-region.ts` | 不改变渲染、给任意组件附加鼠标处理的装饰器 |
//! | `scroll_view` | `scroll-view.ts` | 单子项滚动容器，向布局器暴露滚动条状态 |
//! | `select_list` | `select-list.ts` | 可选择列表：循环选择、过滤、两列布局、滚轮/点击 |
//! | `settings_list` | `settings-list.ts` | 设置列表：子菜单、取值循环、可选模糊搜索 |
//! | `spacer` | `spacer.ts` | 渲染若干空行的占位组件 |
//! | `stack` | `stack.ts` | 栈布局基类，并再导出尺寸分配/可见性算法 |
//! | `text_component` | `text.ts` | 多行文本：按词换行、上下左右留白、自定义背景 |
//! | `truncated_text` | `truncated-text.ts` | 单行截断并补齐到整宽的文本 |
//! | `v_stack` | `v-stack.ts` | 垂直栈：按自然高度与 `gap` 拼接，不裁剪总高度 |

// --- 子模块 ---
pub mod box_component;
pub mod cancellable_loader;
pub mod editor;
pub mod h_stack;
pub mod image;
pub mod input;
pub mod loader;
pub mod markdown;
pub mod mouse_region;
pub mod scroll_view;
pub mod select_list;
pub mod settings_list;
pub mod spacer;
pub mod stack;
pub mod text_component;
pub mod truncated_text;
pub mod v_stack;
