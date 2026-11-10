//! `packages/durable/test/nested-images-bench.ts` 的移植占位。
//!
//! TS 基准每个模型 turn 只发一个 `script` 调用，循环嵌套 `read_image` → `classify`，并报告
//! commit 数、墙钟时间、按写入类型统计的逻辑字节、`script` 返回前后/关闭后/重开后的存储大小、
//! 堆占用（运行中每 5 ms 与每次 commit 采样）、嵌套任务记录数与最大记录大小。
//!
//! 它依赖 harness（任务图、嵌套调用文档、`script` 工具）与 storage（JSONL/SQLite）；这些单元
//! 都已合并，但本文件仍是占位：基准属 `spec/durable-rest.md` §8.11 的非目标（功能正确后再做），
//! 因此保留一个可编译的 `#[ignore]` 用例。

#[test]
#[ignore = "benchmark: the nested image benchmark is not ported (see remaining gaps)"]
fn nested_images_benchmark() {
    // 参数形状与 TS 相同：scenario { backend: sqlite|jsonl, variant: inline|reference, count, bytes }，
    // count 缺省时从命令行参数读取（TS 的 `process.argv[2]`）。
}
