//! `terminal-image.ts`：终端图片能力探测、Kitty/iTerm2 编码、图片元数据注册表、
//! 裁剪/行列换算、OSC 8 超链接与文本兜底。
//!
//! | TS | Rust |
//! |---|---|
//! | `getCapabilities` / `detectCapabilities` / `setCapabilities` / `setCapabilityOverrides` | [`get_capabilities`] / [`detect_capabilities`] / [`set_capabilities`] / [`set_capability_overrides`] |
//! | `resetCapabilitiesCache` / `getCellDimensions` / `setCellDimensions` | 同名 snake_case |
//! | `getTerminalColorMode` | [`get_terminal_color_mode`]；零参数形态 [`get_terminal_color_mode_default`] |
//! | `isImageLine` / `allocateImageId` | [`is_image_line`] / [`allocate_image_id`] |
//! | `encodeKitty` / `encodeITerm2` / `deleteKittyImage` / `deleteAllKittyImages` / `deleteAllKittyPlacements` | 同名 snake_case |
//! | `registerKittyImageMetadata` / `getKittyImageMetadata` / `getKittyImagePlacement` / `getKittyImagePlacementRows` / `cropKittyImageLine` / `getMaxKittyImageRows` | 同名 snake_case |
//! | `calculateImageCellSize` / `calculateImageRows` | [`calculate_image_cell_size`] / [`calculate_image_rows`]；TS 默认参数的形态 [`calculate_image_cell_size_default`] / [`calculate_image_rows_default`] |
//! | `getPngDimensions` / `getJpegDimensions` / `getGifDimensions` / `getWebpDimensions` / `getImageDimensions` | 同名 snake_case |
//! | `renderImage` / `hyperlink` / `imageFallback` | [`render_image`] / [`hyperlink`] / [`image_fallback`] |
//!
//! ## 表示差异（均不改变可观察输出）
//!
//! 1. **全局可变单例**：TS 的模块级 `cachedCapabilities` / `capabilityOverrides` /
//!    `cellDimensions` / `kittyImageMetadata` 用 `thread_local!` + `RefCell` 复刻（见
//!    `ARCHITECTURE.md` §8.2 第 2 条），语义与 JS 模块级变量一致（同一线程内共享，测试可重置）。
//!    `kittyImageMetadata` 用 `IndexMap` 保留插入顺序（TS `Map`），LRU 淘汰按插入序。
//! 2. **数字**：TS 的 `number` 在 Rust 里是 `f64`（尺寸与行/列字段）。凡进入字符串的数字一律走
//!    `js_number_text`（模板字面量语义，内部用 `kiana_json::js_num::format_js_number`），
//!    与 JS 的 `Number::toString` 一致。
//! 3. **`Partial<TerminalCapabilities>`**：TS 的「键存在但值为 `null`」与「键不存在」是两种语义
//!    （`setCapabilityOverrides({ images: null })` 会把图片显式关掉），因此
//!    [`CapabilityOverrides`] 用 `Option<Option<ImageProtocol>>` 表达 `images`。
//! 4. **`process.env` / `process.platform` / `homedir()`**：全部在调用点读取，未参数化——TS 也是
//!    每次调用现读，测试通过修改环境变量并 `reset_capabilities_cache()` 驱动。并发测试需串行
//!    （`tests/terminal_image.rs` 用进程级互斥锁）。
//! 5. **`isAbsolute(path)`**：TS 用 `node:path` 的平台相关实现；Rust 侧用平台感知的
//!    `is_absolute_path`（Windows 上盘符/UNC/根相对都算绝对；POSIX 上只认 `/` 前缀）。
//! 6. **`pathToFileURL(filename).href`**：TS 先 `path.resolve`（规范化 `.`/`..`/重复分隔符）再按
//!    WHATWG URL 路径集合做百分号编码。Rust 侧 [`path_to_file_url`] 复刻 Node 26 的行为
//!    （保留 `a-zA-Z0-9` 与 `!$&'()*+,-./:;=@_`，其余按 UTF-8 字节 `%XX`；`~` 也编码）。
//! 7. **TS 默认参数**：Rust 没有默认参数，TS 的「省略即取默认值」统一用 `_default` 伴生函数
//!    表达（[`detect_capabilities_default`] / [`crate::text::truncate_to_width_default`] 的同款约定）：
//!    [`get_terminal_color_mode_default`]、[`calculate_image_cell_size_default`]、
//!    [`calculate_image_rows_default`]。默认单元格尺寸是**静态 9x18**，不是 [`get_cell_dimensions`]。
//!
//! ## 已知偏差
//!
//! - **`pathToFileURL` 的异常（仅 Windows 构建可达）**：Node 26 对「绝对但格式错误」的 UNC 路径
//!   抛 `ERR_INVALID_ARG_VALUE`（`\\server` / `\\?\UNC\server` 缺资源路径；`\\\\share\x` 空
//!   servername）或 `ERR_INVALID_URL`（host 含空格或非法百分号转义，如 `\\a b\share\x`），
//!   异常从 `imageFallback` 直接冒出（无 try/catch，进程崩溃）。[`image_fallback`] 的跨单元冻结
//!   签名是 `-> String`（`spec/tui-components.md` §5.1，没有错误通道），因此 [`path_to_file_url`]
//!   对这些输入退化为 `file:///...` 文本而不 panic。所有格式良好的路径（POSIX 与 Windows）逐字
//!   一致；该偏差只影响 Windows 上 `isAbsolute` 为真却无法解析的 UNC 路径（不是可读图片路径），
//!   Windows 也不在验收黄金测试矩阵内（`ARCHITECTURE.md` §8 第 26 条）。
//!
//! ## 模块结构与调用方
//!
//! - **能力层**：[`detect_capabilities`] 按固定分支顺序（`spec/tui-core.md` §2.15.2）读环境变量，
//!   再用 `KIANA_HYPERLINKS` / `KIANA_IMAGE_PROTOCOL` / `KIANA_TRUE_COLOR` 覆盖；[`get_capabilities`]
//!   把最终结果缓存在线程本地，[`set_capability_overrides`] 与 [`reset_capabilities_cache`]
//!   负责让缓存失效。
//! - **渲染层**：[`render_image`] 按缓存的能力选择 Kitty/iTerm2 编码；没有图片能力时返回 `None`，
//!   调用方改走 [`image_fallback`] 的文本行（`components/image.rs` 就是这样消费的）。
//! - **屏幕层**：`layout` 用 [`get_kitty_image_metadata`] + [`crop_kitty_image_line`] 裁剪滚动区中
//!   只露出一部分的 Kitty 图片；`alt_screen` 用 [`get_kitty_image_placement`] 把已上传图片的行
//!   换成只含 placement 的命令，避免重复传输（是否复用由 `transmission_generation` 判断）。
//! - **解析层**：[`get_png_dimensions`] 等先解码整段 base64，再只检查头部（PNG/GIF/WEBP 读固定
//!   偏移，JPEG 扫描段直到 SOF），不校验像素数据或文件完整性。
//!
//! ## 状态与线程约束
//!
//! 能力缓存、覆盖集、单元格尺寸、Kitty 元数据表、transmission 计数器与最大行数都是
//! `thread_local!`（见下方 `CACHED_CAPABILITIES` 等静态项），同一线程内共享、跨线程互不可见。
//! 因此在测试里改完环境变量必须调用 [`reset_capabilities_cache`]；新线程首次使用时看到的是
//! 默认值（无缓存、单元格 9x18），需要重新 [`set_capabilities`] / [`set_cell_dimensions`]。
//! Kitty 元数据表最多保留 `MAX_REGISTERED_KITTY_IMAGES`（1000）项，超出时按插入序淘汰最旧项；
//! `get_max_kitty_image_rows` 单调不减，且 [`reset_capabilities_cache`] 不会重置它。
//!
//! ## 失败路径
//!
//! - 行里没有 `\x1b_G`/控件串：以下查询都拿不到控件串，`get_kitty_image_metadata`、
//!   `get_kitty_image_placement_rows`、`get_kitty_image_placement` 返回 `None`，
//!   `crop_kitty_image_line` 返回原行。
//! - 有 `\x1b_G` 但缺 `i=` 或 ID 未注册：`get_kitty_image_metadata` / `get_kitty_image_placement`
//!   返回 `None`，`crop_kitty_image_line` 返回原行；`get_kitty_image_placement_rows` 仍可凭
//!   显式 `r=` 返回行数（没有 `r=` 时返回 `None`）。
//! - 图片头过短或签名不符：各 `get_*_dimensions` 返回 `None`。`render_image` 不调用这些函数，
//!   尺寸由入参提供。
//! - tmux 探针超时、命令不存在或非零退出：按「不转发 OSC 8」处理（`false`），不向上传播错误。
//! - Windows 上 Node 会抛异常的非法 UNC 路径：`path_to_file_url` 退化输出文本而绝不 panic
//!   （见「已知偏差」）。

use std::cell::RefCell;
use std::path::PathBuf;

use indexmap::IndexMap;
use kiana_json::js_num::format_js_number;

use crate::terminal_colors::TerminalColorMode;

/// 图片协议（TS `ImageProtocol`）。`None` 对应 TS 的 `null`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageProtocol {
    /// Kitty graphics protocol（传输命令以 `\x1b_G` 开头）。
    Kitty,
    /// iTerm2 inline image protocol（OSC 1337 `File=` 命令）。
    Iterm2,
}

impl ImageProtocol {
    /// TS 的字面量（`"kitty"` / `"iterm2"`）。
    pub fn as_str(self) -> &'static str {
        match self {
            ImageProtocol::Kitty => "kitty",
            ImageProtocol::Iterm2 => "iterm2",
        }
    }
}

/// 终端能力（TS `TerminalCapabilities`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TerminalCapabilities {
    /// 支持的图片协议；`None` 表示不显示图片（TS 的 `null`）。
    pub images: Option<ImageProtocol>,
    /// 是否支持 24 位真彩；[`get_terminal_color_mode`] 据此在 `Truecolor` 与 `Palette256`
    /// 之间选择。
    pub true_color: bool,
    /// 是否支持 OSC 8 超链接。为假时不应把 [`hyperlink`] 的输出送给终端
    /// （不支持的终端可能吞掉链接文本）。
    pub hyperlinks: bool,
}

/// `Partial<TerminalCapabilities>`：未给出的字段保留自动探测结果。
///
/// `images: Some(None)` 表示「显式设为 `null`」（关掉图片），与 `None`（不覆盖）不同。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct CapabilityOverrides {
    /// `Some(Some(proto))` 强制启用该协议，`Some(None)` 强制关闭图片，`None` 表示不覆盖。
    pub images: Option<Option<ImageProtocol>>,
    /// `Some` 时覆盖探测到的真彩能力。
    pub true_color: Option<bool>,
    /// `Some` 时覆盖探测到的超链接能力；此时 [`get_capabilities`] 不再调用 tmux 探针。
    pub hyperlinks: Option<bool>,
}

impl CapabilityOverrides {
    /// 全空的覆盖集（不清除、不设置任何字段）。
    pub const fn new() -> Self {
        Self {
            images: None,
            true_color: None,
            hyperlinks: None,
        }
    }
}

/// 终端单元格像素尺寸（TS `CellDimensions`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellDimensions {
    /// 单个字符单元格的宽度（像素）。
    pub width_px: f64,
    /// 单个字符单元格的高度（像素）。
    pub height_px: f64,
}

impl Default for CellDimensions {
    fn default() -> Self {
        // TS: `{ widthPx: 9, heightPx: 18 }`
        Self {
            width_px: 9.0,
            height_px: 18.0,
        }
    }
}

/// 图片像素尺寸（TS `ImageDimensions`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageDimensions {
    /// 原图像素宽度。
    pub width_px: f64,
    /// 原图像素高度。
    pub height_px: f64,
}

/// 单元格行列尺寸（TS `ImageCellSize`）。
///
/// TS 的字段是普通 `number`（理论上可为 `NaN`/`Infinity`/`1e30`）。骨架把这里固定成 `usize`
/// （终端行列数，且 `RenderedImage`/`KittyImageMetadata` 的跨单元契约也用 `usize`），因此
/// **退化输入**（`NaN`、`Infinity`、超过 `usize::MAX` 的 `maxWidthCells`）只能饱和/归零；
/// 所有有限、正的常规输入与 TS 逐值一致（20k 组随机对拍 0 差异）。经 `renderImage` 时两者
/// 产生的转义序列相同：JS 里 `NaN`/`0` 都是 falsy，不会写出 `c=`/`r=` 参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageCellSize {
    /// 横向占用的终端列数。
    pub columns: usize,
    /// 纵向占用的终端行数。
    pub rows: usize,
}

/// `renderImage` 的选项（TS `ImageRenderOptions`）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ImageRenderOptions {
    /// 最大列数；`None` 时 [`render_image`] 取 80。
    pub max_width_cells: Option<f64>,
    /// 最大行数；`None` 表示不额外限制高度，行数只由宽度方向的比例决定。
    pub max_height_cells: Option<f64>,
    /// 仅 iTerm2 使用：`Some(false)` 写 `preserveAspectRatio=0`；`None` 等同保持比例（缺省）。
    pub preserve_aspect_ratio: Option<bool>,
    /// Kitty 图片 ID；提供时复用/替换该 ID 的图片。`Some(0)` 会注册元数据，但不会写出 `i=`
    /// （JS 真值判定）。
    pub image_id: Option<u64>,
    /// Kitty 是否应用默认的光标移动（TS 默认 `undefined` → 应用）。
    pub move_cursor: Option<bool>,
}

/// `renderImage` 的结果（TS `{ sequence, columns, rows, imageId? }`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedImage {
    /// 可直接写入屏幕行的转义序列；Kitty 大图会由多个分块命令串联而成。
    pub sequence: String,
    /// 计算出的终端列数。
    pub columns: usize,
    /// 计算出的终端行数；调用方通常据此补空行占位。
    pub rows: usize,
    /// 请求的 Kitty 图片 ID（原样回填，包括不会写进序列的 0）；iTerm2 恒为 `None`。
    pub image_id: Option<u64>,
}

/// 注册进元数据表的图片信息（TS `KittyImageMetadata`）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KittyImageMetadata {
    /// Kitty 图片 ID（`i=` 参数）。
    pub image_id: u64,
    /// 渲染时占用的终端列数（`c=`）。
    pub columns: usize,
    /// 渲染时占用的终端行数（`r=`）。
    pub rows: usize,
    /// 原图像素宽度。
    pub width_px: f64,
    /// 原图像素高度。
    pub height_px: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RegisteredKittyImageMetadata {
    image_id: u64,
    columns: usize,
    rows: usize,
    width_px: f64,
    height_px: f64,
    transmission_generation: u64,
}

/// 只放置命令（TS `KittyImagePlacement`）。
#[derive(Debug, Clone, PartialEq)]
pub struct KittyImagePlacement {
    /// 该图片在注册表中的 ID。
    pub image_id: u64,
    /// 注册该项时的线程本地 transmission 计数（`KITTY_TRANSMISSION_GENERATION`，同一线程内每次
    /// 注册自增）；`alt_screen` 在同一线程内与缓存值比较，判断上传是否仍有效。
    pub transmission_generation: u64,
    /// 本次传输（含所有 `m=1` 分块）的长度，按 **UTF-16 code unit** 计，不是字节数。
    pub transmission_bytes: usize,
    /// 解码后大小的估算：`width_px * height_px * 4`（每像素按 4 字节算）；
    /// `alt_screen` 用它做离屏图片缓存配额。
    pub estimated_decoded_bytes: f64,
    /// 本次 placement 占用的行数：`r=` 显式值优先，否则取注册表的 `rows`。
    pub rows: usize,
    /// 只含 placement 的 `a=p` 命令（保留原控件串中属于 17 个放置相关键的那些）。
    pub sequence: String,
    /// 把原行中的传输段替换成 `sequence` 后的整行；
    /// `alt_screen` 在确认上传缓存仍有效时直接输出它。
    pub replacement_line: String,
}

const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

thread_local! {
    static CACHED_CAPABILITIES: RefCell<Option<TerminalCapabilities>> = const { RefCell::new(None) };
    static CAPABILITY_OVERRIDES: RefCell<CapabilityOverrides> =
        const { RefCell::new(CapabilityOverrides::new()) };
    static CELL_DIMENSIONS: RefCell<CellDimensions> = const {
        RefCell::new(CellDimensions {
            width_px: 9.0,
            height_px: 18.0,
        })
    };
    /// TS `kittyImageMetadata`（`Map`，插入序）。
    static KITTY_IMAGE_METADATA: RefCell<IndexMap<u64, RegisteredKittyImageMetadata>> =
        RefCell::new(IndexMap::new());
    static KITTY_TRANSMISSION_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static MAX_KITTY_IMAGE_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

// ---------------------------------------------------------------------------
// 环境与平台
// ---------------------------------------------------------------------------

/// `process.platform === "win32"`。
fn is_windows_platform() -> bool {
    cfg!(windows)
}

/// `process.env[key]`。
///
/// Node 的 `process.env` 是 `string` 视图：非 UTF-8 字节以 U+FFFD 替换后仍是**存在**的字符串，
/// 所以这里用 `var_os` + 有损转换，而不是 `std::env::var`（后者对非 UTF-8 直接报错，会把
/// 「变量存在」误判成「未设置」）。
fn env_var(key: &str) -> Option<String> {
    std::env::var_os(key).map(|value| value.to_string_lossy().into_owned())
}

/// TS 的 `process.env.X?.toLowerCase() || ""`。
fn env_var_lower(key: &str) -> String {
    env_var(key).unwrap_or_default().to_lowercase()
}

/// TS 的 `Boolean(process.env.X)`。
fn env_var_present(key: &str) -> bool {
    env_var(key).is_some_and(|v| !v.is_empty())
}

/// `homedir()`（`node:os`）。
///
/// POSIX 上 Node 的 `os.homedir()` **只要 `$HOME` 存在就直接返回**（空串也返回空串，
/// 不会回退到 `getpwuid_r`）；`$HOME` 完全未设置时才查 passwd。Windows 上依次是
/// `USERPROFILE`、`HOMEDRIVE` + `HOMEPATH`。`dirs::home_dir` 会忽略空 `$HOME`，所以这里先自己读。
pub fn home_dir() -> Option<PathBuf> {
    if is_windows_platform() {
        if let Some(profile) = std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()) {
            return Some(PathBuf::from(profile));
        }
        if let (Some(drive), Some(path)) = (
            std::env::var_os("HOMEDRIVE").filter(|v| !v.is_empty()),
            std::env::var_os("HOMEPATH").filter(|v| !v.is_empty()),
        ) {
            let mut home = drive;
            home.push(path);
            return Some(PathBuf::from(home));
        }
    } else if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home));
    }
    dirs::home_dir()
}

/// `isAbsolute(path)`（`node:path` 的平台相关实现）。
///
/// Windows：`C:\`、`C:/`、`\\server`、`\foo` 都算绝对（Node 的 win32 `isAbsolute` 用
/// `path.win32.resolve` 之外的独立判定，根相对也算）；POSIX 上只认 `/` 前缀。
fn is_absolute_path(path: &str) -> bool {
    if is_windows_platform() {
        let bytes = path.as_bytes();
        if path.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\')
        {
            return true;
        }
        if path.len() >= 2
            && (bytes[0] == b'/' || bytes[0] == b'\\')
            && (bytes[1] == b'/' || bytes[1] == b'\\')
        {
            return true;
        }
        path.starts_with('\\') || path.starts_with('/')
    } else {
        path.starts_with('/')
    }
}

// ---------------------------------------------------------------------------
// Windows 路径解析（`path.win32.resolve` / `normalizeString` 的逐行移植）
// ---------------------------------------------------------------------------

/// `path.win32` 的 `isPathSeparator`（`/` 或 `\`）。
fn is_path_separator(code: u8) -> bool {
    code == b'/' || code == b'\\'
}

/// `path.win32` 的 `isWindowsDeviceRoot`（`a-zA-Z`）。
fn is_windows_device_root(code: u8) -> bool {
    code.is_ascii_alphabetic()
}

/// `process.cwd()`。
///
/// 在 Linux 上跑 Windows 分支（测试/对拍）时 Node 的 `process.cwd()` 也是 POSIX 路径，
/// 两者都由同一套 win32 解析器处理，所以这里直接用宿主平台的 cwd。
fn windows_cwd() -> String {
    std::env::current_dir()
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// `win32.resolve` 对单个路径的根匹配结果（Node 的 `rootEnd` / `device` / `isAbsolute`）。
struct WindowsRoot {
    device: String,
    root_end: usize,
    is_absolute: bool,
}

/// Node `win32.resolve` 中「Try to match a root」一段的逐行移植。
fn parse_windows_root(path: &str) -> WindowsRoot {
    let bytes = path.as_bytes();
    let len = bytes.len();
    let mut root_end = 0usize;
    let mut device = String::new();
    let mut is_absolute = false;

    if len == 1 {
        if is_path_separator(bytes[0]) {
            // `path` 只有一个分隔符。
            root_end = 1;
            is_absolute = true;
        }
    } else if len > 1 && is_path_separator(bytes[0]) {
        // 可能的 UNC 根。
        //
        // 以分隔符开头说明至少是某种绝对路径（UNC 或根相对）。
        is_absolute = true;
        if is_path_separator(bytes[1]) {
            // 开头是两个分隔符：依次匹配「非分隔符」「分隔符」「非分隔符」三段。
            let mut j = 2usize;
            let mut last = j;
            while j < len && !is_path_separator(bytes[j]) {
                j += 1;
            }
            if j < len && j != last {
                let first_part = path[last..j].to_string();
                last = j;
                while j < len && is_path_separator(bytes[j]) {
                    j += 1;
                }
                if j < len && j != last {
                    last = j;
                    while j < len && !is_path_separator(bytes[j]) {
                        j += 1;
                    }
                    if j == len || j != last {
                        if first_part != "." && first_part != "?" {
                            // 匹配到 UNC 根（`\\server\share`）。
                            device = format!("\\\\{}\\{}", first_part, &path[last..j]);
                            root_end = j;
                        } else {
                            // 匹配到设备根（`\\.\`、`\\?\`）。
                            device = format!("\\\\{}", first_part);
                            root_end = 4;
                        }
                    }
                }
            }
        } else {
            root_end = 1;
        }
    } else if len >= 2 && is_windows_device_root(bytes[0]) && bytes[1] == b':' {
        // 可能的盘符根。
        device = path[0..2].to_string();
        root_end = 2;
        if len > 2 && is_path_separator(bytes[2]) {
            // 盘符后跟分隔符视为绝对路径。
            is_absolute = true;
            root_end = 3;
        }
    }

    WindowsRoot {
        device,
        root_end,
        is_absolute,
    }
}

/// Node 的 `normalizeString(path, allowAboveRoot, "\\", isPathSeparator)`。
///
/// 逐行移植：按 `\` 或 `/` 切段，`.` 丢弃、`..` 弹掉上一段（`allow_above_root` 时在头部累积
/// `..`），空段丢弃。UNC 的 server/share 已在 [`windows_resolve`] 里作为 device 固定。
fn normalize_string(path: &str, allow_above_root: bool) -> String {
    let bytes = path.as_bytes();
    let mut res = String::new();
    let mut last_segment_length = 0usize;
    let mut last_slash: isize = -1;
    let mut dots: isize = 0;
    let mut code: u8 = 0;
    let mut i = 0usize;
    // 逐段刷新：到达末尾时，若最后一个字符不是分隔符，就把 `code` 当作一个虚拟的 `/`
    // 再走一轮，用来把最后一段也写进 `res`（Node 的原始实现用同样的哨兵）。
    loop {
        if i < bytes.len() {
            code = bytes[i];
        } else if is_path_separator(code) {
            break;
        } else {
            code = b'/';
        }

        if is_path_separator(code) {
            let start = (last_slash + 1) as usize;
            if last_slash == i as isize - 1 || dots == 1 {
                // 空段或 `.`：丢弃。
            } else if dots == 2 {
                let res_bytes = res.as_bytes();
                let tail_is_not_dotdot = res.len() < 2
                    || last_segment_length != 2
                    || res_bytes[res.len() - 1] != b'.'
                    || res_bytes[res.len() - 2] != b'.';
                if tail_is_not_dotdot {
                    if res.len() > 2 {
                        let last_slash_index =
                            res.len() as isize - last_segment_length as isize - 1;
                        if last_slash_index == -1 {
                            res.clear();
                            last_segment_length = 0;
                        } else {
                            res.truncate(last_slash_index as usize);
                            let sep = res.rfind('\\').map_or(-1, |p| p as isize);
                            last_segment_length = (res.len() as isize - 1 - sep) as usize;
                        }
                        last_slash = i as isize;
                        dots = 0;
                        i += 1;
                        continue;
                    } else if !res.is_empty() {
                        res.clear();
                        last_segment_length = 0;
                        last_slash = i as isize;
                        dots = 0;
                        i += 1;
                        continue;
                    }
                }
                if allow_above_root {
                    if res.is_empty() {
                        res.push_str("..");
                    } else {
                        res.push('\\');
                        res.push_str("..");
                    }
                    last_segment_length = 2;
                }
            } else {
                if res.is_empty() {
                    res.push_str(&path[start..i]);
                } else {
                    res.push('\\');
                    res.push_str(&path[start..i]);
                }
                last_segment_length = i - start;
            }
            last_slash = i as isize;
            dots = 0;
        } else if code == b'.' && dots != -1 {
            dots += 1;
        } else {
            dots = -1;
        }
        i += 1;
    }
    res
}

/// `path.win32.resolve(path)`（单参数调用；含 cwd 回退与 `=C:` 驱动器工作目录）。
fn windows_resolve(path: &str) -> String {
    let mut resolved_device = String::new();
    let mut resolved_tail = String::new();
    let mut resolved_absolute = false;

    // Node 对单参数路径走 i = 0（参数本身）与 i = -1（cwd）两轮。
    for i in [0i8, -1i8] {
        let input = if i >= 0 {
            if path.is_empty() {
                continue;
            }
            path.to_string()
        } else if resolved_device.is_empty() {
            let cwd = windows_cwd();
            // Fast path：参数为空或 `.` 且 cwd 以分隔符开头时直接返回（`!isWindows` 时把 `/` 换成 `\`）。
            let starts_with_sep = cwd
                .as_bytes()
                .first()
                .is_some_and(|byte| is_path_separator(*byte));
            if (path.is_empty() || path == ".") && starts_with_sep {
                return cwd.replace('/', "\\");
            }
            cwd
        } else {
            // Windows 的「驱动器专属当前目录」（`=C:`）。不存在或不属于该盘时回退到进程 cwd，
            // 再不合规就退到该盘根。
            let mut candidate = env_var(&format!("={}", resolved_device))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(windows_cwd);
            let first_two: String = candidate.chars().take(2).collect();
            let third_is_backslash = candidate.as_bytes().get(2) == Some(&b'\\');
            if !first_two.to_lowercase().eq(&resolved_device.to_lowercase()) && third_is_backslash {
                candidate = format!("{}\\", resolved_device);
            }
            candidate
        };

        let root = parse_windows_root(&input);
        if !root.device.is_empty() {
            if !resolved_device.is_empty() {
                if !root
                    .device
                    .to_lowercase()
                    .eq(&resolved_device.to_lowercase())
                {
                    // 该路径指向另一个设备，不适用。
                    continue;
                }
            } else {
                resolved_device = root.device.clone();
            }
        }

        if resolved_absolute {
            if !resolved_device.is_empty() {
                break;
            }
        } else {
            // 与 Node 相同：后处理的一轮（cwd）拼到已累积 tail 的**前面**，最后统一化简。
            resolved_tail = format!("{}\\{}", &input[root.root_end..], resolved_tail);
            resolved_absolute = root.is_absolute;
            if root.is_absolute && !resolved_device.is_empty() {
                break;
            }
        }
    }

    let tail = normalize_string(&resolved_tail, !resolved_absolute);
    let resolved = if resolved_absolute {
        format!("{}\\{}", resolved_device, tail)
    } else {
        format!("{}{}", resolved_device, tail)
    };
    if resolved.is_empty() {
        ".".to_string()
    } else {
        resolved
    }
}

/// `pathToFileURL(filename).href`。
///
/// Node 26 的实现：先按平台 `resolve`（规范化 `.`/`..`/重复分隔符）再交给 WHATWG URL 的
/// 路径集合做百分号编码。编码表实测为：保留 `a-zA-Z0-9` 与 `!$&'()*+,-./:;=@_`，
/// 其余字符（含空格、`#`、`?`、`%`、`~`、非 ASCII）按 UTF-8 字节 `%XX`（大写十六进制）。
pub fn path_to_file_url(path: &str) -> String {
    if is_windows_platform() {
        return windows_path_to_file_url(path);
    }
    let resolved = resolve_path(path);
    let mut url = String::from("file://");
    // Node 在 POSIX 上对解析结果不做额外处理：`/a/b` → `file:///a/b`（解析结果恒以 `/` 开头）。
    if !resolved.starts_with('/') {
        url.push('/');
    }
    url.push_str(&percent_encode_path(&resolved));
    url
}

/// Windows 分支：`\\server\share\...` → `file://server/share/...`，盘符/根相对 → `file:///C:/...`。
///
/// 逐条对齐 Node 26 的 `pathToFileURL`：
/// - `isUNC = filepath.startsWith("\\\\")`（**原始输入**的两个反斜杠）；是则 `resolved = filepath`
///   不经 `resolve`，否则 `resolved = path.win32.resolve(filepath)`；
/// - 之后 `isUNC || resolved.startsWith("\\\\")` 走 UNC 分支：扩展前缀 `\\?\UNC\` 占 8 个字符、
///   其它占 2 个；host 是下一个 `\` 之前的部分（小写、百分号解码；空 host 允许，`hostnameEndIndex == 2`
///   的空 servername 在 Node 里抛错）；**剩余整段交给 WHATWG URL 解析器**——反斜杠转斜杠、去掉 `.`/`..`
///   段、`..` 能一路弹到根（`\\server\share\a\..\..\b` → `file://server/b`）；
/// - 其余是普通路径分支：`resolve` 已规范化，再按 URL 路径集合做百分号编码。
fn windows_path_to_file_url(path: &str) -> String {
    // Node：`isUNC` 只看原始输入是否以**两个反斜杠**开头（`//server/share` 不算，先 resolve）。
    let is_unc = path.starts_with("\\\\");
    let resolved = if is_unc {
        path.to_string()
    } else {
        windows_resolve(path)
    };
    if is_unc || resolved.starts_with("\\\\") {
        return windows_unc_url(&resolved);
    }

    let mut resolved = resolved;
    // 真 Windows 上 `path.sep` 是 `\\`：原路径以分隔符结尾而解析结果不以 `\\` 结尾时补一个。
    if (path.ends_with('/') || path.ends_with('\\')) && !resolved.ends_with('\\') {
        resolved.push('\\');
    }
    let body = resolved.replace('\\', "/");
    // 根相对（`\\x` 解析成 `\\x`，无盘符）时 body 以 `/` 开头，`file://` + body 才是 `file:///x`。
    if body.starts_with('/') {
        format!("file://{}", percent_encode_path(&body))
    } else {
        format!("file:///{}", percent_encode_path(&body))
    }
}

/// Windows 版 [`path_to_file_url`]（供测试在任意平台直接验证）。
///
/// 生产代码里由 [`path_to_file_url`] 在 `cfg(windows)` 下调用；单独暴露是为了在 Linux CI 上
/// 也能对拍 Node 的 `pathToFileURL(p, { windows: true })`。
#[doc(hidden)]
pub fn path_to_file_url_windows(path: &str) -> String {
    windows_path_to_file_url(path)
}

/// UNC 分支（`resolved` 以 `\\\\` 开头）：拆出 host，剩余段按 WHATWG URL 路径解析。
///
/// Node 在这里把 `resolved.slice(hostnameEndIndex)` 当 URL 输入、把 host 当 base：
/// `new URL("\\share\\a\\..\\b", "server")` → `file://server/share/b`。因此 `\.\C:\...`
/// 的 host 是 `.`（保留），`\\?\C:\...` 的 host 是 `?`（URL 解析时被当作 query 分隔符丢弃，host 变空）。
fn windows_unc_url(resolved: &str) -> String {
    let extended_unc = resolved.starts_with("\\\\?\\UNC\\");
    let prefix_length = if extended_unc { 8 } else { 2 };
    let Some(hostname_end) = resolved[prefix_length..]
        .find('\\')
        .map(|i| prefix_length + i)
    else {
        // Node: ERR_INVALID_ARG_VALUE 'Missing UNC resource path'。这里退化为本地路径文本（绝不 panic）。
        return fallback_windows_url(resolved);
    };
    if hostname_end == 2 {
        // Node: ERR_INVALID_ARG_VALUE 'Empty UNC servername'。
        return fallback_windows_url(resolved);
    }
    // Node 用 `new URL(resolved.slice(hostnameEndIndex), hostname)`：hostname 串本身按 `file:` URL
    // 解析（`sérver/C:` 的 host 是 `sérver`、路径是 `/C:`），而输入以 `\\` 开头总是根相对引用，
    // 会把 base 的路径整段替换掉——所以这里只需要 host 部分（`?`/`#` 之后被 URL 丢弃）。
    let hostname = &resolved[prefix_length..hostname_end];
    let host_part = hostname.split(['/', '?', '#']).next().unwrap_or(hostname);
    let Some(host) = normalize_url_host(host_part) else {
        return fallback_windows_url(resolved);
    };
    let path = &resolved[hostname_end..];
    format!("file://{host}{}", encode_url_path_with_dot_segments(path))
}

/// Node 抛 `ERR_INVALID_*` 的退化输出：保留可读的 `file:///` 文本。
///
/// TS 侧这些输入会让 `pathToFileURL` 抛异常并经 `imageFallback` 冒出；Rust 的
/// [`image_fallback`] 冻结签名是 `-> String`（没有错误通道），故只能退化，见模块文档「已知偏差」。
fn fallback_windows_url(resolved: &str) -> String {
    format!(
        "file:///{}",
        percent_encode_path(resolved.replace('\\', "/").trim_start_matches('/'))
    )
}

/// WHATWG URL 的 host 解析（file 特殊 scheme）：用 [`url::Url`] 解析 `file://<hostname>/`。
///
/// 这一步覆盖 Node 的全部特殊规则：`?`/`#` 在 host 里是分隔符（`\\?\C:\x` 的 host 是 `?`，
/// 解析后为空），百分号转义解码（`%41` → `a`），IDNA/punycode（`sérver` → `xn--srver-bsa`），
/// IPv6 字面量保留（`[::1]`），大小写折叠。
///
/// 返回 `None` 表示 Node 会抛 `ERR_INVALID_URL`（空格、非法主机名等）——调用方退化。
fn normalize_url_host(hostname: &str) -> Option<String> {
    // URL 解析器在输入预处理阶段删除所有 ASCII tab/LF/CR（实测 `\\a\tb\share` 的 host 是 `ab`；
    // 空格与其它控制字符则留在 host 里按 forbidden code point 拒绝）。
    let cleaned: String = hostname
        .chars()
        .filter(|ch| !matches!(ch, '\t' | '\n' | '\r'))
        .collect();
    let hostname = cleaned.as_str();
    // `?`/`#` 在 host 里是分隔符：`\\?\C:\x` 的 host 是 `?`，解析后为空。
    let hostname = hostname.split(['?', '#']).next().unwrap_or(hostname);
    if hostname.is_empty() {
        return Some(String::new());
    }
    match url::Host::parse(hostname) {
        Ok(url::Host::Domain(domain)) => Some(domain),
        Ok(url::Host::Ipv4(ip)) => Some(ip.to_string()),
        Ok(url::Host::Ipv6(ip)) => Some(format!("[{ip}]")),
        Err(_) => {
            // `url` crate 对 ASCII 主机名做 punycode 校验（`xn--x` 被拒），而 WHATWG 的 host parser
            // 对纯 ASCII 输入只做小写化与 forbidden code point 检查（`xn--x` 原样保留，实测 Node 26）。
            let mut decoded = Vec::with_capacity(hostname.len());
            let bytes = hostname.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                if bytes[i] == b'%'
                    && i + 2 < bytes.len()
                    && let Ok(byte) = u8::from_str_radix(&hostname[i + 1..i + 3], 16)
                {
                    decoded.push(byte);
                    i += 3;
                    continue;
                }
                decoded.push(bytes[i]);
                i += 1;
            }
            if decoded
                .iter()
                .any(|byte| !byte.is_ascii() || !is_host_code_point(*byte))
            {
                return None;
            }
            Some(String::from_utf8(decoded).ok()?.to_ascii_lowercase())
        }
    }
}

/// WHATWG 的 forbidden domain code point（host parser 的拒绝集合）。
///
/// 含 C0 控制字符、DEL、空格、`?`、`@`、`:`、`[`、`\`、`%` 等（`%` 只会在百分号解码
/// 失败、原样留下时出现，Node 同样判为非法）。
fn is_host_code_point(byte: u8) -> bool {
    byte >= 0x20
        && byte != 0x7F
        && !matches!(
            byte,
            b' ' | b'#'
                | b'/'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | 0x5C
                | b']'
                | b'^'
                | b'|'
                | b'%'
        )
}

/// WHATWG 的 `is a Windows drive letter`（`C:` 形式，仅 ASCII 字母 + 冒号）。
fn is_windows_drive_letter(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// WHATWG URL 的 path state：切段、`.` 丢弃、`..` 弹段、末段带尾斜杠。
///
/// 与 JS 的 `new URL(path, host)` 一致：`\\` 视作 `/`、空段保留（`a//b` 保留双斜杠）、
/// `..` 弹掉上一段（可以一路弹到根），末尾的 `.`/`..` 产生尾斜杠。**唯一的保护**是 WHATWG
/// 的 `shorten a url's path`：路径只剩一段且该段是盘符（`C:`）时 `..` 不弹它（`/C:/..` → `/C:/`）。
fn encode_url_path_with_dot_segments(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut segments: Vec<&str> = normalized.split('/').collect();
    // 前导 `/` 由 split 产生的首个空串表示；这里去掉它，末尾自己补 `/`。
    if segments.first() == Some(&"") {
        segments.remove(0);
    }
    let mut out: Vec<&str> = Vec::with_capacity(segments.len());
    let count = segments.len();
    for (index, segment) in segments.iter().enumerate() {
        let is_last = index + 1 == count;
        match *segment {
            "." => {
                if is_last {
                    out.push("");
                }
            }
            ".." => {
                let protected = out.len() == 1 && is_windows_drive_letter(out[0]);
                if !protected {
                    out.pop();
                }
                if is_last {
                    out.push("");
                }
            }
            other => out.push(other),
        }
    }
    let mut encoded = String::new();
    for (index, segment) in out.iter().enumerate() {
        if index > 0 {
            encoded.push('/');
        }
        encoded.push_str(&percent_encode_path(segment));
    }
    format!("/{encoded}")
}

/// WHATWG URL 路径集合的百分号编码（保留字符以外一律 `%XX`）。
fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &byte in path.as_bytes() {
        if is_url_path_byte(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

/// WHATWG URL 路径集合的保留字符（实测 Node 26）。
fn is_url_path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b';'
                | b'='
                | b'@'
                | b'_'
        )
}

/// `path.posix.resolve(path)` 的等价物。
///
/// Node 的 `resolve` 会规范化 `.`、`..`、重复分隔符；相对路径基于 `process.cwd()` 解析。
/// [`path_to_file_url`] 另有一条**独立**的尾斜杠规则：原路径以分隔符结尾且解析结果不以分隔符
/// 结尾时补一个（所以 `/tmp/a/../` → `file:///tmp/`，而 `/tmp/a/..` → `file:///tmp`）。
fn resolve_path(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut segments: Vec<String> = Vec::new();
    // 相对路径先垫上 cwd 的段，再统一按绝对路径规则弹栈——这样 `..` 也能越过 cwd 边界。
    if !absolute && let Ok(cwd) = std::env::current_dir() {
        let cwd = cwd.to_string_lossy().into_owned();
        for segment in cwd.split('/') {
            if !segment.is_empty() && segment != "." {
                segments.push(segment.to_string());
            }
        }
    }
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other.to_string()),
        }
    }
    let mut out = String::from("/");
    out.push_str(&segments.join("/"));
    if path.ends_with('/') && !out.ends_with('/') {
        out.push('/');
    }
    out
}

// ---------------------------------------------------------------------------
// 能力探测
// ---------------------------------------------------------------------------

/// `probeTmuxHyperlinks()`：`tmux display-message -p '#{client_termfeatures}'`，250 ms 超时。
///
/// 任何错误（含超时、命令不存在、非零退出）均回退 `false`。超时是**硬截止**：子进程被 kill 后
/// 立即返回，不会等待仍被孙进程握着的 stdout 管道（见 `run_tmux_probe` 的注释）。
pub fn probe_tmux_hyperlinks() -> bool {
    match run_tmux_probe() {
        Some(stdout) => stdout.split(',').map(str::trim).any(|f| f == "hyperlinks"),
        None => false,
    }
}

fn run_tmux_probe() -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    const TIMEOUT: Duration = Duration::from_millis(250);
    let mut child = Command::new("tmux")
        .arg("display-message")
        .arg("-p")
        .arg("#{client_termfeatures}")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    // 读线程 + channel：`recv_timeout` 让「等 stdout EOF」也能被 250 ms 截止时间打断。
    // Node 的 `execSync(..., { timeout: 250 })` 由 `spawnSync` 实现，超时一到就 kill 子进程并
    // 抛 ETIMEDOUT——**不等**管道关闭；若后台孙进程仍握着继承来的 stdout（例如 `sh -c 'x &'`），
    // 直接 `join()` 会一直阻塞到孙进程退出（实测 30 s），把 `getCapabilities()` 首调用拖死。
    let mut stdout = child.stdout.take()?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = sender.send(buf);
    });

    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // 子进程已退出，但仍要等到 stdout EOF（或截止时间）才等于 Node 的行为。
                let remaining = deadline.saturating_duration_since(Instant::now());
                let Ok(bytes) = receiver.recv_timeout(remaining) else {
                    // 到点仍未 EOF：Node 在这里抛 ETIMEDOUT。读线程自行 detach。
                    return None;
                };
                if !status.success() {
                    return None;
                }
                return Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// `parseBooleanCapabilityOverride(value)`：`"1"` → `true`，`"0"` → `false`，其它 → `None`。
fn parse_boolean_capability_override(value: Option<&str>) -> Option<bool> {
    match value {
        Some("1") => Some(true),
        Some("0") => Some(false),
        _ => None,
    }
}

/// `detectCapabilitiesFromEnvironment(probe)`：分支顺序**不可变**（见 `spec/tui-core.md` §2.15.2）。
fn detect_capabilities_from_environment(probe: &mut dyn FnMut() -> bool) -> TerminalCapabilities {
    let term_program = env_var_lower("TERM_PROGRAM");
    let terminal_emulator = env_var_lower("TERMINAL_EMULATOR");
    let term = env_var_lower("TERM");
    let color_term = env_var_lower("COLORTERM");
    let has_true_color_hint =
        color_term == "truecolor" || color_term == "24bit" || term.ends_with("-direct");
    let is_windows_console = is_windows_platform();

    // 1. tmux：只在客户端确认转发时输出 OSC 8；图片协议在 tmux 下不可靠，保持 `images: null`。
    if env_var_present("TMUX") || term.starts_with("tmux") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: probe(),
        };
    }

    // 2. screen 不转发 OSC 8。
    if term.starts_with("screen") {
        return TerminalCapabilities {
            images: None,
            true_color: has_true_color_hint,
            hyperlinks: false,
        };
    }

    // 3. Herdr：转发 OSC 8 且支持 Kitty 图形协议。必须放在外层终端探测之前——
    //    `KITTY_WINDOW_ID` 可能泄漏进它的面板。
    if term_program == "herdr" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: has_true_color_hint,
            hyperlinks: true,
        };
    }

    // 4. Rex。
    if term_program == "rex" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    // 5. Kitty。
    if env_var_present("KITTY_WINDOW_ID") || term_program == "kitty" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    // 6. Ghostty。
    if term_program == "ghostty"
        || term.contains("ghostty")
        || env_var_present("GHOSTTY_RESOURCES_DIR")
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    // 7. WezTerm。
    if env_var_present("WEZTERM_PANE") || term_program == "wezterm" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    // 8. Warp。
    if term_program == "warpterminal"
        || env_var_present("WARP_SESSION_ID")
        || env_var_present("WARP_TERMINAL_SESSION_UUID")
    {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    // 9. iTerm2。
    if env_var_present("ITERM_SESSION_ID") || term_program == "iterm.app" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        };
    }

    // 10. Windows Terminal。
    if env_var_present("WT_SESSION") {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    // 11. Alacritty / VSCode / Zed。
    if matches!(term_program.as_str(), "alacritty" | "vscode" | "zed") {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    // 12. JetBrains。
    if terminal_emulator == "jetbrains-jediterm" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    // 13. 原生 Windows 控制台。
    if is_windows_console {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: false,
        };
    }

    // 14. 未知终端：保守处理（OSC 8 在不支持的终端上会吞掉 URL）。
    TerminalCapabilities {
        images: None,
        true_color: has_true_color_hint,
        hyperlinks: false,
    }
}

/// `detectCapabilities(tmuxForwardsHyperlink = probeTmuxHyperlinks)`。
///
/// 先按环境做终端探测（`detect_capabilities_from_environment` 的分支顺序固定），再叠加覆盖：
/// - `KIANA_HYPERLINKS`：`"1"`/`"0"` 强制超链接能力，并且**跳过**传入的 tmux 探针；
/// - `KIANA_IMAGE_PROTOCOL`：`"kitty"`/`"iterm2"` 强制协议，`"none"`/`"0"` 强制关闭图片；
/// - `KIANA_TRUE_COLOR`：`"1"`/`"0"` 强制真彩。
///
/// 覆盖只影响本次返回值，不写缓存；缓存由 [`get_capabilities`] 负责。
pub fn detect_capabilities(
    tmux_forwards_hyperlink: &mut dyn FnMut() -> bool,
) -> TerminalCapabilities {
    let hyperlinks_override =
        parse_boolean_capability_override(env_var("KIANA_HYPERLINKS").as_deref());

    let mut detected = match hyperlinks_override {
        Some(value) => detect_capabilities_from_environment(&mut || value),
        None => detect_capabilities_from_environment(tmux_forwards_hyperlink),
    };

    let image_protocol = env_var("KIANA_IMAGE_PROTOCOL").map(|v| v.to_lowercase());
    let images_override: Option<Option<ImageProtocol>> = match image_protocol.as_deref() {
        Some("kitty") => Some(Some(ImageProtocol::Kitty)),
        Some("iterm2") => Some(Some(ImageProtocol::Iterm2)),
        Some("none") | Some("0") => Some(None),
        _ => None,
    };
    let true_color_override =
        parse_boolean_capability_override(env_var("KIANA_TRUE_COLOR").as_deref());

    if let Some(images) = images_override {
        detected.images = images;
    }
    if let Some(true_color) = true_color_override {
        detected.true_color = true_color;
    }
    if let Some(hyperlinks) = hyperlinks_override {
        detected.hyperlinks = hyperlinks;
    }
    detected
}

/// `detectCapabilities()`：用默认的 tmux 探针（`probeTmuxHyperlinks`）。
pub fn detect_capabilities_default() -> TerminalCapabilities {
    detect_capabilities(&mut probe_tmux_hyperlinks)
}

/// `getCapabilities()`。
///
/// 首次调用（或缓存被清后）按 [`CapabilityOverrides`] 的 `hyperlinks` 决定是否使用 tmux 探针，
/// 再执行 [`detect_capabilities`]，最后逐字段叠加覆盖，把结果写入线程本地缓存；后续调用直接
/// 返回缓存。因此环境变量变化不会自动生效，测试/宿主必须调用 [`reset_capabilities_cache`]。
pub fn get_capabilities() -> TerminalCapabilities {
    CACHED_CAPABILITIES.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(cached) = *cache {
            // 缓存命中：不再读环境，也不再执行 tmux 探针。
            return cached;
        }
        let overrides = CAPABILITY_OVERRIDES.with(|o| *o.borrow());
        // 有超链接覆盖时用常量闭包代替 tmux 探针：探测阶段不会执行外部命令。
        let mut detected = match overrides.hyperlinks {
            Some(value) => detect_capabilities(&mut || value),
            None => detect_capabilities_default(),
        };
        // 覆盖项在探测结果之上逐字段生效（与 TS 的对象展开顺序一致）。
        if let Some(images) = overrides.images {
            detected.images = images;
        }
        if let Some(true_color) = overrides.true_color {
            detected.true_color = true_color;
        }
        if let Some(hyperlinks) = overrides.hyperlinks {
            detected.hyperlinks = hyperlinks;
        }
        *cache = Some(detected);
        detected
    })
}

/// `getTerminalColorMode(capabilities)`。
pub fn get_terminal_color_mode(capabilities: TerminalCapabilities) -> TerminalColorMode {
    if capabilities.true_color {
        TerminalColorMode::Truecolor
    } else {
        TerminalColorMode::Palette256
    }
}

/// `getTerminalColorMode()` 的零参数形态：TS 的默认参数 `capabilities = getCapabilities()`。
///
/// TS 生产调用方（`coding-agent/src/modes/interactive/theme/theme.ts` 的 `createTheme` /
/// `createSystemTheme`）用的就是这个形态——从缓存的能力推导颜色模式。
pub fn get_terminal_color_mode_default() -> TerminalColorMode {
    get_terminal_color_mode(get_capabilities())
}

/// `resetCapabilitiesCache()`：只清能力缓存。
///
/// 不重置 [`CapabilityOverrides`]、单元格尺寸、Kitty 元数据表，也不重置
/// [`get_max_kitty_image_rows`] 的单调值（测试 `max_kitty_image_rows_is_monotonic` 钉死了后者）。
pub fn reset_capabilities_cache() {
    CACHED_CAPABILITIES.with(|cache| *cache.borrow_mut() = None);
}

/// `setCapabilityOverrides(overrides)`：与当前值相同则直接返回（不清缓存）。
///
/// 值有变化时替换覆盖集并清能力缓存，使下一次 [`get_capabilities`] 重新探测。
pub fn set_capability_overrides(overrides: CapabilityOverrides) {
    CAPABILITY_OVERRIDES.with(|current| {
        let mut current = current.borrow_mut();
        if *current == overrides {
            return;
        }
        *current = overrides;
        CACHED_CAPABILITIES.with(|cache| *cache.borrow_mut() = None);
    });
}

/// `setCapabilities(caps)`：绕过探测直接写缓存。
///
/// 测试用它固定能力；`alt_screen` 在进入 iTerm2 备用屏时也临时把 `images` 置为 `None`
/// （保留其它字段），退出时再恢复。
pub fn set_capabilities(caps: TerminalCapabilities) {
    CACHED_CAPABILITIES.with(|cache| *cache.borrow_mut() = Some(caps));
}

/// `getCellDimensions()`。
pub fn get_cell_dimensions() -> CellDimensions {
    CELL_DIMENSIONS.with(|dims| *dims.borrow())
}

/// `setCellDimensions(dims)`。
pub fn set_cell_dimensions(dims: CellDimensions) {
    CELL_DIMENSIONS.with(|cell| *cell.borrow_mut() = dims);
}

// ---------------------------------------------------------------------------
// 图片行与 ID
// ---------------------------------------------------------------------------

/// JS 模板字面量里的数字文本（`String(number)`）。
///
/// 与 [`format_js_number`]（JSON 语义，`NaN` → `"null"`）只差非有限值：JS 的模板字面量
/// 写 `"NaN"` / `"Infinity"` / `"-Infinity"`。本模块所有数字入串处都是模板字面量。
fn js_number_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_string()
    } else if value == f64::INFINITY {
        "Infinity".to_string()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else {
        format_js_number(value)
    }
}

/// `isImageLine(line)`：先查行首（单行图片的快路径），再查任意位置（多行图片带 cursor-up 前缀）。
///
/// 注意 `includes()` 而不是只看行首——回归测试
/// `bug-regression-isimageline-startswith-bug.test.ts` 钉的就是这条。
pub fn is_image_line(line: &str) -> bool {
    line.starts_with(KITTY_PREFIX)
        || line.starts_with(ITERM2_PREFIX)
        || line.contains(KITTY_PREFIX)
        || line.contains(ITERM2_PREFIX)
}

/// `allocateImageId()`：`floor(Math.random() * 0xfffffffe) + 1`，值域 `[1, 0xfffffffe]`。
///
/// 用 OS 熵（`getrandom`）而不是 `rand` 线程 RNG，避免引入新的 workspace pin；分布与
/// `Math.random()` 一致（均匀 32 位）。
pub fn allocate_image_id() -> u64 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        // 熵源不可用时退化为时间戳（JS 侧不会发生，但这里不能 panic）。
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
            .unwrap_or(0);
        return (u64::from(nanos) % 0xffff_fffe) + 1;
    }
    // `Math.random()` ∈ [0, 1)；用 32 位整数近似 `floor(random * 0xfffffffe)`。
    let scaled = (u64::from(u32::from_le_bytes(bytes)) * 0xffff_fffeu64) >> 32;
    scaled + 1
}

// ---------------------------------------------------------------------------
// Kitty / iTerm2 编码
// ---------------------------------------------------------------------------

const KITTY_CHUNK_SIZE: usize = 4096;

/// `encodeKitty` 的选项。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct KittyEncodeOptions {
    /// `c=` 列数；`0`/`NaN` 视为未提供（JS 真值判定）。
    pub columns: Option<f64>,
    /// `r=` 行数；`0`/`NaN` 视为未提供。
    pub rows: Option<f64>,
    /// `i=` 图片 ID；`0`/`NaN` 视为未提供（因此 `Some(0)` 不会写出 `i=`）。
    pub image_id: Option<f64>,
    /// `Some(false)` 时追加 `C=1`（禁止 Kitty 自行移动光标）；其它值等同于 TS 的 `undefined`。
    pub move_cursor: Option<bool>,
}

/// `encodeKitty(base64Data, options)`。
///
/// 参数以 `a=T,f=100,q=2` 开头；`move_cursor: Some(false)` 时插入 `C=1`；`columns`/`rows`/
/// `image_id` 经 JS 真值过滤（`0`、`NaN` 不写出）。分块切的是 **base64 字符串**
/// （每块 4096 个 UTF-16 code unit），不是原始字节。
pub fn encode_kitty(base64_data: &str, options: KittyEncodeOptions) -> String {
    let mut params: Vec<String> = vec!["a=T".into(), "f=100".into(), "q=2".into()];
    if options.move_cursor == Some(false) {
        params.push("C=1".into());
    }
    if let Some(columns) = options.columns.filter(|v| is_truthy_number(*v)) {
        params.push(format!("c={}", js_number_text(columns)));
    }
    if let Some(rows) = options.rows.filter(|v| is_truthy_number(*v)) {
        params.push(format!("r={}", js_number_text(rows)));
    }
    if let Some(image_id) = options.image_id.filter(|v| is_truthy_number(*v)) {
        params.push(format!("i={}", js_number_text(image_id)));
    }

    // TS 的 `base64Data.length` 按 **UTF-16 code unit** 计（`ARCHITECTURE.md` §4.6）。
    let length = base64_data.encode_utf16().count();
    if length <= KITTY_CHUNK_SIZE {
        return format!("\x1b_G{};{}\x1b\\", params.join(","), base64_data);
    }

    // 一次线性扫描完成切块（`slice(offset, offset + CHUNK_SIZE)` 是 O(1) 索引；旧实现每块都从
    // 串首重扫 UTF-16 下标，大图编码退化成 O(n²)，会卡住渲染线程）。
    let params = params.join(",");
    let mut out = String::with_capacity(base64_data.len() + (length / KITTY_CHUNK_SIZE + 1) * 28);
    let mut current = String::with_capacity(KITTY_CHUNK_SIZE);
    let mut current_units = 0usize;
    let mut emitted_units = 0usize;
    let mut is_first = true;
    for ch in base64_data.chars() {
        let units = ch.len_utf16();
        if current_units + units > KITTY_CHUNK_SIZE {
            // 切点落在代理对中间：JS 的 `slice` 把高代理留在本块末尾、低代理放到下一块开头
            // （两块各含一个孤立代理）。Rust 的 `String` 不能存孤立代理，写成 U+FFFD —— 这正是
            // Node 把该字符串按 UTF-8 写给终端时的字节（`EF BF BD`）。
            current.push('\u{FFFD}');
            emitted_units += 1;
            push_kitty_chunk(&mut out, &params, &mut current, &mut is_first, false);
            current.push('\u{FFFD}');
            current_units = 1;
            emitted_units += 1;
            continue;
        }
        current.push(ch);
        current_units += units;
        emitted_units += units;
        if current_units == KITTY_CHUNK_SIZE {
            let is_last = emitted_units >= length;
            push_kitty_chunk(&mut out, &params, &mut current, &mut is_first, is_last);
            current_units = 0;
        }
    }
    if current_units > 0 {
        push_kitty_chunk(&mut out, &params, &mut current, &mut is_first, true);
    }
    out
}

/// 追加一块 Kitty 负载：首块带完整参数 + `m=1`，末块 `m=0`，中间块 `m=1`。
fn push_kitty_chunk(
    out: &mut String,
    params: &str,
    chunk: &mut String,
    is_first: &mut bool,
    is_last: bool,
) {
    out.push_str("\x1b_G");
    if *is_first {
        out.push_str(params);
        out.push_str(",m=1;");
        *is_first = false;
    } else if is_last {
        out.push_str("m=0;");
    } else {
        out.push_str("m=1;");
    }
    out.push_str(chunk);
    out.push_str("\x1b\\");
    chunk.clear();
}

/// JS `str.length`（UTF-16 code unit 数）。
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// JS 的真值判定：`0`、`NaN`、`-0` 为假。
fn is_truthy_number(value: f64) -> bool {
    value != 0.0 && !value.is_nan()
}

/// `deleteKittyImage(imageId)`：大写 `I`，连同图片数据一起释放。
pub fn delete_kitty_image(image_id: u64) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

/// `deleteAllKittyImages()`：大写 `A`，连同图片数据一起释放。
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

/// `deleteAllKittyPlacements()`：小写 `a`，只删可见 placement，保留已上传数据。
pub fn delete_all_kitty_placements() -> String {
    "\x1b_Ga=d,d=a,q=2\x1b\\".to_string()
}

/// `encodeITerm2` 的选项。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Iterm2EncodeOptions {
    /// `width=` 参数（数字或 `"auto"`/`"50%"` 之类的字符串）。
    pub width: Option<Iterm2Size>,
    /// `height=` 参数。
    pub height: Option<Iterm2Size>,
    /// `name=` 参数：以 UTF-8 字节做 base64 后写出；空串视为未提供。
    pub name: Option<String>,
    /// `Some(false)` 时追加 `preserveAspectRatio=0`；其它值不写该参数（保持比例）。
    pub preserve_aspect_ratio: Option<bool>,
    /// `inline=` 参数：`Some(false)` 写 `0`，其它值写 `1`。
    pub inline: Option<bool>,
}

/// iTerm2 的 width/height 参数：数字或字符串（`"auto"`、`"50%"` 等）。
#[derive(Debug, Clone, PartialEq)]
pub enum Iterm2Size {
    /// 数字参数，输出时按 JS 的数字文本规则（`NaN`/`Infinity` 也有对应写法）。
    Number(f64),
    /// 原样输出的字符串参数，如 `"auto"`、`"50%"`。
    Text(String),
}

impl std::fmt::Display for Iterm2Size {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Iterm2Size::Number(v) => f.write_str(&js_number_text(*v)),
            Iterm2Size::Text(v) => f.write_str(v),
        }
    }
}

impl From<f64> for Iterm2Size {
    fn from(value: f64) -> Self {
        Iterm2Size::Number(value)
    }
}

impl From<&str> for Iterm2Size {
    fn from(value: &str) -> Self {
        Iterm2Size::Text(value.to_string())
    }
}

/// `encodeITerm2(base64Data, options)`。`size=` 是**解码后**的字节数。
///
/// 参数顺序固定为 `inline`、`size`，之后按 `width`、`height`、`name`、`preserveAspectRatio`
/// 追加；`name` 以 UTF-8 字节做 base64；`inline` 缺省为 1，`preserveAspectRatio=0` 只在
/// `Some(false)` 时出现。
pub fn encode_iterm2(base64_data: &str, options: Iterm2EncodeOptions) -> String {
    let inline = options.inline != Some(false);
    let mut params: Vec<String> = vec![
        format!("inline={}", if inline { 1 } else { 0 }),
        format!("size={}", base64_decoded_byte_length(base64_data)),
    ];

    if let Some(width) = &options.width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = &options.height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = options.name.as_ref().filter(|name| !name.is_empty()) {
        params.push(format!("name={}", base64_encode(name.as_bytes())));
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".into());
    }

    format!("\x1b]1337;File={}:{}\x07", params.join(";"), base64_data)
}

/// `Buffer.byteLength(base64Data, "base64")`。
///
/// Node 的实现（`internal/buffer` 的 `base64ByteLength`）：先按**UTF-16 长度**计数，末尾最多
/// 剥掉两个 `=`，再 `(bytes * 3) >>> 2`。它**不做合法性校验**——`"AA!A"` 给出 3，而
/// `Buffer.from` 解码出的长度是 2；二者不同，测试逐一钉死。
fn base64_decoded_byte_length(data: &str) -> usize {
    let units: Vec<u16> = data.encode_utf16().collect();
    let mut count = units.len();
    if count > 0 && units[count - 1] == u16::from(b'=') {
        count -= 1;
    }
    if count > 1 && units[count - 1] == u16::from(b'=') {
        count -= 1;
    }
    (count * 3) >> 2
}

/// 标准 base64 编码（`Buffer.from(str).toString("base64")`）。
fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `Buffer.from(base64Data, "base64")`：Node 的宽松解码。
///
/// 规则（实测 Node 26）：把字符串按 **UTF-16 code unit** 逐个取**低 8 位**
/// （V8 的 base64 解码器吃的是 `charCodeAt(i)`，非 ASCII 字符按 `& 0xff` 掩码后参与判定，
/// 例如 `"AAéA"` 与 `"AA*A"` 同解；已对 U+0000..U+2FFF 全量对拍 Node 验证）；
/// 跳过非法字符；`-`/`_` 视作 `+`/`/`；遇到掩码后的 `=` **停止**；
/// 剩余不足 4 个字符的尾组里，2/3 个字符各解出 1/2 字节，1 个字符丢弃。
pub fn base64_decode(data: &str) -> Vec<u8> {
    let mut cleaned = String::with_capacity(data.len());
    for unit in data.encode_utf16() {
        let byte = (unit & 0xff) as u8;
        if byte == b'=' {
            break;
        }
        match byte {
            b'-' => cleaned.push('+'),
            b'_' => cleaned.push('/'),
            b if b.is_ascii_alphanumeric() || b == b'+' || b == b'/' => cleaned.push(b as char),
            _ => {}
        }
    }

    // V8 的解码器**不校验**尾组里多余的低位（`QUJ` → `41 42`、`QUJDAB` → `41 42 43 00`），
    // 因此这里手写解码，不能用 `base64` crate 的严格引擎（它会以 `InvalidLastSymbol` 拒绝）。
    let bytes = cleaned.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let (chunks, remainder) = bytes.as_chunks::<4>();
    for chunk in chunks {
        let (a, b, c, d) = (
            base64_value(chunk[0]),
            base64_value(chunk[1]),
            base64_value(chunk[2]),
            base64_value(chunk[3]),
        );
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (c >> 2));
        out.push((c << 6) | d);
    }
    match remainder {
        [a, b] => out.push((base64_value(*a) << 2) | (base64_value(*b) >> 4)),
        [a, b, c] => {
            out.push((base64_value(*a) << 2) | (base64_value(*b) >> 4));
            out.push((base64_value(*b) << 4) | (base64_value(*c) >> 2));
        }
        // 单个残余字符不足一个字节，丢弃（与 Node 一致）。
        _ => {}
    }
    out
}

/// base64 字符 → 6 位值（输入已过滤，只会是字母表内的 ASCII 字符）。
const fn base64_value(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        _ => 63, // b'/'
    }
}

// ---------------------------------------------------------------------------
// Kitty 元数据注册表
// ---------------------------------------------------------------------------

/// 注册表容量上限（TS：`if (kittyImageMetadata.size > 1000)` 时删最旧一项）。
const MAX_REGISTERED_KITTY_IMAGES: usize = 1000;

/// `registerKittyImageMetadata(metadata)`：先删同 ID 再插入（刷新插入序），超过 1000 项时淘汰最旧。
///
/// 每次注册都会让线程本地的 transmission generation 加一并记入该项；`alt_screen` 用它与缓存的
/// 旧值比较，判断同一图片 ID 是否被重新传输过。
pub fn register_kitty_image_metadata(metadata: KittyImageMetadata) {
    let generation = KITTY_TRANSMISSION_GENERATION.with(|g| {
        let next = g.get() + 1;
        g.set(next);
        next
    });
    MAX_KITTY_IMAGE_ROWS.with(|m| m.set(m.get().max(metadata.rows)));
    KITTY_IMAGE_METADATA.with(|map| {
        let mut map = map.borrow_mut();
        // 先删再插：同 ID 重传会移到 IndexMap 插入序末尾，淘汰时不会先删刚用过的项。
        map.shift_remove(&metadata.image_id);
        map.insert(
            metadata.image_id,
            RegisteredKittyImageMetadata {
                image_id: metadata.image_id,
                columns: metadata.columns,
                rows: metadata.rows,
                width_px: metadata.width_px,
                height_px: metadata.height_px,
                transmission_generation: generation,
            },
        );
        if map.len() > MAX_REGISTERED_KITTY_IMAGES
            && let Some(oldest) = map.keys().next().copied()
        {
            map.shift_remove(&oldest);
        }
    });
}

/// `getMaxKittyImageRows()`：已注册图片中最高的行数，**单调不减**（`resetCapabilitiesCache` 不重置）。
pub fn get_max_kitty_image_rows() -> usize {
    MAX_KITTY_IMAGE_ROWS.with(|m| m.get())
}

fn get_registered_kitty_image_metadata_from_controls(
    controls: &str,
) -> Option<RegisteredKittyImageMetadata> {
    let image_id: u64 = control_param(controls, 'i')?.parse().ok()?;
    KITTY_IMAGE_METADATA.with(|map| map.borrow().get(&image_id).copied())
}

fn get_registered_kitty_image_metadata(line: &str) -> Option<RegisteredKittyImageMetadata> {
    let controls = kitty_controls(line)?;
    get_registered_kitty_image_metadata_from_controls(controls)
}

/// 读取 `\x1b_G([^;]*);` 的第一捕获组（控件串）。`find` 语义与 TS 的 `exec` 一致（第一个匹配）。
fn kitty_controls(line: &str) -> Option<&str> {
    let start = line.find(KITTY_PREFIX)? + KITTY_PREFIX.len();
    let rest = &line[start..];
    let end = rest.find(';')?;
    Some(&rest[..end])
}

/// 读取控件串中某个整数参数（`(?:^|,)<key>=(\d+)(?:,|$)`）。返回的串保证只含 ASCII 数字。
fn control_param(controls: &str, key: char) -> Option<&str> {
    let mut search_from = 0usize;
    while let Some(rel) = controls[search_from..].find(key) {
        let at = search_from + rel;
        // 键前必须是串首或 `,`
        let before_ok = at == 0 || controls.as_bytes()[at - 1] == b',';
        if before_ok && controls.as_bytes().get(at + 1) == Some(&b'=') {
            let digits_start = at + 2;
            let mut digits_end = digits_start;
            while controls
                .as_bytes()
                .get(digits_end)
                .is_some_and(u8::is_ascii_digit)
            {
                digits_end += 1;
            }
            let after = controls.as_bytes().get(digits_end);
            let after_ok = matches!(after, None | Some(b','));
            if digits_end > digits_start && after_ok {
                return Some(&controls[digits_start..digits_end]);
            }
        }
        search_from = at + key.len_utf8();
    }
    None
}

fn get_explicit_kitty_image_rows(controls: &str) -> Option<usize> {
    // TS：`Number.parseInt(value, 10)`。数字串按 JS 双精度取整（超过 2^53 会舍入），
    // 不能用 `usize::parse`——那会在超过 u64::MAX 时解析失败、错误地回退到注册表里的 rows。
    let value: f64 = control_param(controls, 'r')?.parse().ok()?;
    // `rows > 0 ? rows : undefined`；`f64 as usize` 对超范围值饱和（Rust 的 saturating cast）。
    if value > 0.0 {
        Some(value as usize)
    } else {
        None
    }
}

fn get_kitty_image_rows_from_controls(controls: &str, fallback_rows: usize) -> usize {
    get_explicit_kitty_image_rows(controls).unwrap_or(fallback_rows)
}

/// `getKittyImageMetadata(line)`：公共副本（不含 transmission generation）。
///
/// 行内需有 `\x1b_G`、控件串含 `i=<id>`，且该 ID 已用 [`register_kitty_image_metadata`] 注册；
/// 任一条件不满足都返回 `None`（iTerm2 图片行不参与这套注册表）。
pub fn get_kitty_image_metadata(line: &str) -> Option<KittyImageMetadata> {
    let metadata = get_registered_kitty_image_metadata(line)?;
    Some(KittyImageMetadata {
        image_id: metadata.image_id,
        columns: metadata.columns,
        rows: metadata.rows,
        width_px: metadata.width_px,
        height_px: metadata.height_px,
    })
}

/// `getKittyImagePlacementRows(line)`：不扫描负载的 placement 行数。
///
/// 控件串里的 `r=` 为正数时优先（十进制解析）；否则回退到注册表的 `rows`；
/// 两者都没有时返回 `None`。
pub fn get_kitty_image_placement_rows(line: &str) -> Option<usize> {
    let controls = kitty_controls(line)?;
    if let Some(explicit) = get_explicit_kitty_image_rows(controls) {
        return Some(explicit);
    }
    get_registered_kitty_image_metadata_from_controls(controls).map(|m| m.rows)
}

/// TS 的 `KITTY_PLACEMENT_CONTROL_KEYS`（17 个）；不在其中（含无 `=` 的项）一律丢弃。
fn is_placement_control_key(control: &str) -> bool {
    // `control.split("=", 1)[0] ?? ""`——只取第一个 `=` 之前的部分。
    let key = control.split('=').next().unwrap_or("");
    matches!(
        key,
        "i" | "p"
            | "x"
            | "y"
            | "w"
            | "h"
            | "X"
            | "Y"
            | "c"
            | "r"
            | "C"
            | "U"
            | "z"
            | "P"
            | "Q"
            | "H"
            | "V"
    )
}

/// 控件串里是否含 `(?:^|,)m=1(?:,|$)`。
fn has_more_chunks_flag(controls: &str) -> bool {
    let mut search_from = 0usize;
    while let Some(rel) = controls[search_from..].find("m=1") {
        let at = search_from + rel;
        let before_ok = at == 0 || controls.as_bytes()[at - 1] == b',';
        let after = controls.as_bytes().get(at + 3);
        let after_ok = matches!(after, None | Some(b','));
        if before_ok && after_ok {
            return true;
        }
        search_from = at + 3;
    }
    false
}

/// `getKittyImagePlacement(line)`：为 `renderImage` 产出的图片行构造只放置命令。
///
/// 流程：
/// 1. 找行内第一个 `\x1b_G`，读控件串里的 `i=` 并查注册表（未注册直接 `None`）；
/// 2. 从这条命令起，只要控件串带 `m=1` 就跳到下一个 `\x1b\\` 之后继续找下一块，直到
///    `m≠1`；下一块不以 `\x1b_G` 开头或找不到终止符都返回 `None`；
/// 3. 从**首个**控件串里挑出属于 17 个放置相关键的控件，拼成 `\x1b_Ga=p,q=2,...\x1b\\`；
/// 4. 用它与原行前后缀拼出 `replacement_line`。
///
/// 注册表命中后仍可能返回 `None`（缺 `\x1b\\`、分块断裂）；这些情况调用方应保留原行。
pub fn get_kitty_image_placement(line: &str) -> Option<KittyImagePlacement> {
    // 第一块：定位 `\x1b_G`，解析控件串并确认该图片 ID 已注册。
    let match_index = line.find(KITTY_PREFIX)?;
    let match_controls = kitty_controls(&line[match_index..])?;
    let metadata = get_registered_kitty_image_metadata_from_controls(match_controls)?;

    // 向后走完所有 `m=1` 分块，找到整段传输的结束位置（transmission_end）。
    let mut command_start = match_index;
    let mut command_controls = match_controls;
    let mut transmission_end: usize;
    loop {
        let terminator = line[command_start + KITTY_PREFIX.len()..].find("\x1b\\")?;
        let terminator = command_start + KITTY_PREFIX.len() + terminator;
        transmission_end = terminator + 2;
        if !has_more_chunks_flag(command_controls) {
            break;
        }
        command_start = transmission_end;
        if !line[command_start..].starts_with(KITTY_PREFIX) {
            return None;
        }
        let rest = &line[command_start + KITTY_PREFIX.len()..];
        let controls_end = rest.find(';')?;
        command_controls = &rest[..controls_end];
    }

    // 从首块控件串里只挑放置相关键；负载与传输参数（`a`、`f`、`m` 等）不复制。
    let controls: Vec<&str> = match_controls
        .split(',')
        .filter(|control| is_placement_control_key(control))
        .collect();
    let sequence = format!("\x1b_Ga=p,q=2,{}\x1b\\", controls.join(","));
    // 用 placement 命令原位替换传输段，保留行内前后的普通文本。
    let mut replacement_line = String::with_capacity(line.len());
    replacement_line.push_str(&line[..match_index]);
    replacement_line.push_str(&sequence);
    replacement_line.push_str(&line[transmission_end..]);

    Some(KittyImagePlacement {
        image_id: metadata.image_id,
        transmission_generation: metadata.transmission_generation,
        // TS 的字符串索引按 UTF-16 code unit（`ARCHITECTURE.md` §4.6）；终端列宽/字节配额
        // 也按 code unit 计，因此这里不用字节长度。
        transmission_bytes: utf16_len(&line[match_index..transmission_end]),
        estimated_decoded_bytes: metadata.width_px * metadata.height_px * 4.0,
        rows: get_kitty_image_rows_from_controls(match_controls, metadata.rows),
        sequence,
        replacement_line,
    })
}

/// `cropKittyImageLine(line, hiddenRows, visibleRows)`：按行裁剪（y/h/r 重新计算）。
///
/// 只处理已注册的 Kitty 图片。`hiddenRows` 是图片顶部被卷掉的行数，`visibleRows` 是当前可见
/// 行数（超过剩余行数时按剩余行数截断）。函数把行窗口换算成源图像素坐标：`y` 是被隐藏部分
/// 的像素高度（向下取整），`h` 是可见窗口的像素高度（向上取整、至少 1 像素、不超过原图），
/// `r` 直接写实际可见行数。
///
/// 以下情况逐字返回输入行：找不到元数据/`\x1b_G`/控件串；`hiddenRows < 0`、
/// `hiddenRows >= rows` 或 `visibleRows <= 0`；以及无需裁剪（隐藏 0 行且全部可见）。
/// 控件串之外的文本（行首尾普通字符、分块负载与后续命令）原样保留。
pub fn crop_kitty_image_line(line: &str, hidden_rows: isize, visible_rows: isize) -> String {
    let Some(metadata) = get_kitty_image_metadata(line) else {
        return line.to_string();
    };
    let Some(match_index) = line.find(KITTY_PREFIX) else {
        return line.to_string();
    };
    let Some(controls_len) = kitty_controls(&line[match_index..]).map(str::len) else {
        return line.to_string();
    };
    // 裁剪窗口无效（负数、越过图片底部、0 行可见）时原样返回，由调用方决定后续处理。
    if hidden_rows < 0 || hidden_rows >= metadata.rows as isize || visible_rows <= 0 {
        return line.to_string();
    }
    let hidden = hidden_rows as usize;
    let cropped_rows = (visible_rows as usize).min(metadata.rows - hidden);
    if hidden == 0 && cropped_rows == metadata.rows {
        return line.to_string();
    }

    // `y`/`h` 是源图像素坐标：按 `height_px / rows` 把行窗口换算过去，`h` 至少 1 像素。
    let height_px = metadata.height_px;
    let rows = metadata.rows as f64;
    let source_y = js_floor((height_px * hidden as f64) / rows);
    let source_end = js_ceil((height_px * (hidden + cropped_rows) as f64) / rows);
    let source_height = js_max(1.0, js_min(height_px, source_end) - source_y);
    let source_y_text = js_number_text(source_y);
    let source_height_text = js_number_text(source_height);

    // 先移除原有的 y/h/r，再追加新值；其它控件保持原顺序。
    let controls_start = match_index + KITTY_PREFIX.len();
    let controls = &line[controls_start..controls_start + controls_len];
    let mut kept: Vec<&str> = controls
        .split(',')
        .filter(|control| !is_yhr_control(control))
        .collect();
    let y_control = format!("y={source_y_text}");
    let h_control = format!("h={source_height_text}");
    let r_control = format!("r={cropped_rows}");
    kept.push(&y_control);
    kept.push(&h_control);
    kept.push(&r_control);

    let mut out = String::with_capacity(line.len() + 32);
    out.push_str(&line[..match_index]);
    out.push_str(KITTY_PREFIX);
    out.push_str(&kept.join(","));
    out.push(';');
    out.push_str(&line[controls_start + controls_len + 1..]);
    out
}

/// TS `controls.filter((control) => !/^[yhr]=/.test(control))`。
fn is_yhr_control(control: &str) -> bool {
    let bytes = control.as_bytes();
    bytes.len() >= 2 && matches!(bytes[0], b'y' | b'h' | b'r') && bytes[1] == b'='
}

/// JS `Math.floor`。
fn js_floor(value: f64) -> f64 {
    if value.is_nan() || value.is_infinite() {
        value
    } else {
        value.floor()
    }
}

/// JS `Math.ceil`。
fn js_ceil(value: f64) -> f64 {
    if value.is_nan() || value.is_infinite() {
        value
    } else {
        value.ceil()
    }
}

/// JS `Math.min`（任一为 `NaN` 则结果 `NaN`）。
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a < b {
        a
    } else {
        b
    }
}

/// JS `Math.max`（任一为 `NaN` 则结果 `NaN`）。
fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else if a > b {
        a
    } else {
        b
    }
}

// ---------------------------------------------------------------------------
// 行列换算
// ---------------------------------------------------------------------------

/// TS `chooseLessDistortedCellCount`：在 `upperCount` 与 `upperCount - 1` 之间挑畸变更小的。
fn choose_less_distorted_cell_count(upper_count: usize, ideal_count: f64) -> usize {
    if upper_count <= 1 {
        return upper_count;
    }
    let lower_count = upper_count - 1;
    let upper = upper_count as f64;
    let lower = lower_count as f64;
    let upper_distortion = js_max(upper / ideal_count, ideal_count / upper);
    let lower_distortion = js_max(lower / ideal_count, ideal_count / lower);
    if lower_distortion < upper_distortion {
        lower_count
    } else {
        upper_count
    }
}

/// `calculateImageCellSize(imageDimensions, maxWidthCells, maxHeightCells?, cellDimensions, optimizeAspectRatio?)`。
///
/// TS 的默认参数：`cellDimensions = { widthPx: 9, heightPx: 18 }`、`optimizeAspectRatio = false`
/// （全默认形态见 [`calculate_image_cell_size_default`]）。注意默认单元格尺寸是**静态 9x18**，
/// 不是 [`get_cell_dimensions`]。
pub fn calculate_image_cell_size(
    image_dimensions: ImageDimensions,
    max_width_cells: f64,
    max_height_cells: Option<f64>,
    cell_dimensions: CellDimensions,
    optimize_aspect_ratio: bool,
) -> ImageCellSize {
    // 阶段 1：把限制值与图片尺寸都夹到正数（JS `Math.max(1, ...)`；`NaN` 会传播为 `NaN`）。
    let max_width = js_max(1.0, js_floor(max_width_cells));
    let max_height = max_height_cells.map(|value| js_max(1.0, js_floor(value)));
    let image_width = js_max(1.0, image_dimensions.width_px);
    let image_height = js_max(1.0, image_dimensions.height_px);

    // 阶段 2：分别算出宽、高方向允许的缩放倍数（可小于 1，即缩小），取较小者作为统一缩放比例
    // （等比缩放到限制框内）；只有图片本身小于限制框时对应倍数才大于 1。
    let width_scale = (max_width * cell_dimensions.width_px) / image_width;
    let height_scale = match max_height {
        None => width_scale,
        Some(max_height) => (max_height * cell_dimensions.height_px) / image_height,
    };
    let scale = js_min(width_scale, height_scale);

    // 阶段 3：把缩放后的像素尺寸换算成终端列/行并夹到上限；行数额外受 maxHeightCells 限制。
    let scaled_width_px = image_width * scale;
    let scaled_height_px = image_height * scale;
    let mut columns = js_max(
        1.0,
        js_min(
            max_width,
            js_ceil(scaled_width_px / cell_dimensions.width_px),
        ),
    );
    let height_rows = scaled_height_px / cell_dimensions.height_px;
    let mut rows = js_max(1.0, js_ceil(height_rows));
    if let Some(max_height) = max_height {
        rows = js_min(max_height, rows);
    }

    if !optimize_aspect_ratio {
        return ImageCellSize {
            columns: to_usize(columns),
            rows: to_usize(rows),
        };
    }

    // 阶段 4（仅 Kitty）：宽度先触顶时修正行数，否则修正列数；在 n 与 n-1 之间选让实际
    // 宽高比更接近原图的那个（`choose_less_distorted_cell_count`）。
    if width_scale <= height_scale {
        let ideal_rows = (columns * cell_dimensions.width_px * image_height)
            / (image_width * cell_dimensions.height_px);
        rows = choose_less_distorted_cell_count(to_usize(rows), ideal_rows) as f64;
    } else {
        let ideal_columns = (rows * cell_dimensions.height_px * image_width)
            / (image_height * cell_dimensions.width_px);
        columns = choose_less_distorted_cell_count(to_usize(columns), ideal_columns) as f64;
    }

    ImageCellSize {
        columns: to_usize(columns),
        rows: to_usize(rows),
    }
}

/// `calculateImageCellSize()` 的默认参数形态：`cellDimensions = { widthPx: 9, heightPx: 18 }`、
/// `optimizeAspectRatio = false`。
///
/// 注意 TS 的默认单元格尺寸是**静态的 9x18**，不是 [`get_cell_dimensions`]；需要当前终端实测
/// 尺寸的调用方应显式传入（`renderImage` 就是这么做的）。
pub fn calculate_image_cell_size_default(
    image_dimensions: ImageDimensions,
    max_width_cells: f64,
    max_height_cells: Option<f64>,
) -> ImageCellSize {
    calculate_image_cell_size(
        image_dimensions,
        max_width_cells,
        max_height_cells,
        CellDimensions::default(),
        false,
    )
}

/// `calculateImageRows(imageDimensions, targetWidthCells, cellDimensions?)`。
///
/// TS 的默认参数 `cellDimensions = { widthPx: 9, heightPx: 18 }` 见 [`calculate_image_rows_default`]。
pub fn calculate_image_rows(
    image_dimensions: ImageDimensions,
    target_width_cells: f64,
    cell_dimensions: CellDimensions,
) -> usize {
    calculate_image_cell_size(
        image_dimensions,
        target_width_cells,
        None,
        cell_dimensions,
        false,
    )
    .rows
}

/// `calculateImageRows()` 的默认参数形态：`cellDimensions = { widthPx: 9, heightPx: 18 }`。
pub fn calculate_image_rows_default(
    image_dimensions: ImageDimensions,
    target_width_cells: f64,
) -> usize {
    calculate_image_rows(
        image_dimensions,
        target_width_cells,
        CellDimensions::default(),
    )
}

/// `f64` → `usize`：JS 侧这些值总是正的整数字面量；非有限值退化为 0（不会静默截断出巨大值）。
fn to_usize(value: f64) -> usize {
    if value.is_finite() && value > 0.0 {
        value as usize
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// 图片头部尺寸解析
// ---------------------------------------------------------------------------

/// `getPngDimensions(base64Data)`：PNG 头，大端 `readUInt32BE(16)` / `(20)`。
///
/// 解码后不足 24 字节或前 4 字节不是 `89 50 4E 47`（`\x89PNG`）时返回 `None`。
pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data);
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0] != 0x89 || buffer[1] != 0x50 || buffer[2] != 0x4e || buffer[3] != 0x47 {
        return None;
    }
    let width = read_u32_be(&buffer, 16);
    let height = read_u32_be(&buffer, 20);
    Some(ImageDimensions {
        width_px: f64::from(width),
        height_px: f64::from(height),
    })
}

/// `getJpegDimensions(base64Data)`：扫描 SOF0/SOF1/SOF2 段。
///
/// 解码后不足 2 字节或不是 `FF D8` 时返回 `None`；遇到长度字段小于 2 的非法段也返回 `None`
/// （长度字段包含自身，`<2` 的段非法；与 TS 的 `length < 2 → null` 及
/// `spec/tui-core.md` §2.15.7 一致）。其它段按长度跳过，只有 SOF0/SOF1/SOF2 携带尺寸。
pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data);
    if buffer.len() < 2 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    // JPEG 段格式：`FF <marker> <2 字节大端长度>`，长度包含长度字段自身；SOF0/1/2 的
    // 负载依次是精度(1)、高(2)、宽(2)。`offset + 9 < len` 保证 marker、长度与两个尺寸字段
    // 都在界内（读取用 offset+8）。
    let mut offset = 2usize;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }

        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            let height = read_u16_be(&buffer, offset + 5);
            let width = read_u16_be(&buffer, offset + 7);
            return Some(ImageDimensions {
                width_px: f64::from(width),
                height_px: f64::from(height),
            });
        }

        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = read_u16_be(&buffer, offset + 2);
        if length < 2 {
            return None;
        }
        offset += 2 + usize::from(length);
    }

    None
}

/// `getGifDimensions(base64Data)`：GIF87a/GIF89a，小端 `readUInt16LE(6)` / `(8)`。
///
/// 解码后不足 10 字节或签名不是 `GIF87a`/`GIF89a` 时返回 `None`。签名按 Node 的 ascii
/// 解码比较（见 `node_ascii`），高位字节会被掩码，不是严格的 UTF-8。
pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data);
    if buffer.len() < 10 {
        return None;
    }
    let sig = node_ascii(&buffer[..6]);
    if sig != "GIF87a" && sig != "GIF89a" {
        return None;
    }
    let width = read_u16_le(&buffer, 6);
    let height = read_u16_le(&buffer, 8);
    Some(ImageDimensions {
        width_px: f64::from(width),
        height_px: f64::from(height),
    })
}

/// `getWebpDimensions(base64Data)`：RIFF/WEBP，按 `VP8 `/`VP8L`/`VP8X` 分支。
///
/// 解码后不足 30 字节、`RIFF`/`WEBP` 签名不符或块类型未知时返回 `None`。
pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = base64_decode(base64_data);
    if buffer.len() < 30 {
        return None;
    }

    let riff = node_ascii(&buffer[..4]);
    let webp = node_ascii(&buffer[8..12]);
    if riff != "RIFF" || webp != "WEBP" {
        return None;
    }

    let chunk = node_ascii(&buffer[12..16]);
    if chunk == "VP8 " {
        let width = read_u16_le(&buffer, 26) & 0x3fff;
        let height = read_u16_le(&buffer, 28) & 0x3fff;
        return Some(ImageDimensions {
            width_px: f64::from(width),
            height_px: f64::from(height),
        });
    } else if chunk == "VP8L" {
        // `buffer.length < 25` 的检查在 TS 里永远为假（前面已要求 ≥ 30）；保留以对齐结构。
        if buffer.len() < 25 {
            return None;
        }
        let bits = read_u32_le(&buffer, 21);
        let width = (bits & 0x3fff) + 1;
        let height = ((bits >> 14) & 0x3fff) + 1;
        return Some(ImageDimensions {
            width_px: f64::from(width),
            height_px: f64::from(height),
        });
    } else if chunk == "VP8X" {
        let width =
            u32::from(buffer[24]) | (u32::from(buffer[25]) << 8) | (u32::from(buffer[26]) << 16);
        let height =
            u32::from(buffer[27]) | (u32::from(buffer[28]) << 8) | (u32::from(buffer[29]) << 16);
        return Some(ImageDimensions {
            width_px: f64::from(width + 1),
            height_px: f64::from(height + 1),
        });
    }

    None
}

/// `getImageDimensions(base64Data, mimeType)`。
///
/// 按 MIME 精确匹配（`image/png`、`image/jpeg`、`image/gif`、`image/webp`）；未知 MIME 返回
/// `None`，不嗅探内容。
pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

/// `Buffer.prototype.toString("ascii")`：每个 UTF-16 单元取 `byte & 0x7f`。
///
/// Node 的 `latin1`/`ascii` 解码**不做** UTF-8 校验，高位字节被掩码成 `byte & 0x7f`，
/// 因此 `0xC7 0xC9 0xC6 0xB8 0xB7 0xE1` 会解码成 `"GIF87a"`。用 `String::from_utf8`
/// 会把这类输入误判成「不是 GIF/WEBP」。
fn node_ascii(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(byte & 0x7f)).collect()
}

fn read_u16_be(buffer: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([buffer[offset], buffer[offset + 1]])
}

fn read_u16_le(buffer: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buffer[offset], buffer[offset + 1]])
}

fn read_u32_be(buffer: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

fn read_u32_le(buffer: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buffer[offset],
        buffer[offset + 1],
        buffer[offset + 2],
        buffer[offset + 3],
    ])
}

// ---------------------------------------------------------------------------
// 渲染与文本兜底
// ---------------------------------------------------------------------------

/// `renderImage(base64Data, imageDimensions, options?)`：终端不支持图片时返回 `None`。
///
/// 能力来自 [`get_capabilities`]，单元格尺寸来自 [`get_cell_dimensions`]；`max_width_cells` 缺省
/// 为 80。Kitty 分支：若给了 `image_id`，先把尺寸写进元数据注册表（供裁剪/placement 查询），
/// 再编码；`move_cursor: Some(false)` 会加 `C=1`。iTerm2 分支固定 `height=auto`，
/// `preserveAspectRatio` 缺省保持（只有 `Some(false)` 才写 0），结果不带 `image_id`。
pub fn render_image(
    base64_data: &str,
    image_dimensions: ImageDimensions,
    options: ImageRenderOptions,
) -> Option<RenderedImage> {
    let capabilities = get_capabilities();
    // 没有图片协议：提前返回 None，调用方改走 image_fallback。
    let protocol = capabilities.images?;

    let max_width = options.max_width_cells.unwrap_or(80.0);
    // 只优化 Kitty 的单元格对齐畸变，不缩小 iTerm2 的预留行数。
    let size = calculate_image_cell_size(
        image_dimensions,
        max_width,
        options.max_height_cells,
        get_cell_dimensions(),
        protocol == ImageProtocol::Kitty,
    );

    match protocol {
        ImageProtocol::Kitty => {
            // 先按 image_id 记元数据（裁剪与 placement 查询依赖它），再生成传输命令。
            if let Some(image_id) = options.image_id {
                register_kitty_image_metadata(KittyImageMetadata {
                    image_id,
                    columns: size.columns,
                    rows: size.rows,
                    width_px: image_dimensions.width_px,
                    height_px: image_dimensions.height_px,
                });
            }
            let sequence = encode_kitty(
                base64_data,
                KittyEncodeOptions {
                    columns: Some(size.columns as f64),
                    rows: Some(size.rows as f64),
                    image_id: options.image_id.map(|id| id as f64),
                    move_cursor: options.move_cursor,
                },
            );
            Some(RenderedImage {
                sequence,
                columns: size.columns,
                rows: size.rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::Iterm2 => {
            let sequence = encode_iterm2(
                base64_data,
                Iterm2EncodeOptions {
                    width: Some(Iterm2Size::Number(size.columns as f64)),
                    height: Some(Iterm2Size::Text("auto".to_string())),
                    name: None,
                    preserve_aspect_ratio: options.preserve_aspect_ratio.or(Some(true)),
                    inline: None,
                },
            );
            Some(RenderedImage {
                sequence,
                columns: size.columns,
                rows: size.rows,
                image_id: None,
            })
        }
    }
}

/// `hyperlink(text, url)`：OSC 8 包裹。
///
/// 输出 `ESC ] 8 ; ; <url> ST <text> ESC ] 8 ; ; ST`。`url` 与 `text` 都原样拼接、不做转义；
/// 调用方需自行确认终端支持超链接（`TerminalCapabilities::hyperlinks`），不支持的终端可能
/// 吞掉文本。
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// `shortenImagePath(filename)`：home 前缀压缩成 `~/...`（也处理 Windows 的 `\\` 分隔符）。
///
/// TS 是 `if (home && ...)`——home 为**空串**时为假，不做压缩（`$HOME=""` 时
/// `os.homedir()` 返回空串，但绝不能把任何绝对路径都缩成 `~`）。
fn shorten_image_path(filename: &str) -> String {
    let Some(home) = home_dir() else {
        return filename.to_string();
    };
    let home = home.to_string_lossy();
    if !home.is_empty()
        && (filename == home
            || filename.starts_with(&format!("{home}/"))
            || filename.starts_with(&format!("{home}\\")))
    {
        return format!("~{}", &filename[home.len()..]);
    }
    filename.to_string()
}

/// `imageFallback(mimeType, dimensions?, filename?)`：无图片能力时的文本行。
///
/// 输出形如 `[Image: <filename> [<mimeType>] <W>x<H>]`：`filename` 非空时先做 `~/` 压缩
/// （见 `shorten_image_path`）；若 [`get_capabilities`] 报告支持超链接且路径是绝对路径，再包成
/// OSC 8 链接（URL 由 [`path_to_file_url`] 生成），否则只放纯文本。`mimeType` 总是输出；
/// `dimensions` 为 `Some` 时追加 `宽x高`。
pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<ImageDimensions>,
    filename: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    // TS `if (filename)`：空串是假值，与「未提供」同义。
    if let Some(filename) = filename.filter(|name| !name.is_empty()) {
        let display = shorten_image_path(filename);
        if get_capabilities().hyperlinks && is_absolute_path(filename) {
            parts.push(hyperlink(&display, &path_to_file_url(filename)));
        } else {
            parts.push(display);
        }
    }
    parts.push(format!("[{mime_type}]"));
    if let Some(dimensions) = dimensions {
        parts.push(format!(
            "{}x{}",
            js_number_text(dimensions.width_px),
            js_number_text(dimensions.height_px)
        ));
    }
    format!("[Image: {}]", parts.join(" "))
}

// ---------------------------------------------------------------------------
// 测试支撑（`#[doc(hidden)]`）：环境变量快照/还原
// ---------------------------------------------------------------------------

/// 能力探测读取的全部环境变量（TS 测试的 `ENV_KEYS` 前 16 项）。
///
/// 顺序固定（测试逐项快照/还原）；`CMUX_WORKSPACE_ID` 源码从不读取，保留它是为了让还原覆盖调用方可能设置过的全部键。
#[doc(hidden)]
pub const CAPABILITY_ENV_KEYS: [&str; 16] = [
    "TERM",
    "TERM_PROGRAM",
    "TERMINAL_EMULATOR",
    "COLORTERM",
    "TMUX",
    "KITTY_WINDOW_ID",
    "GHOSTTY_RESOURCES_DIR",
    "WEZTERM_PANE",
    "ITERM_SESSION_ID",
    "WT_SESSION",
    "CMUX_WORKSPACE_ID",
    "WARP_SESSION_ID",
    "WARP_TERMINAL_SESSION_UUID",
    "KIANA_HYPERLINKS",
    "KIANA_IMAGE_PROTOCOL",
    "KIANA_TRUE_COLOR",
];

/// 清空 [`CAPABILITY_ENV_KEYS`] 中出现的所有变量，返回快照供 [`restore_capability_env`] 还原。
///
/// # Safety
///
/// 修改进程环境变量在 Rust 2024 中是 `unsafe`：多线程并发读写环境变量是未定义行为。
/// 调用方必须持有进程级互斥锁（测试里由 `EnvGuard` 保证）。
#[doc(hidden)]
pub fn clear_capability_env() -> Vec<(&'static str, Option<String>)> {
    let mut saved = Vec::with_capacity(CAPABILITY_ENV_KEYS.len());
    for key in CAPABILITY_ENV_KEYS {
        let current = std::env::var_os(key).map(|value| value.to_string_lossy().into_owned());
        // SAFETY: 调用方持有进程级互斥锁，见函数文档。
        unsafe { std::env::remove_var(key) };
        saved.push((key, current));
    }
    saved
}

/// 还原 [`clear_capability_env`] 的快照。
///
/// # Safety
///
/// 同 [`clear_capability_env`]：调用方必须持有进程级互斥锁。
#[doc(hidden)]
pub fn restore_capability_env(saved: Vec<(&'static str, Option<String>)>) {
    for (key, value) in saved {
        // SAFETY: 调用方持有进程级互斥锁，见函数文档。
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// 设置/删除单个能力相关环境变量（`None` 表示删除）。
///
/// # Safety
///
/// 同 [`clear_capability_env`]：调用方必须持有进程级互斥锁。
#[doc(hidden)]
pub fn set_capability_env(key: &str, value: Option<&str>) {
    // SAFETY: 调用方持有进程级互斥锁，见函数文档。
    unsafe {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
