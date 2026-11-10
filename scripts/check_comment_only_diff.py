#!/usr/bin/env python3
"""校验注释清理只改了注释，没有改 Rust 代码。

用法：
    python3 scripts/check_comment_only_diff.py [--baseline /tmp/kiana-comment-baseline]

做法：对基线目录里的每个 `.rs` 文件，去掉 //、///、//!、/* */ 注释后与当前文件比较。
字符串、raw string 里的 `//` 不会被误删。Markdown/TOML 不参与代码比较。
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def char_literal_end(text: str, start: int) -> int | None:
    """`start` 指向字符字面量的左单引号；返回右单引号之后的下标。

    返回 `None` 表示这里不是字符字面量（生命周期标注 `'a`、循环标签 `'outer:` 等）。
    正确识别 `b'"'`、`'\\''`、`'\\u{1F600}'` 这类字面量，避免把里面的双引号
    当成字符串起点，导致后续注释被误判为代码。
    """
    index = start + 1
    if index >= len(text):
        return None
    if text[index] == "\\":
        index += 1
        if index + 1 < len(text) and text[index] == "u" and text[index + 1] == "{":
            close = text.find("}", index + 2)
            if close < 0:
                return None
            index = close + 1
        elif index + 2 < len(text) and text[index] == "x":
            index += 3
        else:
            index += 1
    else:
        index += 1
    if index < len(text) and text[index] == "'":
        return index + 1
    return None


def strip_rust_comments(text: str) -> str:
    output: list[str] = []
    index = 0
    length = len(text)
    depth = 0

    while index < length:
        char = text[index]
        pair = text[index : index + 2]

        if depth > 0:
            if pair == "/*":
                depth += 1
                index += 2
                continue
            if pair == "*/":
                depth -= 1
                index += 2
                continue
            if char == "\n":
                output.append("\n")
            index += 1
            continue

        if pair == "//":
            newline = text.find("\n", index)
            if newline < 0:
                break
            output.append("\n")
            index = newline + 1
            continue

        if pair == "/*":
            depth = 1
            index += 2
            continue

        # char literal: 'x' / '\n' / '\u{...}' / b'x'
        if char == "'":
            end = char_literal_end(text, index)
            if end is not None:
                output.append(text[index:end])
                index = end
                continue
        if char == "b" and index + 1 < length and text[index + 1] == "'":
            end = char_literal_end(text, index + 1)
            if end is not None:
                output.append(text[index:end])
                index = end
                continue

        # raw string: r"..." / r#"..."# / br#"..."#
        if char in {"r", "b"}:
            probe = index + 1
            if char == "b" and probe < length and text[probe] == "r":
                probe += 1
            hashes = 0
            while probe < length and text[probe] == "#":
                hashes += 1
                probe += 1
            if probe < length and text[probe] == '"':
                output.append(text[index : probe + 1])
                probe += 1
                terminator = '"' + "#" * hashes
                end = text.find(terminator, probe)
                if end < 0:
                    output.append(text[probe:])
                    break
                output.append(text[probe : end + len(terminator)])
                index = end + len(terminator)
                continue

        if char == '"':
            # normal string
            output.append(char)
            index += 1
            while index < length:
                current = text[index]
                output.append(current)
                if current == "\\" and index + 1 < length:
                    output.append(text[index + 1])
                    index += 2
                    continue
                index += 1
                if current == '"':
                    break
            continue

        output.append(char)
        index += 1

    return "".join(output)


def normalize(code: str) -> list[str]:
    return [line.rstrip() for line in code.splitlines() if line.rstrip()]


def compare_file(baseline: Path, current: Path) -> str | None:
    try:
        before = strip_rust_comments(baseline.read_text(encoding="utf-8", errors="replace"))
        after = strip_rust_comments(current.read_text(encoding="utf-8", errors="replace"))
    except OSError as error:
        return f"read failed: {error}"

    before_lines = normalize(before)
    after_lines = normalize(after)
    if before_lines == after_lines:
        return None

    # 找出第一处差异，便于定位。
    for number, (left, right) in enumerate(zip(before_lines, after_lines), start=1):
        if left != right:
            return f"code changed near normalized line {number}: {left!r} -> {right!r}"
    return f"code line count changed: {len(before_lines)} -> {len(after_lines)}"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", default="/tmp/kiana-comment-baseline")
    parser.add_argument(
        "--exclude",
        action="append",
        default=[],
        help="repo-relative path to skip (repeatable); use for files changed by another session",
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    excluded = set(args.exclude)
    baseline_root = Path(args.baseline)
    if not baseline_root.exists():
        print(f"baseline not found: {baseline_root}", file=sys.stderr)
        return 2

    issues: list[dict[str, str]] = []
    checked = 0
    for baseline in sorted(baseline_root.rglob("*.rs")):
        rel = baseline.relative_to(baseline_root).as_posix()
        if rel in excluded:
            continue
        current = ROOT / rel
        checked += 1
        if not current.exists():
            issues.append({"path": rel, "reason": "file deleted"})
            continue
        reason = compare_file(baseline, current)
        if reason:
            issues.append({"path": rel, "reason": reason})

    result = {"ok": not issues, "checked_rust_files": checked, "issues": issues}
    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(f"checked_rust_files={checked}")
        print(f"ok={result['ok']}")
        for issue in issues:
            print(f"  {issue['path']}: {issue['reason']}")
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
