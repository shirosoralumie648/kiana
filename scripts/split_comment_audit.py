#!/usr/bin/env python3
"""把注释审计的 A/D 类条目按 crate/子目录拆成工作单，并建立代码基线。

输出：
- docs/internal/comment-audit/work/<group>.md：每个并行 agent 的工作单
- docs/internal/comment-audit/work/index.json：分组索引
- /tmp/kiana-comment-baseline/<path>：编辑前文件副本，供“仅注释改动”检查使用

本脚本只处理 `crates/` 下的代码注释，以及根目录文档；`spec/` 与 `docs/`
不进入自动修改（它们需要整体重写，不是逐行改注释）。
"""

from __future__ import annotations

import csv
import json
import shutil
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
AUDIT_FILES = [
    ROOT / "docs" / "internal" / "comment-audit" / "scan-A.csv",
    ROOT / "docs" / "internal" / "comment-audit" / "scan-D.csv",
]
WORK_DIR = ROOT / "docs" / "internal" / "comment-audit" / "work"
BASELINE = Path("/tmp/kiana-comment-baseline")

ROOT_DOCS = {"ARCHITECTURE.md", "README.md"}
MAX_ROWS_PER_GROUP = 40


@dataclass
class Row:
    path: str
    line: str
    category: str
    matched: str
    excerpt: str
    action: str
    area: str
    crate: str


def load_rows() -> list[Row]:
    rows: list[Row] = []
    for audit_file in AUDIT_FILES:
        with audit_file.open(encoding="utf-8-sig", newline="") as handle:
            for raw in csv.DictReader(handle):
                area = raw["area"]
                path = raw["path"]
                if area == "crates":
                    pass
                elif area == "root" and path in ROOT_DOCS:
                    pass
                else:
                    continue
                rows.append(
                    Row(
                        path=path,
                        line=raw["line"],
                        category=raw["category"],
                        matched=raw["matched"],
                        excerpt=raw["excerpt"],
                        action=raw["suggested_action"],
                        area=area,
                        crate=raw["crate"],
                    )
                )
    return rows


def make_groups(rows: list[Row]) -> dict[str, list[Row]]:
    """按 crate 分桶，每个工作单不超过约 40 条；同一文件不会跨桶。"""
    by_owner: dict[str, list[Row]] = defaultdict(list)
    for row in rows:
        key = "docs-root" if row.area == "root" else row.crate
        by_owner[key].append(row)

    groups: dict[str, list[Row]] = {}
    for owner, owner_rows in by_owner.items():
        if len(owner_rows) <= MAX_ROWS_PER_GROUP:
            groups[owner] = owner_rows
            continue

        by_file: dict[str, list[Row]] = defaultdict(list)
        for row in owner_rows:
            by_file[row.path].append(row)

        bins: list[list[Row]] = []
        for _path, file_rows in sorted(
            by_file.items(), key=lambda item: (-len(item[1]), item[0])
        ):
            for bucket in bins:
                if len(bucket) + len(file_rows) <= MAX_ROWS_PER_GROUP:
                    bucket.extend(file_rows)
                    break
            else:
                bins.append(list(file_rows))

        for index, bucket in enumerate(bins, start=1):
            name = owner if len(bins) == 1 else f"{owner}--part-{index}"
            groups[name] = bucket
    return dict(sorted(groups.items()))


def render_work(group: str, rows: list[Row]) -> str:
    files = sorted({row.path for row in rows})
    lines: list[str] = []
    lines.append(f"# 注释清理工作单：{group}")
    lines.append("")
    lines.append(f"- 待处理条目：{len(rows)}")
    lines.append(f"- 涉及文件：{len(files)}")
    lines.append("")
    lines.append("## 规则（必须遵守）")
    lines.append("")
    lines.append("1. **只改注释/文档文字，不改代码、字符串字面量、测试断言、类型签名或行为。**")
    lines.append("2. 目标是把“因为 TS 这样，所以必须这样”改成“当前设计的技术原因”，或者删除纯 TS 理由。")
    lines.append("3. 如果注释描述的是用户可见契约（错误文案、字段顺序、落盘格式、ANSI 输出），保留契约，但不要再用 TS 作为权威；改成“当前稳定契约，改动需同步测试”。")
    lines.append("4. 如果注释只是出处（`TS 源：...`），保留出处，把约束性措辞去掉；不要删除整个出处。")
    lines.append("5. D 类没有明确提到 TS：先判断是否仍有独立技术原因；有就保留并补技术原因，没有就删除。")
    lines.append("6. 不要重写无关注释，不要扩大 diff；一个文件尽量只动列出的行附近。")
    lines.append("7. 不要运行 `cargo fmt` / `cargo check` / 测试；统一由 verifier 执行。")
    lines.append("8. 不要修改本工作单或 `docs/internal/comment-audit/scan*.csv`。")
    lines.append("")
    lines.append("## 文件清单")
    lines.append("")
    for path in files:
        lines.append(f"- `{path}`")
    lines.append("")
    lines.append("## 待处理条目")
    lines.append("")
    lines.append("| # | 位置 | 类别 | 命中 | 摘要 | 建议 |")
    lines.append("|---:|---|---|---|---|---|")
    for index, row in enumerate(rows, start=1):
        excerpt = row.excerpt.replace("|", "\\|")
        lines.append(
            f"| {index} | `{row.path}:{row.line}` | {row.category} | {row.matched} | {excerpt} | {row.action} |"
        )
    lines.append("")
    lines.append("## 完成后返回")
    lines.append("")
    lines.append("返回本组改写的条目数、删除的条目数、保留的条目数，以及任何需要人工确认的 D 类条目。")
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    rows = load_rows()
    groups = make_groups(rows)

    if WORK_DIR.exists():
        shutil.rmtree(WORK_DIR)
    WORK_DIR.mkdir(parents=True)

    if BASELINE.exists():
        shutil.rmtree(BASELINE)
    BASELINE.mkdir(parents=True)

    index = []
    for group, group_rows in groups.items():
        work_file = WORK_DIR / f"{group}.md"
        work_file.write_text(render_work(group, group_rows), encoding="utf-8")
        files = sorted({row.path for row in group_rows})
        for rel in files:
            source = ROOT / rel
            target = BASELINE / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        index.append(
            {
                "group": group,
                "work_file": work_file.relative_to(ROOT).as_posix(),
                "rows": len(group_rows),
                "files": files,
            }
        )

    (WORK_DIR / "index.json").write_text(
        json.dumps(index, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(f"rows={len(rows)}")
    print(f"groups={len(groups)}")
    print(f"files={len({row.path for row in rows})}")
    print(f"work_dir={WORK_DIR.relative_to(ROOT)}")
    print(f"baseline={BASELINE}")
    for item in index:
        print(f"  {item['group']:40} rows={item['rows']:3} files={len(item['files']):3}")


if __name__ == "__main__":
    main()
