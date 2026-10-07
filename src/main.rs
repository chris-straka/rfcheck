// SPDX-License-Identifier: MIT
//! rfcheck: rig-contract checker for game-bound GLBs.
//!
//! Blender (rigforge) makes the GLBs; rfcheck verifies them:
//! container parses, DEF-only skeleton, animations target DEF joints,
//! and Bevy 0.19's glTF loader takes the file whole. See README.md.

mod bevy;
mod budget;
mod checks;
mod defects;
mod glb;
mod util;
mod weights;

use std::env;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

fn print_help() {
    println!(
        "usage: rfcheck [--json] [--mobile] [--budget file] [--class name]\n\
         \x20             [--image-codecs list] [--] file.glb ...\n\
         \n\
         Check each GLB against the rigforge export contract and print\n\
         one `CODE path: detail` line per finding (OK line when clean).\n\
         Exit 0 = clean, 1 = contract failures, 2 = usage/environment error.\n\
         --mobile checks mobile perf budgets too (P_* warnings, never fail).\n\
         --budget file overrides budget keys via a JSON object or flat TOML.\n\
         --class name checks as one asset class (hero, npc, monster, prop,\n\
         weapon, level): per-class mobile budgets, and prop/weapon/level\n\
         files may be unrigged. Implies budget checks, like --budget.\n\
         --image-codecs list sets the texture codecs the game's Bevy build\n\
         decodes (default png,ktx2; also jpeg, webp, dds, hdr)."
    );
}

fn main() -> ExitCode {
    let raw: Vec<String> = env::args().skip(1).collect();
    let mut json_out = false;
    let mut want_budget = false;
    let mut budget_path: Option<&str> = None;
    let mut class: Option<budget::AssetClass> = None;
    let mut codecs: Vec<&'static str> = bevy::DEFAULT_CODECS.to_vec();
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
        } else if a == "--class" {
            i += 1;
            match raw.get(i) {
                Some(p) => match budget::parse_class(p) {
                    Some(c) => class = Some(c),
                    None => {
                        eprintln!(
                            "rfcheck: unknown asset class '{p}' \
                             (expected {})",
                            budget::CLASS_NAMES
                        );
                        return ExitCode::from(2);
                    }
                },
                None => {
                    eprintln!("rfcheck: --class needs a name argument (see --help)");
                    return ExitCode::from(2);
                }
            }
        } else if a == "--image-codecs" {
            i += 1;
            match raw.get(i).map(|l| bevy::parse_codecs(l)) {
                Some(Ok(c)) => codecs = c,
                Some(Err(e)) => {
                    eprintln!("rfcheck: {e}");
                    return ExitCode::from(2);
                }
                None => {
                    eprintln!("rfcheck: --image-codecs needs a list argument (see --help)");
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

    // --class implies budget checks (mobile profile), like --budget.
    if class.is_some() {
        want_budget = true;
    }
    let budgets: Option<budget::BudgetSet> = match &budget_path {
        Some(p) => match budget::load_budget_set(Path::new(p)) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("rfcheck: {e}");
                return ExitCode::from(2);
            }
        },
        None if want_budget => Some(budget::BudgetSet::mobile()),
        None => None,
    };

    let opts = bevy::BevyOpts { codecs: &codecs };
    let mut failed = false;
    for f in &files {
        let path = Path::new(f);
        let report = match fs::read(path) {
            Err(e) => checks::Report::unreadable(format!("cannot read file: {e}"), class),
            Ok(bytes) => {
                let b = budgets.as_ref().map(|s| s.for_class(class));
                checks::check_glb_full(&bytes, b, class, &opts)
            }
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
