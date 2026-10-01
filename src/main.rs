// SPDX-License-Identifier: MIT
//! rfcheck: rig-contract checker for game-bound GLBs.
//!
//! Blender (rigforge) makes the GLBs; rfcheck verifies them:
//! container parses, exactly one DEF-only skeleton, animations target
//! DEF joints. See README.md.

mod budget;
mod checks;
mod defects;
mod glb;
mod weights;

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn print_help() {
    println!(
        "usage: rfcheck [--json] [--mobile] [--budget file] [--] file.glb ...\n\
         \n\
         Check each GLB against the rigforge export contract and print\n\
         one `CODE path: detail` line per finding (OK line when clean).\n\
         Exit 0 = clean, 1 = contract failures, 2 = usage/environment error.\n\
         --mobile checks mobile perf budgets too (P_* warnings, never fail).\n\
         --budget file overrides budget keys via a JSON object or flat TOML."
    );
}

fn main() -> ExitCode {
    let raw: Vec<String> = env::args().skip(1).collect();
    let mut json_out = false;
    let mut want_budget = false;
    let mut budget_path: Option<&str> = None;
    let mut files: Vec<&str> = Vec::new();
    let mut only_files = false;
    let mut i = 0;
    while i < raw.len() {
        let a = &raw[i];
        if only_files {
            files.push(a);
        } else if a == "--" {
            only_files = true;
        } else if a == "--json" {
            json_out = true;
        } else if a == "--mobile" {
            want_budget = true;
        } else if a == "--budget" {
            i += 1;
            match raw.get(i) {
                Some(p) => budget_path = Some(p),
                None => {
                    eprintln!("rfcheck: --budget needs a file argument (see --help)");
                    return ExitCode::from(2);
                }
            }
        } else if a == "-h" || a == "--help" {
            print_help();
            return ExitCode::SUCCESS;
        } else if a.starts_with('-') {
            eprintln!("rfcheck: unknown flag '{a}' (see --help)");
            return ExitCode::from(2);
        } else {
            files.push(a);
        }
        i += 1;
    }
    if files.is_empty() {
        eprintln!("rfcheck: no input files (see --help)");
        return ExitCode::from(2);
    }

    let budget: Option<budget::Budget> = match &budget_path {
        Some(p) => match budget::load_budget(Path::new(p)) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("rfcheck: {e}");
                return ExitCode::from(2);
            }
        },
        None if want_budget => Some(budget::Budget::default()),
        None => None,
    };

    let mut failed = false;
    for f in &files {
        let path = Path::new(f);
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                let detail = format!("cannot read file: {e}");
                if json_out {
                    println!(
                        "{{\"file\":\"{}\",\"ok\":false,\
                         \"summary\":\"unreadable\",\
                         \"diags\":[{{\"code\":\"R_IO\",\"detail\":\"{}\"}}]}}",
                        path.display(),
                        detail.replace('"', "'"),
                    );
                } else {
                    println!("R_IO {}: {detail}", path.display());
                }
                failed = true;
                continue;
            }
        };
        let report = match &budget {
            Some(b) => checks::check_glb_with_budget(&bytes, Some(b)),
            None => checks::check_glb(&bytes),
        };
        if json_out {
            println!("{}", report.to_json(path));
        } else if report.diags.is_empty() {
            for w in report.warnings() {
                println!("{} {}: {}", w.code, path.display(), w.detail);
            }
            println!("OK {}: {}", path.display(), report.summary);
        } else {
            for d in &report.diags {
                println!("{} {}: {}", d.code, path.display(), d.detail);
            }
            for w in report.warnings() {
                println!("{} {}: {}", w.code, path.display(), w.detail);
            }
        }
        failed |= report.failed();
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
