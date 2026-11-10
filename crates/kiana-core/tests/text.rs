//! `utils/text.ts` 的移植测试（TS 无对应测试文件，按 §2.21.2 的契约新写）。

use kiana_core::text::{BomSplit, split_bom, strip_bom};

#[test]
fn splits_a_leading_bom() {
    let split = split_bom("\u{FEFF}hello");
    assert_eq!(
        split,
        BomSplit {
            bom: "\u{FEFF}".to_string(),
            text: "hello".to_string()
        }
    );
    assert_eq!(strip_bom("\u{FEFF}hello"), "hello");
}

#[test]
fn leaves_text_without_bom_unchanged() {
    assert_eq!(
        split_bom("hello"),
        BomSplit {
            bom: String::new(),
            text: "hello".to_string()
        }
    );
    assert_eq!(strip_bom("hello"), "hello");
}

#[test]
fn strips_only_one_bom() {
    assert_eq!(strip_bom("\u{FEFF}\u{FEFF}x"), "\u{FEFF}x");
}

#[test]
fn handles_empty_and_bom_only_inputs() {
    assert_eq!(split_bom("").bom, "");
    assert_eq!(split_bom("").text, "");
    assert_eq!(strip_bom("\u{FEFF}"), "");
}
