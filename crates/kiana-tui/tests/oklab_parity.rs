//! `oklab.ts` 的 `oklabToLinearSrgb` 与 Node 26.12.1 **逐位**一致。
//!
//! 黄金数据由 Node 直接生成：`oklabToLinearSrgb([L, a, b])` 的每个 f64 用
//! `DataView.getBigUint64` 写成 16 位十六进制（含 NaN/Inf 的位模式），因此测试比的是
//! **bit pattern**，不受十进制往返误差影响。语料一半是 Oklab 常见取值域
//! （`L ∈ [0,1]`、`a,b ∈ [-0.5,0.5]`），一半是随机位模式。

use serde_json::Value;

fn hex_float(value: &Value) -> f64 {
    let text = value.as_str().expect("hex string");
    f64::from_bits(u64::from_str_radix(text, 16).expect("valid hex bits"))
}

fn triple(value: &Value) -> [f64; 3] {
    let items = value.as_array().expect("array of three");
    [
        hex_float(&items[0]),
        hex_float(&items[1]),
        hex_float(&items[2]),
    ]
}

#[test]
fn oklab_to_linear_srgb_matches_node_bit_for_bit() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/oklab_parity.json")).expect("valid fixture");
    assert!(cases.len() >= 150, "fixture should be non-trivial");

    let mut failures: Vec<String> = Vec::new();
    for case in &cases {
        let lab = triple(&case["lab"]);
        let expected = triple(&case["rgb"]);
        let actual = kiana_tui::oklab::oklab_to_linear_srgb(lab);
        for channel in 0..3 {
            if actual[channel].to_bits() != expected[channel].to_bits() {
                failures.push(format!(
                    "{lab:?}[{channel}]: got {:?} ({:016x}), want {:?} ({:016x})",
                    actual[channel],
                    actual[channel].to_bits(),
                    expected[channel],
                    expected[channel].to_bits()
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} channel values differ from Node:\n{}",
        failures.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
