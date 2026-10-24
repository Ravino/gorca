// SPDX-License-Identifier: GPL-3.0-or-later
//! The Rust registries must equal the oracle tables in the FUSE reference corpus headers
//! (`tests/fixtures/fuse/fuse-x32.json`, target layout; `fuse-r2-16.json`, older layout, is a
//! prefix of it). A drift in either side fails here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use gorca_model::sem::registry::{self, Row};
use serde_json::Value;

fn load(name: &str) -> Value {
    let path = format!(
        "{}/../../tests/fixtures/fuse/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn table(tables: &Value, key: &str) -> BTreeMap<String, u64> {
    tables[key]
        .as_object()
        .unwrap_or_else(|| panic!("tables.{key} missing"))
        .iter()
        .map(|(k, v)| (k.clone(), v.as_u64().unwrap()))
        .collect()
}

fn rows(r: Vec<Row>) -> BTreeMap<String, u64> {
    r.into_iter()
        .map(|(n, c)| (n.to_owned(), u64::from(c)))
        .collect()
}

/// Registered groups with no oracle row, in code order.
const APPENDED_GROUPS: [&str; 5] = [
    "roledescription",
    "placeholder",
    "keyshortcuts",
    "valuetext",
    "namefrom",
];

#[test]
fn x32_header_tables_equal_the_rust_registries() {
    let doc = load("fuse-x32.json");
    assert_eq!(doc["layout"], "x32");
    let t = &doc["tables"];

    let mut groups = rows(registry::groups());
    // Groups appended to the registry after the corpus was generated (M22a-C3, review W-6): the
    // rare text and the name source. They have no oracle rows and no corpus case writes them;
    // they are plain string/enum registers under the same join as `description`.
    for name in APPENDED_GROUPS {
        let code = groups
            .remove(name)
            .unwrap_or_else(|| panic!("{name} is not registered"));
        assert!(code >= 18, "{name}: appended codes follow the oracle's 18");
    }
    groups.extend(rows(registry::evidence()));
    assert_eq!(table(t, "group_codes"), groups, "group_codes");
    // `Provisional` (PV-1, M22b1 slice 4) was appended after the corpus was generated: the oracle
    // never produces it (it is made by `Ivm::entry_or_provisional`, outside FUSE).
    let mut prov = rows(registry::prov_codes());
    assert_eq!(
        prov.remove("Provisional"),
        Some(8),
        "appended after the oracle's eight"
    );
    assert_eq!(table(t, "prov_codes"), prov, "prov_codes");
    assert_eq!(
        table(t, "conf_codes"),
        rows(registry::conf_classes()),
        "conf_codes"
    );
    assert_eq!(
        table(t, "bind_quality"),
        rows(registry::bind_qualities()),
        "bind_quality"
    );
    assert_eq!(
        table(t, "class_codes"),
        rows(registry::source_classes()),
        "class_codes"
    );

    assert_eq!(
        t["thresholds"]["high"],
        u64::from(gorca_model::sem::HIGH_MILLI)
    );
    assert_eq!(
        t["thresholds"]["probable"],
        u64::from(gorca_model::sem::PROBABLE_MILLI)
    );
    assert!(
        t["prov_codes"].get("Fallback").is_none(),
        "Fallback was removed from the ADR; regenerate the fixture"
    );
}

#[test]
fn x32_source_tiers_decode_as_trust_tiers() {
    let doc = load("fuse-x32.json");
    let sources = doc["tables"]["sources"].as_object().unwrap();
    assert!(!sources.is_empty());
    for (name, row) in sources {
        let class = row[2].as_str().unwrap();
        let tier = row[3].as_u64().unwrap();
        assert!(
            gorca_model::sem::SourceClass::from_name(class).is_some(),
            "{name}: class {class}"
        );
        assert!(
            u8::try_from(tier)
                .ok()
                .and_then(gorca_model::sem::TrustTier::from_code)
                .is_some(),
            "{name}: tier {tier}"
        );
    }
}

#[test]
fn r2_16_tables_are_the_x32_prefix_with_equal_codes() {
    let r2 = load("fuse-r2-16.json");
    let x32 = load("fuse-x32.json");
    assert_eq!(r2["layout"], "r2-16");
    let r2_groups = table(&r2["tables"], "group_codes");
    let x_groups = table(&x32["tables"], "group_codes");
    for (name, code) in &r2_groups {
        assert_eq!(x_groups.get(name), Some(code), "r2-16 group {name}");
    }
    for key in ["prov_codes", "conf_codes", "bind_quality", "class_codes"] {
        assert_eq!(
            table(&r2["tables"], key),
            table(&x32["tables"], key),
            "{key}"
        );
    }
}

#[test]
fn fixture_headers_label_max_str_as_test_scale() {
    for f in ["fuse-x32.json", "fuse-r2-16.json"] {
        let doc = load(f);
        let schema = doc["schema"].as_str().unwrap();
        assert!(
            schema.contains("TEST-SCALE"),
            "{f}: max_str must be labelled test-scale"
        );
        assert!(schema.contains("1 KiB"), "{f}: product limit must be named");
    }
}
