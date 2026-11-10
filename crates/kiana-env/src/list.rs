//! `list.rs`：`listFiles`——按 `fd --hidden --exclude .git` 的方式列出目录下的文件与目录（不跟随符号链接），
//! 跳过 git 忽略的内容；底层用 fd 与 ripgrep 同款的 `ignore` crate。
//!
//! TS 源：packages/env/daemon/src/list.rs（原 daemon 本身就是 Rust；逐字移植）。
//! `listFiles`: the files and directories below a directory, as `fd --hidden --exclude .git` lists them (without
//! following symbolic links), skipping what git ignores. Uses the `ignore` crate, which fd and ripgrep use.
//!
//! # 在系统中的位置
//! `main.rs` 的 `dispatch` 收到 `listFiles` 请求时调用 [`run`]，并把返回的二进制 payload 直接放进 RESULT 帧的
//! payload 部分。它是协议 1 发布后新增的 feature（`hello.features` 里的 `listFiles`），客户端会先检查
//! `features` 再使用；旧守护进程不认识这个操作，会把它当作未知操作拒绝。
//!
//! # 为什么需要它
//! 远端与本地环境需要同一种“列出目录树”的实现。Durable 没有对应的 `NodeExecutionEnv` 方法，所以由守护进程
//! 直接提供，`LocalExecutionEnv`（本机也起一个守护进程）因此与远端得到完全相同的列表
//! （见 docs/semantics.md 的 “listFiles has no NodeExecutionEnv counterpart”）。语义上模仿
//! `fd --hidden --exclude .git`，但明确不跟随符号链接，也不像 `git ls-files` 那样列出被忽略的已跟踪文件。
//!
//! # 主要流程
//! 1. [`Options::from_json`] 解析可选参数并填入默认值；
//! 2. [`list`] 检查根目录，配置 `ignore::WalkBuilder`（隐藏文件、git 忽略、排除名、排序、不跟链接）；
//! 3. 深度优先遍历，每个条目检查一次取消标志；超出条目数 / payload 字节数 / 时间预算时置 `truncated` 并停止；
//! 4. 条目编码为 `kind 字节 + 相对路径 + NUL`，与 `{ count, truncated }` 一起返回。
//!
//! # 重要约束
//! - 只输出常规文件、目录和符号链接；FIFO（命名管道）、socket、设备等其他类型被跳过。
//! - `maxBytes` 限制的是 payload 字节数（每条记录的编码长度），不是磁盘上的文件大小；同时被压到
//!   `MAX_PAYLOAD` 以内，保证整个列表能放进一个 RESULT 帧。
//! - 根目录用 `fs::metadata` 检查（跟随符号链接），所以“指向目录的链接”可以作为根；遍历本身不跟随任何链接。
//! - 单个条目/目录的读取错误被跳过，不会让整个请求失败；只有根目录打不开、根不是目录、参数无效或取消才返回错误。

use crate::errors::Failure;
use crate::frame::MAX_PAYLOAD;
use ignore::WalkBuilder;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

const DEFAULT_MAX_ENTRIES: u64 = 50_000;
const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 10_000;

/// `listFiles` 的可选参数；缺省值与 docs/protocol.md 的说明一致。
///
/// 通常经 [`Options::from_json`] 构造：它把缺失字段填成默认值，并对计数/数组字段的类型错误返回 `EINVAL`。
/// 字段本身不做范围校验，例如 `max_entries = 0` 会让遇到的第一个条目就触发 `truncated`。
pub struct Options {
    /// Include names starting with `.` (and, on Windows, hidden files).
    ///
    /// 默认 `true`（相当于 `fd --hidden`）。为 `false` 时传给 `WalkBuilder::hidden(true)`，名字以 `.` 开头的
    /// 项被跳过，Windows 上还会跳过带隐藏属性的项。
    pub hidden: bool,
    /// Skip what git ignores inside git work trees: `.gitignore` files, `.git/info/exclude`, the global excludes file.
    ///
    /// 默认 `true`，但只在 git 工作树（work tree，含 `.git` 的仓库目录）内生效
    /// （`WalkBuilder::require_git(true)`）；普通目录里的 `.gitignore`
    /// 不会起作用。详见 [`list`] 的配置说明与下方测试。
    pub gitignore: bool,
    /// Names skipped with everything below them, in addition to `.git`.
    ///
    /// 按文件名精确比较（`OsString` 相等），匹配的是条目名而不是相对路径；命中的目录会连同整棵子树被剪掉。
    pub exclude: Vec<OsString>,
    /// 条目数量上限；遇到第 `max_entries + 1` 个条目时停止遍历并置 `truncated`。恰好等于上限且没有更多条目时
    /// `truncated` 为 false。默认 50 000。
    pub max_entries: u64,
    /// Bytes of the listing payload: per entry one kind byte, the path, and a NUL.
    ///
    /// 默认 4 MiB；`from_json` 会再把它压到 `MAX_PAYLOAD` 以内，避免结果帧超过协议上限。这里的字节数是
    /// payload 的编码长度，不是被列出文件的大小。
    pub max_bytes: u64,
    /// 遍历的墙钟时间预算；超过后停止并置 `truncated`。默认 10 000 ms。
    pub timeout: Duration,
}

impl Options {
    /// 解析 `listFiles` 的 JSON 参数。
    ///
    /// - `hidden` / `gitignore`：缺省为 `true`；值存在但不是布尔时 `as_bool` 返回 `None`，同样回落为 `true`
    ///   （这是本实现的宽容行为，不是协议对客户端的承诺）。
    /// - `exclude`：必须是字符串数组或缺失；数组里出现非字符串会整体报 `EINVAL invalid field exclude`。
    /// - `maxEntries` / `maxBytes` / `timeoutMs`：缺失时用默认值；存在但类型不是非负整数（`u64`）时返回
    ///   `EINVAL invalid field <name>`，不会静默改用默认值。
    /// - `maxBytes` 最终取 `min(给定值, MAX_PAYLOAD)`，保证结果能装进一帧。
    ///
    /// 返回的 [`Options`] 只做结构解析，不检查路径是否存在；那是 [`list`] 的工作。
    pub fn from_json(json: &Value) -> Result<Options, Failure> {
        // 计数参数共用：`json[key]` 在字段缺失或 JSON 不是对象时索引出 Null，因此“缺失”和显式 null 等价。
        let count = |key: &str, default: u64| -> Result<u64, Failure> {
            match &json[key] {
                Value::Null => Ok(default),
                value => value
                    .as_u64()
                    .ok_or_else(|| Failure::new("EINVAL", format!("invalid field {key}"))),
            }
        };
        let exclude = match &json["exclude"] {
            Value::Null => Vec::new(),
            Value::Array(names) => names
                .iter()
                .map(|name| name.as_str().map(OsString::from))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| Failure::new("EINVAL", "invalid field exclude"))?,
            _ => return Err(Failure::new("EINVAL", "invalid field exclude")),
        };
        Ok(Options {
            hidden: json["hidden"].as_bool().unwrap_or(true),
            gitignore: json["gitignore"].as_bool().unwrap_or(true),
            exclude,
            max_entries: count("maxEntries", DEFAULT_MAX_ENTRIES)?,
            max_bytes: count("maxBytes", DEFAULT_MAX_BYTES)?.min(MAX_PAYLOAD as u64),
            timeout: Duration::from_millis(count("timeoutMs", DEFAULT_TIMEOUT_MS)?),
        })
    }
}

/// `listFiles` 的遍历结果；`payload` 是线协议里的二进制部分，不是 JSON。
pub struct Listing {
    /// Records of a kind byte (`f`, `d`, `l`), the path relative to the root with `/` separators, and a NUL.
    ///
    /// 必须与同一结构体的 `count` 一致（由服务端保证）。客户端按 NUL 切分记录并读取 `truncated`，当前不使用
    /// `count` 校验。
    pub payload: Vec<u8>,
    /// payload 中的记录条数；每条记录以 1 个 kind 字节开头，所以它也是 kind 字节的个数。
    pub count: u64,
    /// 是否因为 `max_entries` / `max_bytes` / `timeout` 而提前停止；为 true 时 payload 只是前半棵树。
    pub truncated: bool,
}

/// The path of `path` below `root`, its components joined with `/`; names that are not UTF-8 get replacement
/// characters, as `readdir` reports them.
///
/// 返回 `root` 之下的相对路径，各分量用 `/` 连接（Windows 上也统一输出 `/`，协议规定如此）。不是 `root`
/// 的后代、或含非普通分量时返回 `None`；`root` 自身得到空串，也转成 `None`，调用方据此跳过根条目。
///
/// 非 UTF-8 文件名经 `to_string_lossy` 变成 U+FFFD；不同的原始字节可能解码成同一个字符串，协议接受这一点
/// （payload 里的路径是展示用的，客户端不需要用它做反向查找）。
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let mut text = String::new();
    for component in rest.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        if !text.is_empty() {
            text.push('/');
        }
        text.push_str(&name.to_string_lossy());
    }
    (!text.is_empty()).then_some(text)
}

/// List `root` depth first, siblings in byte order of their names. Stops with `truncated` at the first entry beyond
/// a bound; fails with `aborted` once `aborted()` is true. Entries that cannot be read are skipped.
///
/// # 参数与错误
/// - `root` 必须是目录：这里用 `fs::metadata`（跟随符号链接，所以指向目录的链接可以作为根）。打不开按
///   syscall `scandir` 报错，不是目录报 `ENOTDIR`（同样带 `scandir` 文案）。
/// - `aborted` 每处理一个条目调用一次；返回 true 时整体失败，code 为 `aborted`（与“截断”不同）。
/// - 超出 `max_entries` / `max_bytes` / `timeout` 不算错误：返回已收集的部分并置 `truncated`。
///
/// # 遍历配置（每一项对应的协议语义）
/// - `hidden(!options.hidden)`：`ignore` crate 里 `hidden(true)` 表示“跳过隐藏项”，这里取反；
/// - `ignore(false)`：不读 `.ignore` 文件，只保留 git 系的忽略规则；
/// - `parents` / `git_ignore` / `git_exclude` / `git_global`：分别对应父目录的 `.gitignore`、当前及子目录的
///   `.gitignore`、`.git/info/exclude`、全局 excludes 文件；
/// - `require_git(true)`：只有位于 git 工作树内才应用上述 git 规则；
/// - `follow_links(false)`：符号链接只报告自身，不进入目标（避免环路和越界）；
/// - `current_dir(root)`：全局 excludes 文件相对被列出的目录查找，而不是守护进程自己的工作目录；
/// - `filter_entry`：`.git` 和 `exclude` 里的名字连同整棵子树被剪掉。
///
/// # 失败与跳过
/// 根目录之外的错误都是“按条目跳过”：`ignore` 的迭代项是 `Result`，读不了的目录/条目不会中断遍历。
/// 取消、时间预算和数量/字节上限则分别用 `aborted` 错误与 `truncated` 标记表达，调用方可以安全地只使用
/// 已返回的部分。
pub fn list(root: &str, options: &Options, aborted: &dyn Fn() -> bool) -> Result<Listing, Failure> {
    let metadata = fs::metadata(root).map_err(|error| Failure::io(&error, "scandir", root))?;
    if !metadata.is_dir() {
        return Err(Failure::new(
            "ENOTDIR",
            format!("ENOTDIR: not a directory, scandir '{root}'"),
        )
        .path(root));
    }
    let root_path = Path::new(root);
    let exclude = options.exclude.clone();
    let walk = WalkBuilder::new(root_path)
        // `hidden(true)` 是“跳过隐藏项”，取反后协议里的 `hidden: true` 才是“包含隐藏项”。
        .hidden(!options.hidden)
        .ignore(false)
        .parents(options.gitignore)
        .git_ignore(options.gitignore)
        .git_exclude(options.gitignore)
        .git_global(options.gitignore)
        // 没有这一项，普通目录里的 .gitignore 也会生效；协议只承诺在 git 工作树内跳过被忽略的内容。
        .require_git(true)
        .follow_links(false)
        // The global excludes file is found relative to the listed directory, not the daemon's.
        .current_dir(root_path)
        // 兄弟条目按文件名排序；协议要求深度优先且同级顺序固定（Unix 上 `OsStr` 比较即字节序）。
        .sort_by_file_name(|a, b| a.cmp(b))
        // filter_entry 返回 false 的目录不会进入，整棵子树随之被跳过。
        .filter_entry(move |entry| {
            let name = entry.file_name();
            name != ".git" && !exclude.iter().any(|excluded| excluded == name)
        })
        .build();
    let started = Instant::now();
    let mut listing = Listing {
        payload: Vec::new(),
        count: 0,
        truncated: false,
    };
    for entry in walk {
        // 每处理一个条目检查一次取消：树可能很大，协议只承诺在条目之间停止。
        if aborted() {
            return Err(Failure::new("aborted", "aborted"));
        }
        // 条目级错误（权限不足、遍历中被删除等）按协议跳过，不影响其余结果。
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 {
            // 根自身不是列表项：payload 只包含 root 下面的内容。
            continue;
        }
        // 无法取得文件类型时不能分类成 f/d/l，只能跳过。
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        // 先判链接：file_type 来自目录项本身，不跟随链接，所以指向目录的链接应记为 l 而不是 d。
        let kind = if file_type.is_symlink() {
            b'l'
        } else if file_type.is_dir() {
            b'd'
        } else if file_type.is_file() {
            b'f'
        } else {
            // FIFO、socket、设备等不在协议的三种 kind 内。
            continue;
        };
        let Some(path) = relative(root_path, entry.path()) else {
            continue;
        };
        // 一条记录的编码长度 = 1 个 kind 字节 + 路径的 UTF-8 字节数 + 1 个 NUL。
        let size = path.len() as u64 + 2;
        // 先检查预算再写入：宁可少一条，也不让 payload 越过 maxBytes / 条数上限 / 时间预算。
        if listing.count >= options.max_entries
            || listing.payload.len() as u64 + size > options.max_bytes
            || started.elapsed() > options.timeout
        {
            listing.truncated = true;
            break;
        }
        listing.payload.push(kind);
        listing.payload.extend_from_slice(path.as_bytes());
        listing.payload.push(0);
        listing.count += 1;
    }
    Ok(listing)
}

/// `listFiles` 的请求入口：读取 `path`，解析 [`Options`]，执行 [`list`]，返回
/// `({ count, truncated }, payload)`。
///
/// `path` 缺失或不是字符串时返回 `EINVAL missing field path`；参数解析与遍历错误原样上抛。返回的 payload
/// 由 `main.rs` 直接放进 RESULT 帧，因此其长度上限必须与 `MAX_PAYLOAD` 对齐（见 [`Options::from_json`]）。
pub fn run(json: &Value, aborted: &dyn Fn() -> bool) -> Result<(Value, Vec<u8>), Failure> {
    let root = json["path"]
        .as_str()
        .ok_or_else(|| Failure::new("EINVAL", "missing field path"))?;
    let listing = list(root, &Options::from_json(json)?, aborted)?;
    Ok((
        json!({ "count": listing.count, "truncated": listing.truncated }),
        listing.payload,
    ))
}

// 测试用自清理的临时目录锁定：排序、git 忽略的作用范围、符号链接不跟随、三种预算与错误码。
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Tree(std::path::PathBuf);

    impl Tree {
        fn new(files: &[&str]) -> Tree {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "kiana-env-list-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let tree = Tree(root);
            for file in files {
                tree.write(file, "");
            }
            tree
        }

        fn write(&self, path: &str, content: &str) {
            let path = self.0.join(path);
            if path.to_string_lossy().ends_with('/') {
                fs::create_dir_all(&path).unwrap();
                return;
            }
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }

        fn list(&self, options: Options) -> (Vec<String>, bool) {
            let listing = list(&self.0.to_string_lossy(), &options, &|| false).unwrap();
            let entries: Vec<String> = listing
                .payload
                .split(|byte| *byte == 0)
                .filter(|record| !record.is_empty())
                .map(|record| {
                    format!(
                        "{} {}",
                        record[0] as char,
                        String::from_utf8_lossy(&record[1..])
                    )
                })
                .collect();
            assert_eq!(entries.len() as u64, listing.count);
            (entries, listing.truncated)
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn options() -> Options {
        Options::from_json(&json!({})).unwrap()
    }

    #[test]
    fn lists_files_and_directories_depth_first_in_name_order() {
        let tree = Tree::new(&[
            "b.txt",
            "a/z.txt",
            "a/b/c.txt",
            "empty/",
            ".hidden/x",
            "A.txt",
        ]);
        let (entries, truncated) = tree.list(options());
        assert_eq!(
            entries,
            [
                "d .hidden",
                "f .hidden/x",
                "f A.txt",
                "d a",
                "d a/b",
                "f a/b/c.txt",
                "f a/z.txt",
                "f b.txt",
                "d empty"
            ]
        );
        assert!(!truncated);
        let (entries, _) = tree.list(Options {
            hidden: false,
            ..options()
        });
        assert!(!entries.iter().any(|entry| entry.contains(".hidden")));
    }

    #[test]
    fn skips_what_git_ignores_only_in_work_trees() {
        let tree = Tree::new(&[
            "keep.txt",
            "out/x.js",
            "sub/keep.txt",
            "sub/drop.log",
            "node_modules/m/i.js",
        ]);
        tree.write(".gitignore", "out/\n*.log\n");
        tree.write("sub/.gitignore", "!keep.txt\n");
        // Not a work tree yet: everything is listed.
        let (entries, _) = tree.list(options());
        assert!(entries.contains(&"f out/x.js".to_string()));
        tree.write(".git/HEAD", "ref: refs/heads/main\n");
        tree.write(".git/info/exclude", "keep.txt\n");
        let (entries, _) = tree.list(Options {
            exclude: vec!["node_modules".into()],
            ..options()
        });
        assert_eq!(
            entries,
            [
                "f .gitignore",
                "d sub",
                "f sub/.gitignore",
                "f sub/keep.txt"
            ]
        );
        let (entries, _) = tree.list(Options {
            gitignore: false,
            ..options()
        });
        assert!(entries.contains(&"f out/x.js".to_string()));
        assert!(!entries.iter().any(|entry| entry.contains(".git/")));
        // A subdirectory of a work tree applies the rules of its parents.
        let sub = list(&tree.0.join("sub").to_string_lossy(), &options(), &|| false).unwrap();
        assert_eq!(sub.payload, b"f.gitignore\0fkeep.txt\0");
    }

    #[cfg(unix)]
    #[test]
    fn lists_symbolic_links_without_following_them() {
        let tree = Tree::new(&["dir/inner.txt"]);
        std::os::unix::fs::symlink(tree.0.join("dir"), tree.0.join("link")).unwrap();
        std::os::unix::fs::symlink("missing", tree.0.join("dangling")).unwrap();
        std::os::unix::fs::symlink(&tree.0, tree.0.join("dir/loop")).unwrap();
        let (entries, _) = tree.list(options());
        assert_eq!(
            entries,
            [
                "l dangling",
                "d dir",
                "f dir/inner.txt",
                "l dir/loop",
                "l link"
            ]
        );
    }

    #[test]
    fn stops_at_bounds_with_truncated() {
        let tree = Tree::new(&["a", "b", "c"]);
        let (entries, truncated) = tree.list(Options {
            max_entries: 2,
            ..options()
        });
        assert_eq!((entries.len(), truncated), (2, true));
        let (entries, truncated) = tree.list(Options {
            max_entries: 3,
            ..options()
        });
        assert_eq!((entries.len(), truncated), (3, false));
        let (entries, truncated) = tree.list(Options {
            max_bytes: 6,
            ..options()
        });
        assert_eq!(
            (entries, truncated),
            (vec!["f a".to_string(), "f b".to_string()], true)
        );
        let (entries, truncated) = tree.list(Options {
            timeout: Duration::ZERO,
            ..options()
        });
        assert!(truncated && entries.is_empty());
    }

    #[test]
    fn fails_on_missing_roots_files_and_aborts() {
        let tree = Tree::new(&["file"]);
        let missing = list(
            &tree.0.join("missing").to_string_lossy(),
            &options(),
            &|| false,
        );
        assert_eq!(missing.err().unwrap().code, "ENOENT");
        let file = list(&tree.0.join("file").to_string_lossy(), &options(), &|| {
            false
        });
        assert_eq!(file.err().unwrap().code, "ENOTDIR");
        let aborted = list(&tree.0.to_string_lossy(), &options(), &|| true);
        assert_eq!(aborted.err().unwrap().code, "aborted");
    }
}
