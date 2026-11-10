//! `native/darwin/src/darwin-platform.m`（132 行）的 Rust 等价物。
//!
//! 导出（**逐字**）：`isModifierPressed`、`getText`、`setText`、`getImage`、`getFilePaths`。
//! AppKit 的 `NSPasteboard.generalPasteboard`：
//!
//! - `getText` → `stringForType(NSPasteboardTypeString)`，UTF-8 编码后返回；
//! - `getFilePaths` → `readObjectsForClasses:options:` 取 `NSURL`，返回 `fileSystemRepresentation`；
//! - `getImage` → 优先 `NSPasteboardTypePNG`，否则 TIFF → PNG 重编码；
//! - `isModifierPressed` → `CGEventSourceFlagsState(kCGEventSourceStateCombinedSessionState)`
//!   （与 C 相同的外部函数，不是 `NSEvent.modifierFlags`）；
//! - `setText` → `clearContents` + `setString:forType:`。
//!
//! AppKit 调用沿用 C 的形状：同步代码在**私有线程**上执行（`clipboard.h` 的
//! `napi_create_async_work`/libuv 线程池等价物），事件循环线程只等待 future，最多等 3 秒；
//! 忙时其它调用立即返回 `Unavailable`。错误文案逐字沿用 C。
//!
//! # 设计原因
//!
//! `NSPasteboard` 的调用是同步的；C 版把 `clipboard_execute` 交给 libuv 线程池执行，这里
//! 保持同样的线程模型：所有 `NSPasteboard` 访问都在 `ClipboardWorker` 的私有线程上，
//! TUI 事件循环线程只等待 future。
//!
//! # 执行流程
//!
//! 1. [`DarwinClipboard`] 的剪贴板方法（`get_text`/`get_image`/`get_file_paths`/`set_text`）
//!    先向 `ClipboardWorker` 占位；忙时立即得到 [`ClipboardValue::Unavailable`]。
//!    `is_modifier_pressed` 例外：它是同步查询、不经过 worker（见注意事项）。
//! 2. 工作线程调用本文件的 `read_text`/`read_image`/`read_file_paths`/`write_text`；每次操作
//!    都重新调用 `NSPasteboard::generalPasteboard()`，本类型不缓存 pasteboard 对象。
//! 3. 读操作映射为三态：类型不存在 → `Empty`；传输失败 → `Err(ClipboardError)`；有内容 →
//!    `Value`。写操作不同：成功 → `Unavailable`（TS resolve `undefined`），失败 →
//!    `Err(ClipboardError)`。失败文案是当前稳定契约（用户可见），改动需同步测试。
//!
//! # 注意事项
//!
//! - `get_text` 的字节数来自 `lengthOfBytesUsingEncoding(NSUTF8StringEncoding)`（不含结尾
//!   NUL），随后用 `from_utf8_lossy` 解码：非法字节变成 U+FFFD 而不是报错；只有 `UTF8String`
//!   返回空指针时才得到 `Could not encode clipboard text`。
//! - `get_file_paths` 只接受文件 URL（`NSPasteboardURLReadingFileURLsOnlyKey=true`）；没有 URL
//!   或路径列表为空时是 `Empty`，不是 `Value(vec![])`。
//! - `get_image` 优先返回 `NSPasteboardTypePNG` 的原始字节；PNG 数据缺失时走
//!   `NSImage` → `NSBitmapImageRep` → PNG 重编码（典型来源是 TIFF）；代码没有单独判断
//!   TIFF 是否可用，任一中间步骤失败都得到 `Clipboard does not contain an image`。
//! - `is_modifier_pressed` 是同步查询，不经过 worker：直接在调用它的线程上读
//!   `CGEventSourceFlagsState`，不受 worker 忙闲影响。
//! - `set_text` 成功时 future 的结果是 `Unavailable`（TS 写操作 resolve `undefined`），不要
//!   把它误判为失败；`Option` 返回 `None` 才表示“该平台不导出 setText”（本实现始终返回
//!   `Some`）。

use std::ffi::CStr;

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AnyThread, ClassType};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSImage, NSPasteboard,
    NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    NSPasteboardURLReadingFileURLsOnlyKey,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL, NSUTF8StringEncoding};

use super::{
    ClipboardError, ClipboardFuture, ClipboardValue, ClipboardWorker, ModifierKey, NativeClipboard,
};

/// `kCGEventFlagMaskShift`。
const K_CG_EVENT_FLAG_MASK_SHIFT: u64 = 0x0002_0000;
/// `kCGEventFlagMaskControl`。
const K_CG_EVENT_FLAG_MASK_CONTROL: u64 = 0x0004_0000;
/// `kCGEventFlagMaskAlternate`。
const K_CG_EVENT_FLAG_MASK_ALTERNATE: u64 = 0x0008_0000;
/// `kCGEventFlagMaskCommand`。
const K_CG_EVENT_FLAG_MASK_COMMAND: u64 = 0x0010_0000;
/// `kCGEventSourceStateCombinedSessionState`。
const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION_STATE: i32 = 0;

// CoreGraphics 由系统框架提供（`#[link]` 在链接期解析）。`CGEventSourceFlagsState` 是只读
// 查询：返回当前会话的 `CGEventFlags` 位图，上面那些 `K_CG_EVENT_FLAG_MASK_*` 与它按位与。
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    /// `CGEventFlags CGEventSourceFlagsState(CGEventSourceStateID stateID)`。
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
}

// SAFETY: AppKit 的剪贴板类型常量在进程生命周期内恒有效；读取 extern static 只是一次指针加载。
fn pasteboard_type_string() -> &'static NSString {
    unsafe { NSPasteboardTypeString }
}

/// `NSPasteboardTypePNG`。
fn pasteboard_type_png() -> &'static NSString {
    unsafe { NSPasteboardTypePNG }
}

/// `NSPasteboardTypeTIFF`。
fn pasteboard_type_tiff() -> &'static NSString {
    unsafe { NSPasteboardTypeTIFF }
}

/// `NSPasteboardURLReadingFileURLsOnlyKey`。
fn file_urls_only_key() -> &'static NSString {
    unsafe { NSPasteboardURLReadingFileURLsOnlyKey }
}

/// `darwin-platform.node` 的等价物。
///
/// 每个实例持有一个 `ClipboardWorker`（单槽位）：并发调用中只有第一个会真正执行，其余立即
/// `Unavailable`。父模块在 `darwin` feature 下把它缓存成进程级单例。
pub struct DarwinClipboard {
    /// `clipboard.h` 的私有工作线程（忙 → `Unavailable`、3 秒上限、异步执行）。
    worker: ClipboardWorker,
}

impl Default for DarwinClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl DarwinClipboard {
    /// 创建实例：只初始化 worker 状态，不访问 `NSPasteboard`；第一次操作时才读剪贴板。
    pub fn new() -> Self {
        DarwinClipboard {
            worker: ClipboardWorker::new(),
        }
    }
}

impl NativeClipboard for DarwinClipboard {
    fn get_text(&self) -> ClipboardFuture<'_, String> {
        self.worker.run(read_text)
    }

    fn get_image(&self) -> ClipboardFuture<'_, Vec<u8>> {
        self.worker.run(read_image)
    }

    fn get_file_paths(&self) -> Option<ClipboardFuture<'_, Vec<String>>> {
        Some(self.worker.run(read_file_paths))
    }

    fn set_text(&self, text: String) -> Option<ClipboardFuture<'_, ()>> {
        Some(self.worker.run(move || write_text(&text)))
    }

    /// 把 [`ModifierKey`] 映射到对应的 `CGEventFlags` 掩码后按位与查询。
    ///
    /// `Command` 对应 `kCGEventFlagMaskCommand`，`Option` 对应 `kCGEventFlagMaskAlternate`
    /// （macOS 命名），不是 `NSEvent.modifierFlags` 的键码。
    fn is_modifier_pressed(&self, key: ModifierKey) -> bool {
        let mask = match key {
            ModifierKey::Shift => K_CG_EVENT_FLAG_MASK_SHIFT,
            ModifierKey::Command => K_CG_EVENT_FLAG_MASK_COMMAND,
            ModifierKey::Control => K_CG_EVENT_FLAG_MASK_CONTROL,
            ModifierKey::Option => K_CG_EVENT_FLAG_MASK_ALTERNATE,
        };
        // SAFETY: CGEventSourceFlagsState 是只读查询，任意线程可调用。
        unsafe {
            (CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION_STATE) & mask) != 0
        }
    }
}

/// `CLIPBOARD_TEXT` 分支。
///
/// `stringForType` 为 nil → `Empty`（剪贴板里没有文本）；`UTF8String` 为空指针 →
/// `Err("Could not encode clipboard text")`；否则按 UTF-8 字节数解码为 `Value`。
fn read_text() -> Result<ClipboardValue<String>, ClipboardError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let Some(text) = pasteboard.stringForType(pasteboard_type_string()) else {
        return Ok(ClipboardValue::Empty);
    };
    let length = text.lengthOfBytesUsingEncoding(NSUTF8StringEncoding);
    let pointer = text.UTF8String();
    if pointer.is_null() {
        return Err(ClipboardError::new("Could not encode clipboard text"));
    }
    // SAFETY: UTF8String 返回以 NUL 结尾的缓冲，`lengthOfBytesUsingEncoding` 给出不含 NUL 的
    // 字节数（C 里的 `[text lengthOfBytesUsingEncoding:NSUTF8StringEncoding]`）。
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
    Ok(ClipboardValue::Value(
        String::from_utf8_lossy(bytes).into_owned(),
    ))
}

/// `CLIPBOARD_WRITE` 分支。
///
/// 返回值：成功时是 `Unavailable`（写操作在 TS 里 resolve `undefined`）；`clearContents` 后
/// `setString:forType:` 失败时是 `Err("Could not set clipboard text")`。
fn write_text(text: &str) -> Result<ClipboardValue<()>, ClipboardError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    // C 先按 UTF-8 解码（失败 → "Clipboard text is not valid UTF-8"）；Rust 的 `&str` 已经是
    // 合法 UTF-8，因此该错误不可达。
    let string = NSString::from_str(text);
    pasteboard.clearContents();
    if !pasteboard.setString_forType(&string, pasteboard_type_string()) {
        return Err(ClipboardError::new("Could not set clipboard text"));
    }
    // `complete_clipboard_work` 对写操作把 promise resolve 成 `undefined`。
    Ok(ClipboardValue::Unavailable)
}

/// `CLIPBOARD_FILES` 分支。
///
/// 只接受文件 URL（`NSPasteboardURLReadingFileURLsOnlyKey=true`）。没有匹配的 URL，或返回的
/// 路径列表为空时是 `Empty`；路径按 `fileSystemRepresentation` 的原始字节转成 `String`
/// （非法 UTF-8 会被 `from_utf8_lossy` 替换为 U+FFFD）。
fn read_file_paths() -> Result<ClipboardValue<Vec<String>>, ClipboardError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let classes: Retained<NSArray<AnyClass>> = NSArray::from_slice(&[NSURL::class()]);
    let only_file_urls: Retained<AnyObject> = NSNumber::numberWithBool(true).into();
    let options = NSDictionary::<NSString, AnyObject>::from_slices(
        &[file_urls_only_key()],
        &[&*only_file_urls],
    );
    // SAFETY: `NSURL` 在 classes 里，返回的对象都是 NSURL 实例（C 里同样直接按 NSURL 使用）。
    let urls = unsafe { pasteboard.readObjectsForClasses_options(&classes, Some(&options)) };
    let Some(urls) = urls else {
        return Ok(ClipboardValue::Empty);
    };
    // SAFETY: `readObjectsForClasses:@[NSURL.class]` 返回的元素都是 `NSURL`。
    let urls: Retained<NSArray<NSURL>> = unsafe { Retained::cast_unchecked(urls) };
    let mut paths: Vec<String> = Vec::new();
    for url in urls.iter() {
        // C: `const char* path = url.fileSystemRepresentation; if (!path) continue;`
        let representation = url.fileSystemRepresentation();
        // SAFETY: fileSystemRepresentation 返回以 NUL 结尾的 POSIX 路径。
        let path = unsafe { CStr::from_ptr(representation.as_ptr()) };
        paths.push(String::from_utf8_lossy(path.to_bytes()).into_owned());
    }
    if paths.is_empty() {
        return Ok(ClipboardValue::Empty);
    }
    Ok(ClipboardValue::Value(paths))
}

/// `CLIPBOARD_IMAGE` 分支。
///
/// 1. 先看 PNG/TIFF 是否可用；都不可用 → `Empty`。
/// 2. 有 PNG 就直接取原始字节；PNG 数据缺失时经 `NSImage`/`NSBitmapImageRep` 重编码成 PNG
///    （典型来源是 TIFF）；代码没有单独判断 TIFF 是否可用。
/// 3. 两步都拿不到数据 → `Err("Clipboard does not contain an image")`。
fn read_image() -> Result<ClipboardValue<Vec<u8>>, ClipboardError> {
    let pasteboard = NSPasteboard::generalPasteboard();
    let types = NSArray::from_slice(&[pasteboard_type_png(), pasteboard_type_tiff()]);
    if pasteboard.availableTypeFromArray(&types).is_none() {
        return Ok(ClipboardValue::Empty);
    }
    let png = match pasteboard.dataForType(pasteboard_type_png()) {
        Some(data) => Some(data),
        None => {
            let image = NSImage::initWithPasteboard(NSImage::alloc(), &pasteboard);
            let tiff = image.and_then(|image| image.TIFFRepresentation());
            let bitmap = tiff.and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff));
            bitmap.and_then(|bitmap| {
                let properties = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
                // SAFETY: 空的 properties 字典与 C 的 `@{}` 一致。
                unsafe {
                    bitmap
                        .representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
                }
            })
        }
    };
    match png {
        Some(data) => Ok(ClipboardValue::Value(data.to_vec())),
        None => Err(ClipboardError::new("Clipboard does not contain an image")),
    }
}
