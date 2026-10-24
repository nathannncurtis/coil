//! The `coilc` driver (spec section 2): `coilc build <job.json>`,
//! `coilc inspect <job.json> --json` and `coilc --version`.

use std::path::Path;
use std::process::ExitCode;

use coilc::build;
use coilc::job;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--version"] => {
            println!("coilc {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        ["build", job_path] => build_command(Path::new(job_path)),
        ["inspect", _job, "--json"] => not_implemented("inspect"),
        _ => usage(),
    }
}

fn build_command(job_path: &Path) -> ExitCode {
    let job = match job::load(job_path) {
        Ok(job) => job,
        Err(error) => return fail(&error),
    };
    let cache_root = match build::default_cache_root() {
        Ok(cache_root) => cache_root,
        Err(error) => return fail(&error),
    };
    match build::run(&job, &cache_root) {
        Ok(summary) => {
            println!("coilc: {summary}");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error),
    }
}

fn fail(error: &impl std::fmt::Display) -> ExitCode {
    eprintln!("coilc: {error}");
    ExitCode::from(1)
}

fn not_implemented(command: &str) -> ExitCode {
    eprintln!("coilc: {command} is not implemented in this build");
    ExitCode::from(2)
}

fn usage() -> ExitCode {
    eprintln!("usage: coilc build <job.json> | coilc inspect <job.json> --json | coilc --version");
    ExitCode::from(2)
}
