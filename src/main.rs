// SPDX-License-Identifier: MIT
//! rfcheck: rig-contract checker for game-bound GLBs.
//!
//! Blender (rigforge) makes the GLBs; rfcheck verifies them:
//! container parses, exactly one DEF-only skeleton, animations target
//! DEF joints. See README.md.

mod checks;
mod glb;

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn print_help() {
    println!(
        "usage: rfcheck [--json] [--] file.glb ...\n\
         \n\
         Check each GLB against the rigforge export contract and print\n\
         one `CODE path: detail` line per finding (OK line when clean).\n\
         Exit 0 = clean, 1 = contract failures, 2 = usage/environment error."
    );
}

fn main() -> ExitCode {
    let raw: Vec<String> = env::args().skip(1).collect();
    let mut json_out = false;
    let mut files: Vec<&str> = Vec::new();
    let mut only_files = false;
    for a in &raw {
        if only_files {
            files.push(a);
        } else if a == "--" {
            only_files = true;
        } else if a == "--json" {
            json_out = true;
        } else if a == "-h" || a == "--help" {
            print_help();
            return ExitCode::SUCCESS;
        } else if a.starts_with('-') {
            eprintln!("rfcheck: unknown flag '{a}' (see --help)");
            return ExitCode::from(2);
        } else {
            files.push(a);
        }
    }
    if files.is_empty() {
        eprintln!("rfcheck: no input files (see --help)");
        return ExitCode::from(2);
    }

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
        let report = checks::check_glb(&bytes);
        if json_out {
            println!("{}", report.to_json(path));
        } else if report.diags.is_empty() {
            println!("OK {}: {}", path.display(), report.summary);
        } else {
            for d in &report.diags {
                println!("{} {}: {}", d.code, path.display(), d.detail);
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
