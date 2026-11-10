//! `experimental/source-resolver.ts`：Node 模块解析钩子（**documentation-only**）。
//!
//! TS 版读取仓库根 `tsconfig.json` 的 `compilerOptions.paths`，只保留
//! `@earendil-works/` 前缀的 pattern，并通过 `registerHooks({resolve})` 把
//! `@earendil-works/<pkg>` 映射到源码 `.ts` 文件，使 `node --experimental-strip-types`
//! 能直接运行仓库源码。错误文案（逐字）：
//!
//! - `Source runtime requires compilerOptions.paths in ${tsconfigPath}`
//! - `Source runtime does not support multiple wildcards in ${pattern}`
//! - `Source runtime could not resolve ${specifier} through tsconfig path ${matchedPattern}`
//!
//! Rust 版本编译期就固定了 crate 边界，不需要运行时模块解析器；按 spec/ca-cli.md §2.20
//! 保留本模块仅作文档。源码直跑脚本由 Node 侧的 `--import ./src/experimental/source-resolver.ts`
//! 负责（只用于参考仓库，不参与 Rust 构建）。
//!
//! # 为什么 Rust 侧不需要实现
//!
//! Rust 的模块解析在编译期完成：crate 之间的依赖写在 `Cargo.toml`，符号由
//! `use kiana_x::…` 在编译期解析，不存在运行期按 tsconfig `paths` 改写的等价需求。
//! 因此本文件没有函数、常量或类型，`lib.rs` 里的 `pub mod source_resolver;` 只是保留
//! TS 侧的文档锚点，模块里没有可调用条目（rustdoc 页面只有这段说明）；仓库内除
//! `pub mod` 声明外没有引用它的代码，不要在这里补一个“近似”的解析器。
//!
//! # 参考实现的行为（仅用于对照 TS）
//!
//! 逐字错误文案见上。解析流程概括：
//!
//! 1. 读取仓库根 `tsconfig.json`（相对本文件 `../../../..`）；缺少
//!    `compilerOptions.paths` 立即报错；
//! 2. 只保留 key 以 `@earendil-works/` 开头的 pattern，按 pattern 长度降序依次尝试；
//! 3. pattern 含多个 `*` 视为不支持的配置并报错；
//! 4. 命中 pattern 但没有任何 replacement 解析到仓库内存在的文件（通常为 `.ts`，也接受
//!    `.mts`/`.cts`/`.json` 及无扩展名的 `base`/`base.ts`/`index.ts` 候选，见
//!    `spec/ca-cli.md` §2.20）时，报“could not resolve”；只有全部 pattern 都不匹配时才
//!    交回 `nextResolve`。
//!
//! 细节（扩展名映射、仓库根越界检查、无 `*` 的全等匹配等）以 `spec/ca-cli.md` §2.20
//! 与 TS 源为准，本文件不复刻代码。
