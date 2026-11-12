//! `include/fylo.h` must declare exactly the C functions, status values and
//! ABI version that the crate exports (ADR 0026 section 3).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

type Signatures = BTreeMap<String, (String, Vec<String>)>;

fn crate_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn c_type(rust: &str) -> String {
    let c = match rust.trim() {
        "*const u8" => "const uint8_t *",
        "*mut u64" => "uint64_t *",
        "*mut FyloBytes" => "FyloBytes *",
        "usize" => "size_t",
        "u64" => "uint64_t",
        "u32" => "uint32_t",
        "FyloBytes" => "FyloBytes",
        "FyloStatus" => "FyloStatus",
        "" => "void",
        other => panic!("no C spelling for the Rust type {other}"),
    };
    c.to_owned()
}

fn rust_signatures(source: &str) -> Signatures {
    let mut signatures = Signatures::new();
    for piece in source.split("extern \"C\" fn ").skip(1) {
        let (name, rest) = piece.split_once('(').unwrap();
        let (parameters, after) = rest.split_once(')').unwrap();
        let returned = after
            .split('{')
            .next()
            .unwrap()
            .trim()
            .trim_start_matches("->");
        let types = parameters
            .split(',')
            .filter(|parameter| !parameter.trim().is_empty())
            .map(|parameter| c_type(parameter.split_once(':').unwrap().1))
            .collect();
        signatures.insert(name.trim().to_owned(), (c_type(returned), types));
    }
    signatures
}

fn header_signatures(header: &str) -> Signatures {
    let code: String = header
        .split("/*")
        .map(|piece| piece.split_once("*/").map_or(piece, |(_, after)| after))
        .collect();
    let mut signatures = Signatures::new();
    for statement in code.split(';') {
        let statement = statement.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(start) = statement.find("fylo_") else {
            continue;
        };
        let Some((head, parameters)) = statement[start..].split_once('(') else {
            continue;
        };
        let returned = statement[..start].rsplit('}').next().unwrap().trim();
        let returned = returned.rsplit(' ').next().unwrap_or(returned);
        let types = parameters
            .trim_end_matches(')')
            .split(',')
            .filter(|parameter| parameter.trim() != "void")
            .map(|parameter| {
                let parameter = parameter.trim();
                let cut = parameter.rfind([' ', '*']).unwrap();
                let pointer = if parameter[..=cut].contains('*') {
                    " *"
                } else {
                    ""
                };
                format!(
                    "{}{pointer}",
                    parameter[..=cut].trim_end_matches(['*', ' '])
                )
            })
            .collect();
        signatures.insert(head.trim().to_owned(), (returned.to_owned(), types));
    }
    signatures
}

#[test]
fn every_exported_function_has_the_same_c_signature_in_the_header() {
    let code = rust_signatures(&crate_file("src/c_abi.rs"));
    let header = header_signatures(&crate_file("include/fylo.h"));
    assert_eq!(code.len(), 11, "{code:#?}");
    assert_eq!(header, code);
}

#[test]
fn the_header_names_the_abi_version_the_library_reports() {
    let header = crate_file("include/fylo.h");
    let expected = format!("#define FYLO_ABI_VERSION {}", fylo_ffi::CAbi::ABI_VERSION);
    assert!(header.contains(&expected), "fylo.h lacks `{expected}`");
}

#[test]
fn the_header_gives_every_status_its_number() {
    let header = crate_file("include/fylo.h");
    let statuses = [
        ("FYLO_OK", fylo_ffi::FyloStatus::Ok),
        ("FYLO_NOT_FOUND", fylo_ffi::FyloStatus::NotFound),
        ("FYLO_ERROR", fylo_ffi::FyloStatus::Error),
        (
            "FYLO_INVALID_ARGUMENT",
            fylo_ffi::FyloStatus::InvalidArgument,
        ),
        ("FYLO_CLOSED", fylo_ffi::FyloStatus::Closed),
        ("FYLO_PANIC", fylo_ffi::FyloStatus::Panic),
    ];
    let declared = header.matches(" = ").count();
    assert_eq!(declared, statuses.len(), "fylo.h declares other values");
    for (name, status) in statuses {
        let line = format!("{name} = {}", status as i32);
        assert!(header.contains(&line), "fylo.h lacks `{line}`");
    }
}
