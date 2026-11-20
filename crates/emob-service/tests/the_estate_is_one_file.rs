//! The whole estate (spec 012): every section any daemon reads, the published
//! JSON Schema that describes it, and the example that has to satisfy both.
#![cfg(feature = "schema")]

use std::path::PathBuf;

use emob_service::estate::{self, Estate};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_example_estate_is_a_whole_estate() {
    let estate: Estate =
        estate::load(&root().join("deploy/estate.example.toml")).unwrap_or_else(|e| panic!("{e}"));
    assert!(!estate.stations.is_empty());
}

#[test]
fn a_section_no_daemon_reads_is_refused_and_named() {
    let text = std::fs::read_to_string(root().join("deploy/estate.example.toml")).unwrap();
    let refused = estate::from_str::<Estate>(&format!("{text}\n[[subscriber]]\nname = \"x\"\n"))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("subscriber"), "{refused}");
}

/// The schema in the repository is the one the types derive. `UPDATE_SCHEMAS=1`
/// rewrites it; CI never sets it, so a change to a section without the schema
/// fails here.
#[test]
fn the_published_schema_is_the_one_the_types_describe() {
    let path = root().join("schemas/estate.schema.json");
    let derived = serde_json::to_string_pretty(&estate::json_schema()).unwrap() + "\n";
    if std::env::var_os("UPDATE_SCHEMAS").is_some() {
        std::fs::write(&path, &derived).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == derived,
        "schemas/estate.schema.json is stale: run `UPDATE_SCHEMAS=1 cargo test -p emob-service --features schema --test the_estate_is_one_file`"
    );
}
