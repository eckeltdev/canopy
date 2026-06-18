//! Machine-check `include/canopy_protocol.h` against the live Rust constants.
//!
//! The header is the wire contract a non-Rust author (the C++ builder binding) encodes
//! against. If a Rust op tag, handle id, or well-known widget id ever changes and the
//! header is not updated (or vice versa), this test fails — so the contract can never
//! silently drift from the engine.

use std::collections::BTreeMap;

/// Parse a C integer literal as it appears in the header: optional surrounding parens,
/// `u`/`U`/`l`/`L` suffixes, hex (`0x…`), and a single `<<` shift (for `(1u << 20)`).
fn parse_c_int(s: &str) -> u64 {
    let s = s
        .trim()
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    if let Some((l, r)) = s.split_once("<<") {
        return parse_c_int(l) << parse_c_int(r);
    }
    let s = s.trim().trim_end_matches(['u', 'U', 'l', 'L']);
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).expect("hex literal")
    } else {
        s.parse::<u64>().expect("decimal literal")
    }
}

/// Every `#define CANOPY_* VALUE` in the header (the include guard, which has no value,
/// is skipped), as name -> parsed integer.
fn header_defines() -> BTreeMap<String, u64> {
    let src = include_str!("../include/canopy_protocol.h");
    let mut out = BTreeMap::new();
    for line in src.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("#define ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        if !name.starts_with("CANOPY_") {
            continue;
        }
        // Strip a trailing `/* … */` comment.
        let value = value.split("/*").next().unwrap_or("").trim();
        if value.is_empty() {
            continue; // e.g. the `#define CANOPY_PROTOCOL_H` include guard
        }
        out.insert(name.to_string(), parse_c_int(value));
    }
    out
}

#[test]
fn header_matches_the_rust_constants() {
    use canopy_protocol::{tags, AttrId, NodeId, PROTOCOL_VERSION};

    // The authoritative values, straight from the Rust source of record.
    let expected: BTreeMap<&str, u64> = BTreeMap::from([
        // Protocol (canopy-protocol / canopy-abi).
        ("CANOPY_PROTOCOL_VERSION", u64::from(PROTOCOL_VERSION)),
        ("CANOPY_MAX_BATCH_BYTES", canopy_abi::MAX_BATCH_BYTES as u64),
        (
            "CANOPY_MAX_EVENT_BATCH_BYTES",
            canopy_abi::MAX_EVENT_BATCH_BYTES as u64,
        ),
        ("CANOPY_NODE_ROOT", 0),
        ("CANOPY_NODE_NULL", NodeId::NULL.raw()),
        ("CANOPY_ATTR_ID", u64::from(AttrId::ID.raw())),
        ("CANOPY_OP_BEGIN_BATCH", u64::from(tags::BEGIN_BATCH)),
        ("CANOPY_OP_END_BATCH", u64::from(tags::END_BATCH)),
        ("CANOPY_OP_CREATE_ELEMENT", u64::from(tags::CREATE_ELEMENT)),
        ("CANOPY_OP_CREATE_TEXT", u64::from(tags::CREATE_TEXT)),
        ("CANOPY_OP_REMOVE_NODE", u64::from(tags::REMOVE_NODE)),
        ("CANOPY_OP_INSERT_BEFORE", u64::from(tags::INSERT_BEFORE)),
        ("CANOPY_OP_SET_TEXT", u64::from(tags::SET_TEXT)),
        ("CANOPY_OP_SET_ATTRIBUTE", u64::from(tags::SET_ATTRIBUTE)),
        (
            "CANOPY_OP_SET_INLINE_STYLE",
            u64::from(tags::SET_INLINE_STYLE),
        ),
        ("CANOPY_OP_SET_CLASS", u64::from(tags::SET_CLASS)),
        ("CANOPY_OP_REMOVE_CLASS", u64::from(tags::REMOVE_CLASS)),
        ("CANOPY_OP_ADD_LISTENER", u64::from(tags::ADD_LISTENER)),
        (
            "CANOPY_OP_REMOVE_LISTENER",
            u64::from(tags::REMOVE_LISTENER),
        ),
        ("CANOPY_OP_INTERN_STRING", u64::from(tags::INTERN_STRING)),
        ("CANOPY_OP_SET_TAG_NAME", u64::from(tags::SET_TAG_NAME)),
        ("CANOPY_OP_DISPATCH_EVENT", u64::from(tags::DISPATCH_EVENT)),
        ("CANOPY_PAYLOAD_NONE", u64::from(tags::PAYLOAD_NONE)),
        ("CANOPY_PAYLOAD_POINTER", u64::from(tags::PAYLOAD_POINTER)),
        ("CANOPY_PAYLOAD_KEY", u64::from(tags::PAYLOAD_KEY)),
        ("CANOPY_PAYLOAD_TEXT", u64::from(tags::PAYLOAD_TEXT)),
        // Host-tier widget ids (canopy-view / canopy-paint convention).
        ("CANOPY_EL_COLUMN", u64::from(canopy_view::COLUMN.raw())),
        ("CANOPY_EL_ROW", u64::from(canopy_view::ROW.raw())),
        ("CANOPY_EL_BUTTON", u64::from(canopy_view::BUTTON.raw())),
        ("CANOPY_EL_INPUT", u64::from(canopy_view::INPUT.raw())),
        ("CANOPY_EVENT_CLICK", u64::from(canopy_view::CLICK.raw())),
        ("CANOPY_PROP_BG", u64::from(canopy_paint::BG.raw())),
        ("CANOPY_PROP_FG", u64::from(canopy_paint::FG.raw())),
        ("CANOPY_PROP_WIDTH", u64::from(canopy_paint::WIDTH.raw())),
        ("CANOPY_PROP_HEIGHT", u64::from(canopy_paint::HEIGHT.raw())),
        ("CANOPY_PROP_GAP", u64::from(canopy_paint::GAP.raw())),
        (
            "CANOPY_PROP_PADDING",
            u64::from(canopy_paint::PADDING.raw()),
        ),
        (
            "CANOPY_PROP_DIRECTION",
            u64::from(canopy_paint::DIRECTION.raw()),
        ),
        ("CANOPY_PROP_RADIUS", u64::from(canopy_paint::RADIUS.raw())),
        (
            "CANOPY_PROP_OPACITY",
            u64::from(canopy_paint::OPACITY.raw()),
        ),
        (
            "CANOPY_PROP_TRANSLATE_X",
            u64::from(canopy_paint::TRANSLATE_X.raw()),
        ),
        (
            "CANOPY_PROP_TRANSLATE_Y",
            u64::from(canopy_paint::TRANSLATE_Y.raw()),
        ),
        ("CANOPY_PROP_ALIGN", u64::from(canopy_paint::ALIGN.raw())),
        (
            "CANOPY_PROP_JUSTIFY",
            u64::from(canopy_paint::JUSTIFY.raw()),
        ),
        (
            "CANOPY_PROP_TEXT_ALIGN",
            u64::from(canopy_paint::TEXT_ALIGN.raw()),
        ),
        ("CANOPY_PROP_MARGIN", u64::from(canopy_paint::MARGIN.raw())),
        (
            "CANOPY_PROP_MIN_WIDTH",
            u64::from(canopy_paint::MIN_WIDTH.raw()),
        ),
        (
            "CANOPY_PROP_MIN_HEIGHT",
            u64::from(canopy_paint::MIN_HEIGHT.raw()),
        ),
        (
            "CANOPY_PROP_MAX_WIDTH",
            u64::from(canopy_paint::MAX_WIDTH.raw()),
        ),
        (
            "CANOPY_PROP_MAX_HEIGHT",
            u64::from(canopy_paint::MAX_HEIGHT.raw()),
        ),
        (
            "CANOPY_PROP_FLEX_GROW",
            u64::from(canopy_paint::FLEX_GROW.raw()),
        ),
        (
            "CANOPY_PROP_BORDER_WIDTH",
            u64::from(canopy_paint::BORDER_WIDTH.raw()),
        ),
        (
            "CANOPY_PROP_BORDER_COLOR",
            u64::from(canopy_paint::BORDER_COLOR.raw()),
        ),
    ]);

    let header = header_defines();

    // Every expected constant is present in the header with the right value.
    for (name, want) in &expected {
        let got = header
            .get(*name)
            .unwrap_or_else(|| panic!("header is missing #define {name}"));
        assert_eq!(got, want, "#define {name} drifted from the Rust constant");
    }

    // And the header carries no #define the test does not vouch for — a new constant
    // must be added here (and validated) rather than landing unchecked.
    for name in header.keys() {
        assert!(
            expected.contains_key(name.as_str()),
            "header defines {name} but the parity test does not check it"
        );
    }
}
