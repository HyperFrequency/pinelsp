//! `pine-cli` — command-line Pine Script v6 linter.
//!
//! Runs `pine-check` over a `.pine` file. Human output by default; `--json`
//! emits a structured report (the harness P3's differential testing builds on).
//! Exit code: 0 = no errors, 1 = errors present, 2 = usage/IO/parse failure.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser as ClapParser;
use pine_check::{analyze, Severity};
use pine_core::Document;
use serde::Serialize;

#[derive(ClapParser)]
#[command(name = "pine-cli", about = "Pine Script v6 linter")]
struct Args {
    /// Path to the .pine file to check.
    file: PathBuf,
    /// Emit a JSON report instead of human-readable lines.
    #[arg(long)]
    json: bool,
}

#[derive(Serialize)]
struct Report {
    file: String,
    diagnostics: Vec<DiagOut>,
}

#[derive(Serialize)]
struct DiagOut {
    severity: &'static str,
    code: &'static str,
    message: String,
    range: RangeOut,
}

#[derive(Serialize)]
struct RangeOut {
    start: PosOut,
    end: PosOut,
}

#[derive(Serialize)]
struct PosOut {
    line: u32,
    character: u32,
}

fn sev_str(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn main() -> ExitCode {
    let args = Args::parse();

    let src = match std::fs::read_to_string(&args.file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pine-cli: {}: {e}", args.file.display());
            return ExitCode::from(2);
        }
    };
    let Some(doc) = Document::parse(&src) else {
        eprintln!("pine-cli: {}: failed to parse", args.file.display());
        return ExitCode::from(2);
    };

    let diagnostics = analyze(&doc);
    let has_error = diagnostics.iter().any(|d| matches!(d.severity, Severity::Error));

    if args.json {
        let out = Report {
            file: args.file.display().to_string(),
            diagnostics: diagnostics
                .iter()
                .map(|d| {
                    let (sl, sc) = doc.position_at(d.start_byte);
                    let (el, ec) = doc.position_at(d.end_byte);
                    DiagOut {
                        severity: sev_str(d.severity),
                        code: d.code,
                        message: d.message.clone(),
                        range: RangeOut {
                            start: PosOut { line: sl, character: sc },
                            end: PosOut { line: el, character: ec },
                        },
                    }
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else if diagnostics.is_empty() {
        println!("{}: no issues", args.file.display());
    } else {
        for d in &diagnostics {
            let (line, col) = doc.position_at(d.start_byte);
            println!(
                "{}:{}:{}: {}: {} [{}]",
                args.file.display(),
                line + 1,
                col + 1,
                sev_str(d.severity),
                d.message,
                d.code
            );
        }
    }

    if has_error {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
