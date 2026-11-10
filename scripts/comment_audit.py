#!/usr/bin/env python3
"""扫描全仓注释里与 TypeScript 参照实现相关的表述，生成分类审计清单。

输出：
- docs/internal/comment-audit/scan.csv：逐行清单（UTF-8 BOM，便于表格软件打开）
- docs/internal/comment-audit/README.md：汇总、分类说明与高优先级样本

分类：
- A 约束型：把“与 TS 一致/逐字一致/保真”当作硬约束，最需要改写或删除。
- B 功能说明：提到 TS 的行为/格式/顺序，随对应代码替换时更新。
- C 历史来源：仅记录出处，建议保留但降级为“非约束”。
- D 待人工判断：出现“必须一致/逐字”等强约束，但没有明确提到 TS。

注意：这是启发式扫描，不做完整 Rust 词法分析；字符串里的 `//`、URL、
代码块中的示例可能造成少量误报，A 类仍建议逐条人工确认。
"""

from __future__ import annotations

import csv
import re
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT_CSV = ROOT / "docs" / "internal" / "comment-audit" / "scan.csv"
OUT_A_CSV = ROOT / "docs" / "internal" / "comment-audit" / "scan-A.csv"
OUT_D_CSV = ROOT / "docs" / "internal" / "comment-audit" / "scan-D.csv"
OUT_MD = ROOT / "docs" / "internal" / "comment-audit" / "README.md"

SKIP_DIRS = {
    "reference_project",
    "target",
    ".git",
    "node_modules",
    ".cargo",
    "audit-work",
    "internal",
}
SKIP_FILES = {
    # 由工作流 journal 汇总生成，内容不是代码注释，且会淹没真正的清单。
    "docs/UNVERIFIED.md",
    # 本次审计自身的输出。
    "docs/internal/comment-audit/scan.csv",
    "docs/internal/comment-audit/scan-A.csv",
    "docs/internal/comment-audit/scan-D.csv",
    "docs/internal/comment-audit/README.md",
    "docs/internal/comment-audit/reports/cleanup.md",
    "docs/internal/comment-audit/unexpected-code.patch",
}

# 只扫这些后缀；Rust 注释 + 文档 + Cargo 注释。
SUFFIXES = {".rs", ".md", ".toml"}

CONSTRAINT_PATTERNS = [
    ("必须与TS", re.compile(r"必须(与|和|同)\s*ts(?![A-Za-z])", re.I)),
    ("与TS一致", re.compile(r"(与|和|同)\s*ts\s*(完全)?一致", re.I)),
    ("与TS保持", re.compile(r"(与|和|同)\s*ts\s*保持", re.I)),
    ("逐字一致", re.compile(r"逐字(一致|保持|保留|对应|相同|文本)")),
    ("逐字节一致", re.compile(r"逐字节一致")),
    ("逐位一致", re.compile(r"逐位一致")),
    ("完全一致", re.compile(r"完全一致")),
    ("保真", re.compile(r"保真")),
    ("兼容面", re.compile(r"兼容面")),
    ("TS等价物", re.compile(r"ts\s*的等价物|等价于\s*ts(?![A-Za-z])", re.I)),
    ("强约束", re.compile(r"必须一致|必须相同|不能改变|不得改变")),
]

HISTORY_PATTERNS = [
    ("TS源", re.compile(r"(?<![A-Za-z])ts\s*源", re.I)),
    ("TS的", re.compile(r"(?<![A-Za-z])ts\s*的", re.I)),
    ("TS里", re.compile(r"(?<![A-Za-z])ts\s*里", re.I)),
    ("TS:", re.compile(r"(?<![A-Za-z])ts\s*:", re.I)),
    ("对应TS", re.compile(r"对应\s*ts(?![A-Za-z])", re.I)),
    ("上游TS", re.compile(r"上游\s*ts(?![A-Za-z])", re.I)),
    ("从TS", re.compile(r"从\s*ts(?![A-Za-z])", re.I)),
    ("TS版本", re.compile(r"(?<![A-Za-z])ts\s*版本", re.I)),
    ("TS代码", re.compile(r"(?<![A-Za-z])ts\s*代码", re.I)),
    ("packages/", re.compile(r"packages/", re.I)),
    ("参照/母本", re.compile(r"参照实现|母本|移植自|历史来源")),
]

TS_WORD = re.compile(r"(?<![A-Za-z])TS(?![A-Za-z])", re.I)

FUNCTIONAL_HINTS = [
    "顺序",
    "格式",
    "文案",
    "行为",
    "等价",
    "兼容",
    "字段",
    "字节",
    "ansi",
    "utf-16",
    "键序",
    "事件顺序",
    "错误",
    "默认值",
    "超时",
]

ACTION = {
    "A": "改写为当前设计约束（写清技术原因），或删除；不要再用 TS 作为理由",
    "B": "随对应代码替换时更新；保留可验证的技术原因",
    "C": "保留为历史来源；建议统一标为“来源（历史）”并注明非约束",
    "D": "人工判断：确认是否仍有独立技术原因；没有则删除",
}

PRIORITY = {"A": "high", "B": "low", "C": "low", "D": "medium"}


@dataclass
class Finding:
    path: str
    line: int
    area: str
    crate: str
    kind: str
    category: str
    matched: str
    excerpt: str
    action: str
    priority: str
    is_test: bool


def comment_text(line: str, suffix: str) -> str | None:
    """取出该行的注释部分；没有注释返回 None。"""
    if suffix == ".rs":
        # 找第一个 //；字符串里的 // 可能被误判，但审计用途可接受。
        index = line.find("//")
        if index < 0:
            return None
        return line[index:]
    if suffix == ".toml":
        index = line.find("#")
        if index < 0:
            return None
        return line[index:]
    # Markdown：整行都算可审计文本。
    return line


def classify(text: str) -> tuple[str, list[str]] | None:
    lower = text.lower()
    has_ts = (
        TS_WORD.search(text) is not None
        or "typescript" in lower
        or "参照" in text
        or "上游" in text
        or "母本" in text
        or "packages/" in lower
    )

    constraint = [name for name, pattern in CONSTRAINT_PATTERNS if pattern.search(text)]
    history = [name for name, pattern in HISTORY_PATTERNS if pattern.search(text)]

    if constraint and has_ts:
        matched = list(dict.fromkeys(constraint + history))
        return "A", matched
    if constraint and not has_ts:
        return "D", constraint
    if history:
        return "C", history
    if has_ts and any(hint in lower for hint in FUNCTIONAL_HINTS):
        matched = [hint for hint in FUNCTIONAL_HINTS if hint in lower][:3]
        return "B", matched
    if has_ts:
        return "B", ["TS"]
    return None


def area_of(path: str) -> str:
    first = path.split("/", 1)[0]
    if first in {"crates", "docs", "spec"}:
        return first
    return "root"


def crate_of(path: str) -> str:
    parts = path.split("/")
    if len(parts) >= 3 and parts[0] == "crates":
        return parts[1]
    return ""


def kind_of(comment: str, suffix: str) -> str:
    if suffix == ".md":
        return "markdown"
    if suffix == ".toml":
        return "toml"
    stripped = comment.lstrip()
    if stripped.startswith("///") or stripped.startswith("//!"):
        return "rust-doc"
    return "rust-line"


def is_test_path(path: str) -> bool:
    parts = path.split("/")
    return "tests" in parts or "benches" in parts or "/test" in path


def iter_files():
    for path in sorted(ROOT.rglob("*")):
        if not path.is_file() or path.suffix not in SUFFIXES:
            continue
        rel = path.relative_to(ROOT).as_posix()
        if any(part in SKIP_DIRS for part in path.relative_to(ROOT).parts):
            continue
        if rel in SKIP_FILES:
            continue
        yield path, rel


def scan() -> list[Finding]:
    findings: list[Finding] = []
    for path, rel in iter_files():
        try:
            lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            continue
        for number, line in enumerate(lines, start=1):
            comment = comment_text(line, path.suffix)
            if not comment:
                continue
            result = classify(comment)
            if result is None:
                continue
            category, matched = result
            excerpt = " ".join(comment.strip().split())
            if len(excerpt) > 220:
                excerpt = excerpt[:217] + "…"
            findings.append(
                Finding(
                    path=rel,
                    line=number,
                    area=area_of(rel),
                    crate=crate_of(rel),
                    kind=kind_of(comment, path.suffix),
                    category=category,
                    matched=", ".join(dict.fromkeys(matched)),
                    excerpt=excerpt,
                    action=ACTION[category],
                    priority=PRIORITY[category],
                    is_test=is_test_path(rel),
                )
            )
    return findings


def write_csv(findings: list[Finding], target: Path) -> None:
    target.parent.mkdir(parents=True, exist_ok=True)
    with target.open("w", encoding="utf-8-sig", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(
            [
                "category",
                "priority",
                "area",
                "crate",
                "path",
                "line",
                "kind",
                "is_test",
                "matched",
                "excerpt",
                "suggested_action",
            ]
        )
        for item in findings:
            writer.writerow(
                [
                    item.category,
                    item.priority,
                    item.area,
                    item.crate,
                    item.path,
                    item.line,
                    item.kind,
                    "yes" if item.is_test else "no",
                    item.matched,
                    item.excerpt,
                    item.action,
                ]
            )


def write_markdown(findings: list[Finding], files_scanned: int) -> None:
    by_category = Counter(item.category for item in findings)
    by_area = Counter(item.area for item in findings)
    by_crate = Counter(item.crate for item in findings if item.crate)
    a_items = [item for item in findings if item.category == "A"]
    d_items = [item for item in findings if item.category == "D"]
    top_files = Counter(item.path for item in a_items)
    top_crates = Counter(item.crate for item in a_items if item.crate)

    lines: list[str] = []
    lines.append("# 全仓注释审计清单")
    lines.append("")
    lines.append(
        "本清单扫描 Rust 注释、Markdown 文档与 Cargo 注释中与 TypeScript 参照实现相关的表述，"
        "用于清理“TS 保真”这一已过时的前提。"
    )
    lines.append("")
    lines.append(f"- 扫描文件：{files_scanned}")
    lines.append(f"- 命中行：{len(findings)}")
    lines.append(f"- 完整逐行清单：[`scan.csv`](scan.csv)")
    lines.append(f"- 可直接处理：A 类 [`scan-A.csv`](scan-A.csv)、D 类 [`scan-D.csv`](scan-D.csv)")
    lines.append("")
    lines.append("## 分类口径")
    lines.append("")
    lines.append("| 类别 | 含义 | 建议动作 |")
    lines.append("|---|---|---|")
    lines.append(
        "| A | 把“与 TS 一致/逐字一致/保真”当作硬约束 | 改写为当前设计约束，或删除；不要再用 TS 作为理由 |"
    )
    lines.append(
        "| B | 提到 TS 的行为/格式/顺序，但主要是功能说明 | 随对应代码替换时更新；保留可验证的技术原因 |"
    )
    lines.append(
        "| C | 仅记录 TS 出处 | 保留，降级为“历史来源（非约束）” |"
    )
    lines.append(
        "| D | 有“必须一致/逐字”等强约束，但没有明确提到 TS | 人工判断是否仍有独立技术原因 |"
    )
    lines.append("")
    lines.append("## 总量")
    lines.append("")
    lines.append("| 类别 | 行数 |")
    lines.append("|---|---:|")
    for category in ["A", "B", "C", "D"]:
        lines.append(f"| {category} | {by_category.get(category, 0)} |")
    lines.append("")
    lines.append("| 区域 | 行数 |")
    lines.append("|---|---:|")
    for area, count in by_area.most_common():
        lines.append(f"| {area} | {count} |")
    lines.append("")
    lines.append("## A 类：按 crate")
    lines.append("")
    lines.append("| crate | A 类行数 |")
    lines.append("|---|---:|")
    for crate, count in top_crates.most_common(25):
        lines.append(f"| `{crate}` | {count} |")
    lines.append("")
    lines.append("## A 类：按文件（前 30）")
    lines.append("")
    lines.append("| 文件 | A 类行数 |")
    lines.append("|---|---:|")
    for path, count in top_files.most_common(30):
        lines.append(f"| `{path}` | {count} |")
    lines.append("")
    lines.append("## 建议清理批次")
    lines.append("")
    lines.append(
        "1. **中央文档与规范**：`ARCHITECTURE.md`、`README.md`、`docs/START_HERE.md`、`docs/EXECUTION_FLOW.md`、`spec/*.md`。先把“行为完全一致”改成“历史移植目标，已废弃”。"
    )
    lines.append(
        "2. **正在改的 crate**：`kiana-permissions`、`kiana-sandbox`、`kiana-session`、`kiana-tools`、`kiana-compaction`。随权限/沙箱/工具替换一起清理。"
    )
    lines.append(
        "3. **高密度 crate**：A 类行数最多的 crate 分批处理；每批只改注释，不夹带代码改动，便于 review。"
    )
    lines.append(
        "4. **测试与黄金文件**：`tests/`、`benches/` 里的 TS 断言先保留；等对应行为真正替换时，再改测试名和快照。"
    )
    lines.append(
        "5. **历史来源**：C 类不删；统一标为“来源（历史，非约束）”，必要时迁移到 `docs/PROVENANCE.md`。"
    )
    lines.append("")
    lines.append("## A 类高优先级样本（前 40 条）")
    lines.append("")
    lines.append("| 位置 | 摘要 |")
    lines.append("|---|---|")
    for item in a_items[:40]:
        excerpt = item.excerpt.replace("|", "\\|")
        lines.append(f"| `{item.path}:{item.line}` | {excerpt} |")
    lines.append("")
    lines.append("## D 类样本（前 20 条）")
    lines.append("")
    lines.append("| 位置 | 摘要 |")
    lines.append("|---|---|")
    for item in d_items[:20]:
        excerpt = item.excerpt.replace("|", "\\|")
        lines.append(f"| `{item.path}:{item.line}` | {excerpt} |")
    lines.append("")
    lines.append("## 使用方式")
    lines.append("")
    lines.append("```bash")
    lines.append("# 只看 A 类")
    lines.append("awk -F, 'NR==1 || $1==\"A\"' docs/internal/comment-audit/scan.csv")
    lines.append("# 只看某个 crate")
    lines.append("awk -F, 'NR==1 || $4==\"kiana-tui\"' docs/internal/comment-audit/scan.csv")
    lines.append("# 只看测试")
    lines.append("awk -F, 'NR==1 || $8==\"yes\"' docs/internal/comment-audit/scan.csv")
    lines.append("```")
    lines.append("")
    lines.append(
        "> 注意：这是启发式扫描，不做完整 Rust 词法分析；字符串里的 `//`、URL、"
        "Markdown 代码块可能造成少量误报。A 类仍建议逐条人工确认。"
    )
    lines.append("")

    OUT_MD.write_text("\n".join(lines), encoding="utf-8")


def main() -> None:
    files_scanned = sum(1 for _ in iter_files())
    findings = scan()
    write_csv(findings, OUT_CSV)
    write_csv([item for item in findings if item.category == "A"], OUT_A_CSV)
    write_csv([item for item in findings if item.category == "D"], OUT_D_CSV)
    write_markdown(findings, files_scanned)

    by_category = Counter(item.category for item in findings)
    print(f"scanned_files={files_scanned}")
    print(f"findings={len(findings)}")
    for category in ["A", "B", "C", "D"]:
        print(f"{category}={by_category.get(category, 0)}")
    print(f"csv={OUT_CSV.relative_to(ROOT)}")
    print(f"csv_A={OUT_A_CSV.relative_to(ROOT)}")
    print(f"csv_D={OUT_D_CSV.relative_to(ROOT)}")
    print(f"markdown={OUT_MD.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
