//! `cli/list-models.ts`（115 行）：`kiana --list-models` 的表格输出。
//!
//! 列宽按 UTF-16 码元（`padEnd`）计算，列间两个空格；排序用 `localeCompare`。
//!
//! # 为什么需要
//! `--list-models` 要在不启动会话/TUI 的情况下回答“当前配置下有哪些可用模型”，供人阅读
//! 也方便脚本 grep。数据来自 [`ModelRuntime::get_available`]，即运行时维护的可用模型快照；
//! 一个模型都没有时打印配置引导文案而不是空表。
//!
//! # 输出格式
//! - 六列：`provider`、`model`、`context`、`max-out`、`thinking`、`images`；列宽取表头与
//!   该列所有单元格的 UTF-16 码元数最大值（等价 JS 的 `String.length`/`padEnd`，emoji
//!   按代理对计 2），列间固定两个空格。
//! - 排序先按 provider 做 `localeCompare`，同一 provider 再按 model id；fuzzy 过滤阶段
//!   按分数产生的顺序随后被这次稳定排序重排。
//! - `context`/`max-out` 缺失时按 0 显示；`thinking` 看 `reasoning`，`images` 看 `input`
//!   是否包含 [`ModelInputModality::Image`]。
//!
//! # 错误与输出通道
//! - `models.json` 的加载/合成错误不会中止：先以黄色 `Warning:` 前缀写 stderr，再继续列表。
//! - [`ModelRuntime::get_available`] 返回的 [`AuthError`] 原样上抛；调用方打印
//!   `Error: …` 并以退出码 1 结束。
//! - 表头/数据/引导文案走 stdout，警告走 stderr；都经由 [`Output`]。
//! - 调用方是 `crate::app::main` 的 `--list-models` 分支，它传入 15 秒的取消作用域。
//!
//! TS: `packages/coding-agent/src/cli/list-models.ts`

use kiana_ai::auth::types::AuthError;
use kiana_ai::types::{Model, ModelInputModality};
use kiana_ai::utils::abort::AbortScope;
use kiana_config::auth_guidance::format_no_models_available_message;
use kiana_config::models::runtime::ModelRuntime;
use kiana_tui::autocomplete::locale_compare;
use kiana_tui::fuzzy::fuzzy_filter;

use crate::cli::args::pad_end_utf16;
use crate::cli::setup::{Output, Style};

/// `formatTokenCount(count)`（`1000000 → "1M"`、`1500000 → "1.5M"`、`200000 → "200K"`）。
///
/// 先按原值选百万元/千元分支，再决定是否补一位小数：因此 `999_999` 得到 `"1000.0K"`、
/// `999_999_999` 得到 `"1000.0M"`，不会进位成 `"1M"`/`"1G"`。小数部分用
/// `js_to_fixed(…, 1)`（JS `toFixed(1)`，逢半远离零），不能用 Rust 的 `{:.1}` 替代。
///
/// `count` 先转成 `f64`：超过 2^53 的 u64 在这个转换里会损失精度。
pub fn format_token_count(count: u64) -> String {
    let count = count as f64;
    if count >= 1_000_000.0 {
        let millions = count / 1_000_000.0;
        if millions.fract() == 0.0 {
            format!("{millions}M")
        } else {
            format!("{}M", kiana_ai::model_catalog::js_to_fixed(millions, 1))
        }
    } else if count >= 1_000.0 {
        let thousands = count / 1_000.0;
        if thousands.fract() == 0.0 {
            format!("{thousands}K")
        } else {
            format!("{}K", kiana_ai::model_catalog::js_to_fixed(thousands, 1))
        }
    } else {
        format!("{count}")
    }
}

/// 表格的一行（表头也复用一个 `Row`，只是文案不同）。
struct Row {
    provider: String,
    model: String,
    context: String,
    max_out: String,
    thinking: String,
    images: String,
}

/// `listModels(modelRuntime, searchPattern?, signal?)`。
///
/// 输出写到进程通道（[`Output::process`]）；测试可用 [`list_models_with_output`] 捕获。
///
/// # 错误
/// 只有 [`ModelRuntime::get_available`] 的 [`AuthError`] 会向上返回；“加载 models.json
/// 失败”“无模型”“无匹配”都只写输出并返回 `Ok(())`。
pub async fn list_models(
    model_runtime: &ModelRuntime,
    search_pattern: Option<&str>,
    signal: Option<AbortScope>,
) -> Result<(), AuthError> {
    list_models_with_output(model_runtime, search_pattern, signal, &Output::process()).await
}

/// 指定 [`Output`] 的 [`list_models`]。
///
/// # 参数
/// - `search_pattern`：`None` 或空串表示不过滤；非空时用 `fuzzy_filter` 对
///   `"<provider> <id>"` 做匹配（空白或 `/` 分隔的 token 必须全部命中），之后仍会按
///   provider/model 排序。
/// - `signal`：透传给 `get_available`，用于取消可用性刷新。
pub async fn list_models_with_output(
    model_runtime: &ModelRuntime,
    search_pattern: Option<&str>,
    signal: Option<AbortScope>,
    output: &Output,
) -> Result<(), AuthError> {
    let style = Style::detect();

    // models.json 的加载/合成错误只作为警告：TS 仍然展示能加载到的模型，不中止命令。
    if let Some(load_error) = model_runtime.get_error() {
        output.write_line_stderr(&style.yellow(&format!(
            "Warning: errors loading models.json:\n{load_error}"
        )));
    }

    let models = model_runtime
        .get_available(None, kiana_ai::auth::types::AuthOperationOptions { signal })
        .await?;

    if models.is_empty() {
        output.write_line_stdout(&format_no_models_available_message());
        return Ok(());
    }

    // TS 的 `if (searchPattern)`：空串是 falsy，与未给出一样不过滤；只有真非空才做
    // fuzzy 过滤（纯空白串会进入 fuzzy_filter，而它 trim 后为空，原样返回）。
    let search_pattern = search_pattern.filter(|pattern| !pattern.is_empty());
    let mut filtered_models: Vec<Model> = match search_pattern {
        Some(pattern) => fuzzy_filter(models, pattern, |model| {
            format!("{} {}", model.provider, model.id)
        }),
        None => models,
    };

    if filtered_models.is_empty() {
        output.write_line_stdout(&format!(
            "No models matching \"{}\"",
            search_pattern.unwrap_or_default()
        ));
        return Ok(());
    }

    // `localeCompare` 语义（运行时 locale 的 ICU collation），provider 为主序、id 为次序。
    filtered_models.sort_by(|left, right| {
        locale_compare(&left.provider, &right.provider)
            .then_with(|| locale_compare(&left.id, &right.id))
    });

    let rows: Vec<Row> = filtered_models
        .iter()
        .map(|model| Row {
            provider: model.provider.clone(),
            model: model.id.clone(),
            context: format_token_count(model.context_window.unwrap_or(0)),
            max_out: format_token_count(model.max_tokens.unwrap_or(0)),
            thinking: if model.reasoning.unwrap_or(false) {
                "yes".to_string()
            } else {
                "no".to_string()
            },
            images: if model.input.contains(&ModelInputModality::Image) {
                "yes".to_string()
            } else {
                "no".to_string()
            },
        })
        .collect();

    let headers = Row {
        provider: "provider".to_string(),
        model: "model".to_string(),
        context: "context".to_string(),
        max_out: "max-out".to_string(),
        thinking: "thinking".to_string(),
        images: "images".to_string(),
    };

    // 列宽 = 表头与该列所有值的 UTF-16 码元数最大值；用码元而不是字符/显示宽度，
    // 是为了复刻 JS `padEnd(str.length)` 在 emoji 等代理对上的表现。
    let width = |header: &str, values: &[&str]| -> usize {
        values
            .iter()
            .map(|value| value.encode_utf16().count())
            .chain(std::iter::once(header.encode_utf16().count()))
            .max()
            .unwrap_or(0)
    };
    let provider_width = width(
        &headers.provider,
        &rows
            .iter()
            .map(|row| row.provider.as_str())
            .collect::<Vec<_>>(),
    );
    let model_width = width(
        &headers.model,
        &rows
            .iter()
            .map(|row| row.model.as_str())
            .collect::<Vec<_>>(),
    );
    let context_width = width(
        &headers.context,
        &rows
            .iter()
            .map(|row| row.context.as_str())
            .collect::<Vec<_>>(),
    );
    let max_out_width = width(
        &headers.max_out,
        &rows
            .iter()
            .map(|row| row.max_out.as_str())
            .collect::<Vec<_>>(),
    );
    let thinking_width = width(
        &headers.thinking,
        &rows
            .iter()
            .map(|row| row.thinking.as_str())
            .collect::<Vec<_>>(),
    );
    let images_width = width(
        &headers.images,
        &rows
            .iter()
            .map(|row| row.images.as_str())
            .collect::<Vec<_>>(),
    );

    // 六个单元格都按各自列宽补空格，再用两个空格连接；表头与数据行走同一个闭包，
    // 保证列对齐一致。
    let line = |provider: &str,
                model: &str,
                context: &str,
                max_out: &str,
                thinking: &str,
                images: &str|
     -> String {
        [
            pad_end_utf16(provider, provider_width),
            pad_end_utf16(model, model_width),
            pad_end_utf16(context, context_width),
            pad_end_utf16(max_out, max_out_width),
            pad_end_utf16(thinking, thinking_width),
            pad_end_utf16(images, images_width),
        ]
        .join("  ")
    };

    output.write_line_stdout(&line(
        &headers.provider,
        &headers.model,
        &headers.context,
        &headers.max_out,
        &headers.thinking,
        &headers.images,
    ));
    for row in &rows {
        output.write_line_stdout(&line(
            &row.provider,
            &row.model,
            &row.context,
            &row.max_out,
            &row.thinking,
            &row.images,
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_token_count_matches_js_to_fixed_semantics() {
        // 期望值由 Node 26 直接计算（`toFixed(1)` 的半数行为与 Rust 格式化不同）。
        for (count, expected) in [
            (0, "0"),
            (999, "999"),
            (1000, "1K"),
            (1234, "1.2K"),
            (1500, "1.5K"),
            (1999, "2.0K"),
            (2000, "2K"),
            (999_999, "1000.0K"),
            (1_000_000, "1M"),
            (1_500_000, "1.5M"),
            (1_234_567, "1.2M"),
            (1_999_999, "2.0M"),
            (2_500_000, "2.5M"),
            (100_000_000, "100M"),
            (999_999_999, "1000.0M"),
        ] {
            assert_eq!(format_token_count(count), expected, "{count}");
        }
    }
}
