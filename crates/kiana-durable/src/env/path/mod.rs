//! `node:path` 的 posix/win32 两套语义的唯一实现位置（`ARCHITECTURE.md` §4.9 / GAPS G4.3）。
//!
//! TS 侧 `packages/durable/src/env/node.ts`、`node-watch.ts` 与 `packages/env/src/{ssh,remote-env}.ts`
//! 都直接从 `node:path` 取 `join/resolve/dirname/basename/isAbsolute/relative/sep`（以及
//! `path.win32.*`）。Rust 的 `std::path` 语义不同（`..` 归一化、盘符相对路径、尾随分隔符、
//! `//` 折叠），必须逐函数移植两份实现（`spec/env-evals-durable-testing.md` §6.2 第 1 条、
//! §8.3 第 1 条）。
//!
//! **落点决策**：放在 `kiana-durable::env::path`。理由：
//!
//! 1. `kiana-env-client`（L3）已经依赖 `kiana-durable`（L2），而 `kiana-core`（L3）不依赖
//!    `kiana-durable` 也不依赖 `kiana-env-client`，放 `kiana-durable` 是唯一不会引入新边的位置；
//! 2. TS 里真正需要 win32 语义的调用点全部在 `durable::env::node` 与 `env-client`
//!    （daemon 自身是 Rust，用 `std::path` 即可，不需要 JS 语义）；
//! 3. `kiana-env-client` 直接复用本模块：`src/remote_env.rs` 里
//!    `use kiana_durable::env::path::{PathFlavor, posix, win32};`，再按 `PathFlavor` 分派，
//!    保证“只有一份实现”。（`ARCHITECTURE.md` §8 第 23 条描述的
//!    `pub use kiana_durable::env::path::posix as node_path;` 是设计意图，当前源码没有这个再导出。）
//!
//! 两个子模块导出同名函数：`join`、`resolve`、`dirname`、`basename`、`is_absolute`、
//! `relative`、`normalize`、`SEP`。语义以 Node 文档与 TS 测试为准。
//!
//! # 设计原因
//!
//! Node 的 `path.posix` 与 `path.win32` 大部分是**纯字符串算法**：`normalize`、`join`、`dirname`、
//! `basename`、`is_absolute` 对相同输入总是返回相同字符串，与宿主平台无关。`resolve` / `relative`
//! 并非完全如此：结果由参数与 `cwd`（win32 的盘符相对参数还包括 `=C:` 环境变量）共同决定，而默认
//! 包装（`posix::resolve`、`posix::relative`、`win32::resolve`、`win32::relative`）每次都会先读取
//! 进程 cwd（`std::env::current_dir()`），win32 侧还传入 `cfg!(windows)` 作为 `is_windows`——它只
//! 决定快路径是否把 cwd 的 `/` 换成 `\`。Rust 的 `std::path` 则按编译目标平台解释路径（Windows 上
//! `\` 是分隔符），且不提供 Node 的 `..` 折叠、`//` 折叠与尾随分隔符规则，也不提供 `path.win32` 对
//! 盘符相对路径（`C:foo`）、UNC/设备路径的纯字符串处理规则（`std::path` 的 `Prefix` 语义与输出形式
//! 都与 Node 不同）。下游要把这些字符串当作 watch 键、错误里的 `path` 字段等逐字节比较的值，所以
//! 必须按 `lib/path.js` 逐函数移植，不能换成 `std::path`。
//!
//! # 执行流程
//!
//! 1. 调用方先决定语义版本：本地 `env::node` 用编译期 `cfg!(windows)` 直接在 posix/win32 之间选；
//!    远端环境（`kiana-env-client::remote_env::remote_path`）拿不到**远端**的平台信息，只能等
//!    daemon 报告系统名后再构造 [`PathFlavor`]。
//! 2. 按选定的 flavor 调用 `posix`/`win32` 中的同名函数；本模块不提供运行时分派包装。
//! 3. 这些函数都是纯字符串运算，不访问文件系统；只有默认包装（[`posix::resolve`] /
//!    [`posix::resolve_string`] / [`posix::relative`] 及其 win32 对应物）会读取一次进程 cwd
//!    作为缺省根。
//!
//! # 注意事项
//!
//! - **不要先用 `std::path` 计算再转回字符串**：`PathBuf` 不保证与 Node 相同的 `..` 折叠、
//!   `//` 折叠与尾随分隔符；[`posix::resolve`] 返回 `PathBuf` 只是匹配骨架冻结的签名。
//! - 语义由 `tests/fixtures/path_corpus.json` 锁定（见下方 `golden` 模块）：改任何分支前先
//!   想清楚是否会让某个 corpus 用例的逐字节输出变化。
//! - 两个子模块的同名函数签名并不完全对称：`win32::resolve_string_with_cwd` /
//!   `win32::relative_with_cwd` 额外接收 `is_windows`，跨平台调用以各自模块文档为准。

pub mod posix;
pub mod win32;

/// `nodePlatform(info).platform === "win32" ? win32 : posix` 的选择器。
///
/// 只表示“用哪一套路径函数”，自身不做任何路径运算。本地环境在编译期就能确定平台
/// （`env::node` 用 `cfg!(windows)`），远端环境则要等 daemon 报告系统名后才能确定，
/// 因此需要把这个选择做成值来传递。`#[derive(Default)]` 的默认变体是 [`PathFlavor::Posix`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PathFlavor {
    /// `node:path.posix`
    #[default]
    Posix,
    /// `node:path.win32`
    Win32,
}

impl PathFlavor {
    /// 从 Node 的 `process.platform` 拼写（`"win32"`）选择；其它值一律 posix。
    ///
    /// 参数用的是 **Node 的拼写**，不是 Rust 的 `std::env::consts::OS`：传 `"windows"` 会得到
    /// posix。`kiana-env-client` 先用 `node_platform` 把 daemon 报告的系统名换成 Node 拼写，
    /// 再调用本函数。只认精确的 `"win32"`（大小写敏感）。
    pub fn from_node_platform(platform: &str) -> Self {
        if platform == "win32" {
            PathFlavor::Win32
        } else {
            PathFlavor::Posix
        }
    }

    /// 对应的 `sep`（`"/"` 或 `"\\"`）。
    pub fn sep(self) -> &'static str {
        match self {
            PathFlavor::Posix => "/",
            PathFlavor::Win32 => "\\",
        }
    }
}

#[cfg(test)]
mod golden {
    //! 与 Node 的对拍表：`tests/fixtures/path_corpus.json` 由 Node 26 生成
    //! （`process.cwd` 被覆盖为 `/cwd`，见 `spec/env-evals-durable-testing.md` §6.2 第 1 条、§8.3 第 1 条）。
    //!
    //! fixture 结构：`{ "cwd": "/cwd", "cases": [ { "flavor": "posix"|"win32",
    //! "kind": "<函数名>", "cases": [ { "args": [...], "expected": ... } ] } ] }`。
    //! 每个 (flavor, kind) 一组，共 16 组；`isAbsolute` 的期望是布尔值，其余是字符串；
    //! `basenameSuffix` 组的 `args[1]` 是后缀。`cwd` 固定为 `/cwd`，所以 resolve/relative 的
    //! 期望值与运行测试的机器无关，这正是各 `*_with_cwd` 变体存在的目的。

    use super::{posix, win32};
    use serde_json::Value;

    fn corpus() -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/path_corpus.json");
        let text = std::fs::read_to_string(&path).expect("read path_corpus.json");
        serde_json::from_str(&text).expect("parse path_corpus.json")
    }

    fn strings(value: &Value) -> Vec<&str> {
        value
            .as_array()
            .expect("args array")
            .iter()
            .map(|item| item.as_str().expect("string argument"))
            .collect()
    }

    #[test]
    fn node_path_corpus() {
        let corpus = corpus();
        let cwd = corpus["cwd"].as_str().expect("cwd");
        // 遍历 fixture 的每一组 (flavor, kind)，逐例比对；`label` 带进断言消息，失败时能直接定位
        // 是哪个 flavor 的哪个函数、哪组参数。
        let mut checked = 0usize;
        for group in corpus["cases"].as_array().expect("groups") {
            let flavor = group["flavor"].as_str().expect("flavor");
            let kind = group["kind"].as_str().expect("kind");
            for case in group["cases"].as_array().expect("cases") {
                let args = strings(&case["args"]);
                let expected = &case["expected"];
                let label = format!("{flavor} {kind} {args:?}");
                if kind == "isAbsolute" {
                    // isAbsolute 返回 bool，其余函数返回字符串，两者的期望值形状不同，分开比较。
                    let actual = match flavor {
                        "posix" => posix::is_absolute(args[0]),
                        _ => win32::is_absolute(args[0]),
                    };
                    assert_eq!(Some(actual), expected.as_bool(), "{label}");
                } else {
                    let actual: String = match (flavor, kind) {
                        ("posix", "normalize") => posix::normalize(args[0]),
                        ("posix", "dirname") => posix::dirname(args[0]),
                        ("posix", "basename") => posix::basename(args[0], None),
                        ("posix", "basenameSuffix") => posix::basename(args[0], Some(args[1])),
                        ("posix", "join") => posix::join(&args),
                        ("posix", "relative") => posix::relative_with_cwd(args[0], args[1], cwd),
                        ("posix", "resolve") => posix::resolve_string_with_cwd(&args, cwd),
                        ("win32", "normalize") => win32::normalize(args[0]),
                        ("win32", "dirname") => win32::dirname(args[0]),
                        ("win32", "basename") => win32::basename(args[0], None),
                        ("win32", "basenameSuffix") => win32::basename(args[0], Some(args[1])),
                        ("win32", "join") => win32::join(&args),
                        // win32 的 resolve/relative 多一个 `is_windows` 参数：它只影响快路径里
                        // 是否把 cwd 的 `/` 换成 `\`（见 win32 模块文档）；语料对拍固定传 false，
                        // 是为了复现 Linux 上生成的期望值。
                        ("win32", "relative") => {
                            win32::relative_with_cwd(args[0], args[1], cwd, false)
                        }
                        ("win32", "resolve") => win32::resolve_string_with_cwd(&args, cwd, false),
                        other => panic!("unknown corpus group {other:?}"),
                    };
                    assert_eq!(
                        actual,
                        expected.as_str().expect("string expectation"),
                        "{label}"
                    );
                }
                checked += 1;
            }
        }
        // 下限而不是精确值：fixture 当前有 7972 个用例，这个阈值只能发现 fixture 被大面积删减
        // 或 `cases` 被解析成空数组；最大的分组也只有 554 例，删掉任意一组后仍剩 7418 例以上，
        // 断言照常通过——单个分组的缺失不会被这个下限发现。
        assert!(checked > 7000, "checked only {checked} cases");
    }
}
