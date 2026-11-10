//! `native/win32/src/win32-platform.c`（241 行）的 Rust 等价物。
//!
//! 导出（**逐字**）：`enableVirtualTerminalInput`、`isModifierPressed`、`getText`、`setText`、
//! `getImage`。链接 `kernel32` 与 `user32`（由 `windows-sys` 的
//! `Win32_System_DataExchange`/`Win32_System_Memory`/`Win32_System_Ole`/`Win32_UI_Input_KeyboardAndMouse`
//! features 提供）。
//!
//! - `getText` → `OpenClipboard`/`GetClipboardData(u32::from(CF_UNICODETEXT))`；`GlobalLock` 后按
//!   **code unit 数**（不是字节数）构造字符串（`clipboard_copy` 的 length 语义）；
//! - `getImage` → `CF_DIB`/`CF_DIBV5` → BMP 编码（TS 侧输出 Uint8Array）；
//! - `setText` → 消息窗口 owner + `EmptyClipboard` + `GlobalAlloc(GPTR)` 语义的
//!   `GlobalAlloc(GMEM_MOVEABLE)` + `SetClipboardData(CF_UNICODETEXT)`；
//! - `enableVirtualTerminalInput` → `GetStdHandle(STD_INPUT_HANDLE)` +
//!   `GetConsoleMode`/`SetConsoleMode(ENABLE_VIRTUAL_TERMINAL_INPUT)`；
//! - `isModifierPressed` → `GetAsyncKeyState(VK_SHIFT/VK_CONTROL/VK_MENU/VK_LWIN|VK_RWIN)`。
//!
//! 所有失败路径错误文案逐字沿用 `clipboard.h`/C 源。剪贴板操作在**私有线程**上执行
//! （`clipboard.h` 的 `napi_create_async_work`/libuv 线程池等价物）：事件循环线程只等待
//! future，最多等 3 秒；忙时其它调用立即返回 `Unavailable`（`OpenClipboard` 的 10×5ms
//! 重试因此不会卡住 TUI 线程）。
//!
//! # 设计原因
//!
//! Win32 剪贴板 API 是同步的，`OpenClipboard` 也可能失败；C 版因此用 10×5 ms 重试等待。
//! 这里保持同样的线程模型，把重试与 I/O 都放在 `ClipboardWorker` 的私有线程上，TUI 事件
//! 循环线程只等待最多 3 秒的 future。
//!
//! # 执行流程（一次读取）
//!
//! 1. 工作线程调用 `open_clipboard(NULL)`（读取不需要 owner 窗口）；失败 →
//!    `Err("Could not open clipboard")`。
//! 2. `read_clipboard_text`/`read_clipboard_image` 在已打开的剪贴板上取数据。
//! 3. `CloseClipboard()` 之后结果送回等待方；打开成功后的读取无论成功还是失败都会关闭。
//!
//! 写入需要真实的 owner 窗口：在**同一工作线程**上创建 `HWND_MESSAGE` 的 `STATIC` 窗口，
//! 打开剪贴板、写入，最后销毁窗口。
//!
//! # DIB → BMP
//!
//! `CF_DIB`/`CF_DIBV5` 的数据没有文件头，不能直接当 BMP 文件使用：`dib_pixel_offset` 根据
//! 头大小、调色板与位域掩码算出像素数据偏移，再在前面补 14 字节 `BITMAPFILEHEADER`。偏移
//! 无法确定（返回 0）或总长度超出 `u32` 时返回
//! `Could not create clipboard image buffer`，不会返回截断的图片。
//!
//! # 注意事项
//!
//! - 读用 `OpenClipboard(NULL)`，写必须有 owner 窗口；窗口的创建/销毁与 `CloseClipboard`
//!   都只在工作线程上发生。
//! - `SetClipboardData` 成功后内存所有权转给系统，代码不再 `GlobalFree`；只有失败路径才释放。
//! - `enable_virtual_terminal_input` 与 `is_modifier_pressed` 是同步调用，不经过 worker：
//!   前者修改本进程 stdin 的控制台模式，后者用 `GetAsyncKeyState` 读键盘状态。

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    ENABLE_VIRTUAL_TERMINAL_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleA;
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_UNICODETEXT};
use windows_sys::Win32::System::Threading::Sleep;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL,
    VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

use super::{
    ClipboardError, ClipboardFuture, ClipboardValue, ClipboardWorker, ModifierKey, NativeClipboard,
};

/// `BI_ALPHABITFIELDS`。
const BI_ALPHABITFIELDS: u32 = 6;
/// `BI_BITFIELDS`。
const BI_BITFIELDS: u32 = 3;
/// `KEY_PRESSED_MASK`。
const KEY_PRESSED_MASK: u16 = 0x8000;
/// `OPEN_CLIPBOARD_ATTEMPTS`。
const OPEN_CLIPBOARD_ATTEMPTS: u32 = 10;
/// `OPEN_CLIPBOARD_RETRY_MS`。
const OPEN_CLIPBOARD_RETRY_MS: u32 = 5;
/// `BITMAP_FILE_HEADER_SIZE`。
const BITMAP_FILE_HEADER_SIZE: usize = 14;

/// `win32-platform.node` 的等价物。
///
/// 每个实例持有一个 `ClipboardWorker`（单槽位）：并发调用中只有第一个会真正执行，其余立即
/// `Unavailable`。父模块在 `win32` feature 下把它缓存成进程级单例。
pub struct Win32Clipboard {
    /// `clipboard.h` 的私有工作线程（忙 → `Unavailable`、3 秒上限、异步执行）。
    worker: ClipboardWorker,
}

impl Default for Win32Clipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl Win32Clipboard {
    /// 创建实例：只初始化 worker 状态，不访问剪贴板，也不创建 owner 窗口。
    pub fn new() -> Self {
        Win32Clipboard {
            worker: ClipboardWorker::new(),
        }
    }
}

impl NativeClipboard for Win32Clipboard {
    fn get_text(&self) -> ClipboardFuture<'_, String> {
        self.worker.run(read_text)
    }

    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>> {
        self.worker.run(read_image)
    }

    fn set_text(&self, text: String) -> Option<ClipboardFuture<'_, ()>> {
        Some(self.worker.run(move || write_text(&text)))
    }

    /// 给本进程 stdin 的控制台模式加上 `ENABLE_VIRTUAL_TERMINAL_INPUT`。
    ///
    /// 依次检查句柄有效、`GetConsoleMode` 成功、`SetConsoleMode` 成功，任一失败返回 `false`。
    /// stdin 被重定向或不是控制台时调用会失败；调用方 `terminal.rs` 忽略返回值（此时
    /// Shift+Tab 无法与 Tab 区分）。
    fn enable_virtual_terminal_input(&self) -> bool {
        // SAFETY: 只查询/设置本进程的 stdin 控制台模式。
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0u32;
            handle != INVALID_HANDLE_VALUE
                && !handle.is_null()
                && GetConsoleMode(handle, &mut mode) != 0
                && SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) != 0
        }
    }

    /// 用 `GetAsyncKeyState` 的高位（`KEY_PRESSED_MASK`）判断按键当前是否按下。
    ///
    /// Shift/Control/Option 都同时查询通用虚拟键（`VK_SHIFT` 等）与左右键；Command 只查
    /// `VK_LWIN`/`VK_RWIN`。查询结果按位与为 0 时视为未按下。
    fn is_modifier_pressed(&self, key: ModifierKey) -> bool {
        let pressed = |virtual_key: i32| -> bool {
            // SAFETY: GetAsyncKeyState 只读取键盘状态。
            (unsafe { GetAsyncKeyState(virtual_key) } as u16 & KEY_PRESSED_MASK) != 0
        };
        match key {
            ModifierKey::Shift => {
                pressed(VK_SHIFT.into()) || pressed(VK_LSHIFT.into()) || pressed(VK_RSHIFT.into())
            }
            ModifierKey::Control => {
                pressed(VK_CONTROL.into())
                    || pressed(VK_LCONTROL.into())
                    || pressed(VK_RCONTROL.into())
            }
            ModifierKey::Option => {
                pressed(VK_MENU.into()) || pressed(VK_LMENU.into()) || pressed(VK_RMENU.into())
            }
            ModifierKey::Command => pressed(VK_LWIN.into()) || pressed(VK_RWIN.into()),
        }
    }
}

/// `open_clipboard(owner)`：最多重试 10 次，每次间隔 5 ms。
///
/// C 版对 `OpenClipboard` 失败做 10×5 ms 的重试（见 `spec/tui-core.md` §2.16.5），这里保持
/// 一致。`Sleep` 是阻塞的，因此**必须在工作线程上调用**，不能放到 TUI 事件循环线程。
unsafe fn open_clipboard(owner: HWND) -> bool {
    for _ in 0..OPEN_CLIPBOARD_ATTEMPTS {
        // SAFETY: 调用方保证窗口句柄有效（或为 0）。
        if unsafe { OpenClipboard(owner) } != 0 {
            return true;
        }
        // SAFETY: Sleep 只让出 CPU。
        unsafe { Sleep(OPEN_CLIPBOARD_RETRY_MS) };
    }
    false
}

/// `read_u16`：按小端读取 DIB/BMP 头里的 16 位字段（调用方保证至少 2 字节）。
fn read_u16(data: &[u8]) -> u16 {
    u16::from(data[0]) | (u16::from(data[1]) << 8)
}

/// `read_u32`：按小端读取 DIB/BMP 头里的 32 位字段（调用方保证至少 4 字节）。
fn read_u32(data: &[u8]) -> u32 {
    u32::from(data[0])
        | (u32::from(data[1]) << 8)
        | (u32::from(data[2]) << 16)
        | (u32::from(data[3]) << 24)
}

/// `write_u32`：按小端写入 BMP 文件头字段（调用方保证至少 4 字节）。
fn write_u32(data: &mut [u8], value: u32) {
    data[0] = value as u8;
    data[1] = (value >> 8) as u8;
    data[2] = (value >> 16) as u8;
    data[3] = (value >> 24) as u8;
}

/// `dib_pixel_offset(dib, size)`：`CF_DIB` 像素数据相对 DIB 起点的字节偏移。
///
/// `dib` 从 `biSize` 开始（没有 14 字节文件头）。返回值 0 是“无法可靠计算”的哨兵，调用方
/// 据此报错，而不是拼出一个错误的 BMP：
///
/// - 数据不足 12 字节，或头大小既不是 12（`BITMAPCOREHEADER`）也不落在 `40..=size`；
/// - `BITMAPCOREHEADER`：8 位及以下的调色板项是 RGBTRIPLE（每项 3 字节），偏移为
///   `12 + (1 << bpp) * 3`；
/// - `BITMAPINFOHEADER` 及更大：调色板每项 4 字节；`biClrUsed == 0` 且 bpp ≤ 8 时按
///   `1 << bpp` 项计；
/// - `header_size == 40` 且压缩为 `BI_BITFIELDS`/`BI_ALPHABITFIELDS` 时，掩码 DWORD 紧跟在
///   头后，需要再各加 12/16 字节；
/// - 最终偏移超过 `size` 也返回 0。
fn dib_pixel_offset(dib: &[u8]) -> usize {
    let size = dib.len();
    if size < 12 {
        return 0;
    }
    let header_size = read_u32(dib);

    if header_size == 12 {
        let bits_per_pixel = u32::from(read_u16(&dib[10..]));
        let color_count = if bits_per_pixel <= 8 {
            1u32 << bits_per_pixel
        } else {
            0
        };
        let offset = header_size as usize + color_count as usize * 3;
        return if offset <= size { offset } else { 0 };
    }

    if header_size < 40 || header_size as usize > size {
        return 0;
    }
    let bits_per_pixel = u32::from(read_u16(&dib[14..]));
    let compression = read_u32(&dib[16..]);
    let mut color_count = read_u32(&dib[32..]);
    if color_count == 0 && bits_per_pixel <= 8 {
        color_count = 1u32 << bits_per_pixel;
    }

    let mut offset = header_size as usize + color_count as usize * 4;
    if header_size == 40 && compression == BI_BITFIELDS {
        offset += 3 * 4;
    }
    if header_size == 40 && compression == BI_ALPHABITFIELDS {
        offset += 4 * 4;
    }
    if offset <= size { offset } else { 0 }
}

/// `read_clipboard_image(job)`：读 PNG 或 DIB，统一返回可直接当文件使用的字节。
///
/// 格式优先级与 C 一致：注册格式 `"PNG"` → `CF_DIBV5` → `CF_DIB`。PNG 原样返回；DIB 由
/// `dib_pixel_offset` 补充 14 字节 `BITMAPFILEHEADER`（`BM`、总大小、像素偏移）。
///
/// 没有可用格式 → `Empty`；`GlobalLock`/`GlobalSize` 失败 →
/// `Err("Clipboard does not contain an image")`；偏移不可算（返回 0）或 `size + 14` 超出
/// `u32` → `Err("Could not create clipboard image buffer")`。非空的锁定内存在每条返回路径上
/// 都会 `GlobalUnlock`。
unsafe fn read_clipboard_image() -> Result<ClipboardValue<Vec<u8>>, ClipboardError> {
    // SAFETY: 调用方已打开剪贴板。
    unsafe {
        let png_format = RegisterClipboardFormatW(windows_sys::w!("PNG"));
        if (png_format == 0 || IsClipboardFormatAvailable(png_format) == 0)
            && IsClipboardFormatAvailable(u32::from(CF_DIBV5)) == 0
            && IsClipboardFormatAvailable(u32::from(CF_DIB)) == 0
        {
            return Ok(ClipboardValue::Empty);
        }
        let mut handle: HGLOBAL = if png_format != 0 && IsClipboardFormatAvailable(png_format) != 0
        {
            GetClipboardData(png_format)
        } else {
            null_mut()
        };
        let is_png = !handle.is_null();
        if handle.is_null() && IsClipboardFormatAvailable(u32::from(CF_DIBV5)) != 0 {
            handle = GetClipboardData(u32::from(CF_DIBV5));
        }
        if handle.is_null() && IsClipboardFormatAvailable(u32::from(CF_DIB)) != 0 {
            handle = GetClipboardData(u32::from(CF_DIB));
        }

        let source = if handle.is_null() {
            null_mut()
        } else {
            GlobalLock(handle)
        };
        let size = if handle.is_null() {
            0
        } else {
            GlobalSize(handle)
        };
        if source.is_null() || size == 0 {
            if !source.is_null() {
                GlobalUnlock(handle);
            }
            return Err(ClipboardError::new("Clipboard does not contain an image"));
        }
        let bytes = std::slice::from_raw_parts(source.cast::<u8>(), size);
        if is_png {
            let result = ClipboardValue::Value(bytes.to_vec());
            GlobalUnlock(handle);
            return Ok(result);
        }

        let offset = dib_pixel_offset(bytes);
        let result = if offset != 0 && size <= u32::MAX as usize - BITMAP_FILE_HEADER_SIZE {
            let mut bitmap = vec![0u8; size + BITMAP_FILE_HEADER_SIZE];
            bitmap[0] = b'B';
            bitmap[1] = b'M';
            write_u32(&mut bitmap[2..], (size + BITMAP_FILE_HEADER_SIZE) as u32);
            write_u32(&mut bitmap[10..], (BITMAP_FILE_HEADER_SIZE + offset) as u32);
            bitmap[BITMAP_FILE_HEADER_SIZE..].copy_from_slice(bytes);
            Ok(ClipboardValue::Value(bitmap))
        } else {
            Err(ClipboardError::new(
                "Could not create clipboard image buffer",
            ))
        };
        GlobalUnlock(handle);
        result
    }
}

/// `CLIPBOARD_TEXT` 分支（调用方负责打开/关闭剪贴板）。
///
/// 没有 `CF_UNICODETEXT` → `Empty`；`GlobalLock` 失败 → `Err("Clipboard does not contain text")`。
/// 长度按 `GlobalSize(handle)/2` 个 UTF-16 code unit 计，遇到 NUL 截断（C 的 `length` 语义是
/// code unit 数，不是字节数）。
unsafe fn read_clipboard_text() -> Result<ClipboardValue<String>, ClipboardError> {
    // SAFETY: 调用方已打开剪贴板。
    unsafe {
        if IsClipboardFormatAvailable(u32::from(CF_UNICODETEXT)) == 0 {
            return Ok(ClipboardValue::Empty);
        }
        let handle = GetClipboardData(u32::from(CF_UNICODETEXT));
        let text = if handle.is_null() {
            null_mut()
        } else {
            GlobalLock(handle)
        };
        if text.is_null() {
            return Err(ClipboardError::new("Clipboard does not contain text"));
        }
        let capacity = GlobalSize(handle) / 2;
        let units = text.cast::<u16>();
        let mut length = 0usize;
        while length < capacity && *units.add(length) != 0 {
            length += 1;
        }
        let slice = std::slice::from_raw_parts(units, length);
        // JS 字符串可以保存**未配对**的代理项；Rust 的 `String` 只能存合法的 Unicode 标量。
        // 配对代理项（全部正常文本）逐字保留，未配对的孤立代理项只能替换为 U+FFFD
        // （Rust 类型约束，与 `kiana-json` 的 `\uD800` 决策同类）。
        let value = String::from_utf16_lossy(slice);
        GlobalUnlock(handle);
        Ok(ClipboardValue::Value(value))
    }
}

/// `clipboard_execute` 的 `CLIPBOARD_TEXT` 分支。
///
/// 读取不需要 owner 窗口（C 里只有 `CLIPBOARD_WRITE` 创建）；打开失败直接返回 `Err`，打开
/// 成功后无论读取结果如何都会 `CloseClipboard`，避免把剪贴板锁住。
fn read_text() -> Result<ClipboardValue<String>, ClipboardError> {
    // 读取不需要 owner 窗口（C 里 CLIPBOARD_WRITE 才创建）。
    // SAFETY: 未打开剪贴板的失败路径直接返回错误。
    let opened = unsafe { open_clipboard(null_mut()) };
    if !opened {
        return Err(ClipboardError::new("Could not open clipboard"));
    }
    // SAFETY: 剪贴板已打开，读取完立即关闭。
    let result = unsafe { read_clipboard_text() };
    unsafe { CloseClipboard() };
    result
}

/// `clipboard_execute` 的 `CLIPBOARD_WRITE` 分支。
///
/// 步骤：创建 `HWND_MESSAGE` 的 `STATIC` owner 窗口 → `open_clipboard(owner)` →
/// `GlobalAlloc(GMEM_MOVEABLE)` + `GlobalLock` 写入 UTF-16（含结尾 NUL）→ `EmptyClipboard`
/// + `SetClipboardData(CF_UNICODETEXT)` → `CloseClipboard` + `DestroyWindow`。
///
/// 失败点与文案：建窗失败 `Could not create clipboard owner window`；打开失败
/// `Could not open clipboard`；其余（分配/锁定/Empty/Set 失败）`Could not set clipboard text`。
/// 成功时返回 `Unavailable`（TS 写操作 resolve `undefined`）。
fn write_text(text: &str) -> Result<ClipboardValue<()>, ClipboardError> {
    // 文本写入需要一个真实的剪贴板 owner 窗口（Node 无控制台时也一样）。
    // SAFETY: 窗口创建/销毁都成对出现，且都在同一线程。
    unsafe {
        let owner = CreateWindowExW(
            0,
            windows_sys::w!("STATIC"),
            null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            null_mut(),
            GetModuleHandleA(null()),
            null(),
        );
        if owner.is_null() {
            return Err(ClipboardError::new(
                "Could not create clipboard owner window",
            ));
        }
        if !open_clipboard(owner) {
            DestroyWindow(owner);
            return Err(ClipboardError::new("Could not open clipboard"));
        }

        let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let size = units.len() * std::mem::size_of::<u16>();
        let handle = GlobalAlloc(GMEM_MOVEABLE, size);
        let target = if handle.is_null() {
            null_mut()
        } else {
            GlobalLock(handle)
        };
        let mut success = false;
        if !target.is_null() {
            std::ptr::copy_nonoverlapping(units.as_ptr().cast::<u8>(), target.cast::<u8>(), size);
            GlobalUnlock(handle);
            success = EmptyClipboard() != 0
                && !SetClipboardData(u32::from(CF_UNICODETEXT), handle).is_null();
        }
        if !success {
            if !handle.is_null() {
                GlobalFree(handle);
            }
            CloseClipboard();
            DestroyWindow(owner);
            return Err(ClipboardError::new("Could not set clipboard text"));
        }
        CloseClipboard();
        DestroyWindow(owner);
        // `complete_clipboard_work` 对写操作把 promise resolve 成 `undefined`。
        Ok(ClipboardValue::Unavailable)
    }
}

/// `clipboard_execute` 的 `CLIPBOARD_IMAGE` 分支。
///
/// 与 `read_text` 相同：打开失败直接返回 `Err`，成功路径在读取后 `CloseClipboard`。
fn read_image() -> Result<ClipboardValue<Vec<u8>>, ClipboardError> {
    // SAFETY: 打开失败直接返回错误；成功路径保证最终关闭。
    unsafe {
        if !open_clipboard(null_mut()) {
            return Err(ClipboardError::new("Could not open clipboard"));
        }
        let result = read_clipboard_image();
        CloseClipboard();
        result
    }
}

// `HANDLE` 在类型层面就是 `*mut c_void`；这个别名让 clippy 的 `unused` 检查不误报。
const _: fn(HANDLE) = |_| {};
