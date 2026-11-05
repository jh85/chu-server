use std::path::PathBuf;

use chu_rules::{Backend, Referee, BUILT_CHUSHOGI_GEN};

#[allow(dead_code)]
pub fn vendor_tests() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/chushogi/tests")
}

/// Both backends, labelled.
pub fn backends() -> Vec<(&'static str, Box<dyn Referee>)> {
    vec![
        ("ffi", Backend::Ffi.create().unwrap()),
        (
            "subprocess",
            Backend::Subprocess(BUILT_CHUSHOGI_GEN.into()).create().unwrap(),
        ),
    ]
}
