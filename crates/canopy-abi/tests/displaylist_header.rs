//! Machine-check `include/canopy_displaylist.h` against the live `canopy_abi::displaylist`
//! constants, so the display-list wire format documented for non-Rust renderers can never drift
//! from the Rust serializer that produces it (the same guarantee `protocol_header.rs` gives the
//! op-stream).

use std::collections::BTreeMap;

/// Every `#define CANOPY_DL_* VALUE` in the header (the include guard, which has no value, is
/// skipped), as name -> parsed integer. Values here are plain `<n>u` decimals or `0xNN` hex.
fn header_defines() -> BTreeMap<String, u64> {
    let src = include_str!("../include/canopy_displaylist.h");
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("#define ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if !name.starts_with("CANOPY_DL_") {
            continue;
        }
        let value = value.split("/*").next().unwrap_or("").trim();
        if value.is_empty() {
            continue;
        }
        let digits = value.trim_end_matches(['u', 'U', 'l', 'L']);
        let parsed = if let Some(hex) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            u64::from_str_radix(hex, 16).expect("hex literal")
        } else {
            digits.parse::<u64>().expect("decimal literal")
        };
        out.insert(name.to_string(), parsed);
    }
    out
}

#[test]
fn displaylist_header_matches_the_rust_constants() {
    use canopy_abi::displaylist as dl;

    let expected: BTreeMap<&str, u64> = BTreeMap::from([
        ("CANOPY_DL_VERSION", u64::from(dl::DL_VERSION)),
        ("CANOPY_DL_RECT", u64::from(dl::DL_RECT)),
        ("CANOPY_DL_GLYPHS", u64::from(dl::DL_GLYPHS)),
        ("CANOPY_DL_TEXT", u64::from(dl::DL_TEXT)),
        ("CANOPY_DL_BORDER", u64::from(dl::DL_BORDER)),
        ("CANOPY_DL_GRADIENT", u64::from(dl::DL_GRADIENT)),
        ("CANOPY_DL_SHADOW", u64::from(dl::DL_SHADOW)),
        ("CANOPY_DL_PUSH_CLIP", u64::from(dl::DL_PUSH_CLIP)),
        ("CANOPY_DL_POP_CLIP", u64::from(dl::DL_POP_CLIP)),
        ("CANOPY_DL_DIR_VERTICAL", u64::from(dl::DL_DIR_VERTICAL)),
        ("CANOPY_DL_DIR_HORIZONTAL", u64::from(dl::DL_DIR_HORIZONTAL)),
    ]);

    let header = header_defines();

    for (name, want) in &expected {
        let got = header
            .get(*name)
            .unwrap_or_else(|| panic!("header is missing #define {name}"));
        assert_eq!(got, want, "#define {name} drifted from the Rust constant");
    }
    for name in header.keys() {
        assert!(
            expected.contains_key(name.as_str()),
            "header defines {name} but the parity test does not check it"
        );
    }
}
