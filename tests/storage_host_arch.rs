//! A storage result that does not name its host and arch is refused (libviprs/libviprs#1331).
//!
//! The archive buckets runs by os, arch and cpu model (`run_id`), and a document missing them
//! files under `unknown` next to every other document missing them, so two machines' numbers
//! look like one series. The refusal is the same for every storage document, the PVec row's
//! included.

use libviprs_bench::storage::archive::admit;
use serde_json::{Value, json};

fn doc(os: Value, arch: Value, cpu: Value) -> Value {
    json!({ "provenance": { "os": os, "arch": arch, "cpuModel": cpu } })
}

fn codes(v: &Value) -> Vec<&'static str> {
    admit(v).into_iter().map(|r| r.code).collect()
}

#[test]
fn a_document_without_arch_os_or_cpu_is_refused_by_name() {
    for (label, d) in [
        ("no arch", doc(json!("linux"), Value::Null, json!("x"))),
        ("empty arch", doc(json!("linux"), json!(" "), json!("x"))),
        ("no os", doc(Value::Null, json!("x86_64"), json!("x"))),
        ("no cpu", doc(json!("linux"), json!("x86_64"), Value::Null)),
        ("nothing", json!({})),
    ] {
        assert!(codes(&d).contains(&"host-arch"), "{label}: {:?}", codes(&d));
    }
}

#[test]
fn a_document_that_names_them_is_not_refused_for_that() {
    let d = doc(json!("linux"), json!("x86_64"), json!("AMD Ryzen"));
    assert!(!codes(&d).contains(&"host-arch"));
}
