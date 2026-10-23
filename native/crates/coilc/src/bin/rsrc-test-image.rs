//! Builds resources for the PE resource tests (spec section 13.3).
//!
//! `dump-versioninfo <out.bin> [key=value ...]` writes the raw
//! `VS_VERSIONINFO` bytes for the given fields, so the Python tests can
//! byte-compare them with what `platforms/windows.py:set_version_info`
//! produces. `link <out.exe> [--icon <ico>] [--manifest <xml>]
//! [key=value ...]` links a minimal image carrying those resources and
//! prints one `key=value` layout fact per line. Field defaults mirror
//! `set_version_info`: `file-description` and `internal-name` default to
//! `product-name`, `original-filename` to the output path's file name
//! (standing in for the exe name), the two versions to 1.0.0.0 and the
//! three free-text fields to the empty string.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

use coilc::link::objfile::{BssSpec, ObjectFile, SectionContent, SectionId, Subsystem, Symbol};
use coilc::link::{pe, rsrc};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("dump-versioninfo") => dump_versioninfo(args.next(), args.collect()),
        Some("link") => link(args.next(), args.collect()),
        _ => {
            eprintln!("usage: rsrc-test-image dump-versioninfo <out.bin> [key=value ...]");
            eprintln!(
                "       rsrc-test-image link <out.exe> [--icon <ico>] [--manifest <xml>] [key=value ...]"
            );
            ExitCode::from(2)
        }
    }
}

/// The nine `set_version_info` fields as given on the command line;
/// `None` means the key was absent and the reference default applies.
#[derive(Default)]
struct Fields {
    product_name: Option<String>,
    file_description: Option<String>,
    file_version: Option<String>,
    product_version: Option<String>,
    company_name: Option<String>,
    legal_copyright: Option<String>,
    internal_name: Option<String>,
    original_filename: Option<String>,
    comments: Option<String>,
}

fn parse_fields(args: &[String]) -> Result<Fields, String> {
    let mut fields = Fields::default();
    for arg in args {
        let Some((key, value)) = arg.split_once('=') else {
            return Err(format!("expected key=value, got {arg:?}"));
        };
        let slot = match key {
            "product-name" => &mut fields.product_name,
            "file-description" => &mut fields.file_description,
            "file-version" => &mut fields.file_version,
            "product-version" => &mut fields.product_version,
            "company-name" => &mut fields.company_name,
            "legal-copyright" => &mut fields.legal_copyright,
            "internal-name" => &mut fields.internal_name,
            "original-filename" => &mut fields.original_filename,
            "comments" => &mut fields.comments,
            unknown => return Err(format!("unknown version field {unknown:?}")),
        };
        *slot = Some(value.to_string());
    }
    Ok(fields)
}

/// Applies `set_version_info`'s defaults; `exe_name` stands in for its
/// `exe_path.name` default of `original_filename`. `None` when no
/// `product-name` was given, meaning no `RT_VERSION` at all.
fn resolve(fields: Fields, exe_name: &str) -> Result<Option<rsrc::VersionInfo>, String> {
    let Some(product_name) = fields.product_name else {
        return Ok(None);
    };
    Ok(Some(rsrc::VersionInfo {
        file_description: fields
            .file_description
            .unwrap_or_else(|| product_name.clone()),
        internal_name: fields.internal_name.unwrap_or_else(|| product_name.clone()),
        original_filename: fields
            .original_filename
            .unwrap_or_else(|| exe_name.to_string()),
        file_version: fields.file_version.unwrap_or_else(|| "1.0.0.0".into()),
        product_version: fields.product_version.unwrap_or_else(|| "1.0.0.0".into()),
        company_name: fields.company_name.unwrap_or_default(),
        legal_copyright: fields.legal_copyright.unwrap_or_default(),
        comments: fields.comments.unwrap_or_default(),
        product_name,
    }))
}

/// The output path's file name, standing in for `set_version_info`'s exe.
fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn dump_versioninfo(output: Option<String>, rest: Vec<String>) -> ExitCode {
    let Some(output) = output else {
        eprintln!("usage: rsrc-test-image dump-versioninfo <out.bin> [key=value ...]");
        return ExitCode::from(2);
    };
    match dump(&output, &rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rsrc-test-image: {error}");
            ExitCode::from(1)
        }
    }
}

fn dump(output: &str, fields: &[String]) -> Result<(), String> {
    let info = resolve(parse_fields(fields)?, file_name(output).as_str())?
        .ok_or("product-name is required")?;
    let bytes = rsrc::vs_version_info(&info).map_err(|error| error.to_string())?;
    std::fs::write(output, bytes).map_err(|error| format!("{output}: {error}"))
}

fn link(output: Option<String>, rest: Vec<String>) -> ExitCode {
    let Some(output) = output else {
        eprintln!(
            "usage: rsrc-test-image link <out.exe> [--icon <ico>] [--manifest <xml>] [key=value ...]"
        );
        return ExitCode::from(2);
    };
    match link_resources(&output, rest) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rsrc-test-image: {error}");
            ExitCode::from(1)
        }
    }
}

fn link_resources(output: &str, rest: Vec<String>) -> Result<(), String> {
    let mut args = rest.into_iter();
    let mut icon_path = None;
    let mut manifest_path = None;
    let mut fields = Vec::new();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--icon" => {
                icon_path = Some(args.next().ok_or("--icon needs a path".to_string())?);
            }
            "--manifest" => {
                manifest_path = Some(args.next().ok_or("--manifest needs a path".to_string())?);
            }
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            field => fields.push(field.to_string()),
        }
    }
    let read = |path: &Option<String>| -> Result<Option<Vec<u8>>, String> {
        path.as_deref()
            .map(std::fs::read)
            .transpose()
            .map_err(|error| format!("{}: {error}", path.as_deref().unwrap_or("?")))
    };
    let icon = read(&icon_path)?;
    let manifest = read(&manifest_path)?;

    let info = resolve(parse_fields(&fields)?, file_name(output).as_str())?;
    let obj = minimal_object();
    let resources = rsrc::Resources {
        icon: icon.as_deref(),
        version: info.as_ref(),
        manifest: manifest.as_deref(),
    };
    let linked = pe::link_with_resources(&obj, &resources).map_err(|error| error.to_string())?;
    std::fs::write(output, &linked.image).map_err(|error| format!("{output}: {error}"))?;
    println!("text_rva={}", linked.layout.text_rva);
    println!("rsrc_rva={}", linked.layout.rsrc_rva);
    println!("rsrc_size={}", linked.layout.rsrc_vsize);
    println!("size_of_image={}", linked.layout.size_of_image);
    Ok(())
}

/// The smallest object that links: a lone `ret` entry. The resource tests
/// read the section table and the resources, not the code.
fn minimal_object() -> ObjectFile {
    ObjectFile {
        text: SectionContent {
            bytes: vec![0xC3],
            align: 16,
        },
        rdata: SectionContent {
            bytes: Vec::new(),
            align: 1,
        },
        data: SectionContent {
            bytes: Vec::new(),
            align: 1,
        },
        bss: BssSpec::default(),
        symbols: vec![Symbol {
            name: "entry".into(),
            section: SectionId::Text,
            offset: 0,
        }],
        relocs: Vec::new(),
        imports: BTreeMap::new(),
        unwind: Vec::new(),
        entry: "entry".into(),
        subsystem: Subsystem::Console,
        stack_reserve: None,
    }
}
