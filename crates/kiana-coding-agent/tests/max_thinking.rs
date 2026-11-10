//! `test/max-thinking.test.ts` 的 CLI 部分。
//!
//! 原文件还断言 `SettingsManager` 接受 `max` 与旧主题回退 `thinkingXhigh`；那两部分属于
//! kiana-config / kiana-theme 单元，已在各自 crate 覆盖，这里只保留 `isValidThinkingLevel("max")`。

use kiana_coding_agent::cli::args::is_valid_thinking_level;

#[test]
fn max_thinking_level_is_accepted_by_cli() {
    assert!(is_valid_thinking_level("max"));
}
