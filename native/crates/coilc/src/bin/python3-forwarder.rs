//! Writes the `python3.dll` stable-ABI forwarder (spec section 10.1) to a
//! path: a DLL with no entry point whose export table forwards every name
//! of `coil-capi-sys/python3-exports.txt` to `python312.<name>`, exactly
//! the way CPython's own `python3.dll` does. The Python tests compare the
//! output with the real DLL from the embeddable distribution.

use std::process::ExitCode;

use coilc::link::pe;

fn main() -> ExitCode {
    let Some(output) = std::env::args().nth(1) else {
        eprintln!("usage: python3-forwarder <output.dll>");
        return ExitCode::from(2);
    };
    // One `name func|data` line per export; the kind column is irrelevant
    // here because data exports forward exactly like functions.
    let names: Vec<String> = coil_capi_sys::PYTHON3_EXPORTS
        .lines()
        .map(|line| line.split(' ').next().unwrap_or_default().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    let image = match pe::python3_forwarder(&names) {
        Ok(image) => image,
        Err(error) => {
            eprintln!("python3-forwarder: {error}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = std::fs::write(&output, image) {
        eprintln!("python3-forwarder: {error}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
