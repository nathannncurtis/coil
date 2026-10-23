//! The `coilc` driver (spec section 2): `coilc build <job.json>`,
//! `coilc inspect <job.json> --json` and `coilc --version`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => {
            println!("coilc {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["build", _job] => not_implemented("build"),
        ["inspect", _job, "--json"] => not_implemented("inspect"),
        _ => usage(),
    }
}

fn not_implemented(command: &str) -> ExitCode {
    eprintln!("coilc: {command} is not implemented in this build");
    ExitCode::from(2)
}

fn usage() -> ExitCode {
    eprintln!("usage: coilc build <job.json> | coilc inspect <job.json> --json | coilc --version");
    ExitCode::from(2)
}
