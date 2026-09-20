//! `run_evals` — the Rust analogue of the upstream `npm run eval`.
//!
//! ```text
//! cargo run -p pi-evals --example run_evals -- [OPTIONS]
//!
//!   --suite <name>        run only suites whose name contains <name>
//!   --case <id>           run only cases whose id contains <id>
//!   --repetitions <n>     run every selected case n times (default 1)
//!   --live                set PI_EVAL_LIVE=1 so network cases run
//!   --out <dir>           artifact directory (default `.eval`)
//!   --no-artifacts        do not write report.json / report.txt / runs.jsonl
//!   --json                print the JSON report instead of the text report
//!   --help                show this help
//! ```
//!
//! Exits non-zero when any case failed, so CI can gate on it.

use std::path::PathBuf;
use std::process::ExitCode;

use pi_evals::{run_suites, suite_named, CaseStatus, EvalReport, RunOptions};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(success) => {
            if success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Cli {
    options: RunOptions,
    json: bool,
}

async fn run() -> Result<bool, String> {
    let cli = match parse_args()? {
        Some(cli) => cli,
        None => return Ok(true),
    };

    let suites = match &cli.options.suite_filter {
        Some(name) => vec![suite_named(name).ok_or_else(|| {
            format!("unknown suite `{name}` (expected smoke|models|providers|extensions|docs)")
        })?],
        None => pi_evals::all_suites(),
    };

    let report = run_suites(&suites, &cli.options).await;
    if cli.json {
        println!("{}", report.to_json().map_err(|error| error.to_string())?);
    } else {
        print!("{}", report.to_text());
    }
    if let Some(dir) = &cli.options.artifacts_dir {
        report
            .write(dir)
            .map_err(|error| format!("writing artifacts to {}: {error}", dir.display()))?;
        eprintln!("artifacts written to {}", dir.display());
    }
    Ok(summarize(&report))
}

fn parse_args() -> Result<Option<Cli>, String> {
    let mut options = RunOptions {
        artifacts_dir: Some(PathBuf::from(".eval")),
        ..RunOptions::default()
    };
    let mut json = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                return Ok(None);
            }
            "--live" => {
                std::env::set_var("PI_EVAL_LIVE", "1");
            }
            "--json" => json = true,
            "--no-artifacts" => options.artifacts_dir = None,
            "--suite" => {
                options.suite_filter = Some(
                    args.next()
                        .ok_or_else(|| "--suite needs a value".to_string())?,
                );
            }
            "--case" => {
                options.case_filter = Some(
                    args.next()
                        .ok_or_else(|| "--case needs a value".to_string())?,
                );
            }
            "--repetitions" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--repetitions needs a value".to_string())?;
                options.repetitions = value
                    .parse()
                    .map_err(|error| format!("invalid --repetitions `{value}`: {error}"))?;
            }
            "--out" => {
                options.artifacts_dir = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--out needs a value".to_string())?,
                ));
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }
    Ok(Some(Cli { options, json }))
}

fn print_help() {
    println!(
        "pi-evals — offline-first eval harness\n\n\
         cargo run -p pi-evals --example run_evals -- [OPTIONS]\n\n\
         \x20 --suite <name>        run only suites whose name contains <name>\n\
         \x20 --case <id>           run only cases whose id contains <id>\n\
         \x20 --repetitions <n>     run every selected case n times (default 1)\n\
         \x20 --live                set PI_EVAL_LIVE=1 so network cases run\n\
         \x20 --out <dir>           artifact directory (default `.eval`)\n\
         \x20 --no-artifacts        skip report.json / report.txt / runs.jsonl\n\
         \x20 --json                print the JSON report instead of the text report\n\
         \x20 --help                show this help"
    );
}

fn summarize(report: &EvalReport) -> bool {
    let skipped: Vec<&str> = report
        .cases()
        .filter(|case| case.status == CaseStatus::Skipped)
        .map(|case| case.id.as_str())
        .collect();
    if !skipped.is_empty() {
        eprintln!(
            "skipped (opt in with PI_EVAL_LIVE=1): {}",
            skipped.join(", ")
        );
    }
    report.totals.is_success()
}
