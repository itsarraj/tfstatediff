use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use tfstatediff::diff::{compare, ResourceChangeKind};
use tfstatediff::state;

#[derive(Parser)]
#[command(
    name = "tfstatediff",
    about = "Diffs two Terraform state (.tfstate) snapshots: resources created/destroyed/drifted and output changes"
)]
struct Cli {
    old: PathBuf,
    new: PathBuf,
}

fn load(path: &PathBuf) -> anyhow::Result<state::TfState> {
    let text = std::fs::read_to_string(path)?;
    Ok(state::parse(&text)?)
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let old = match load(&cli.old) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tfstatediff: {}: {e}", cli.old.display());
            return ExitCode::from(2);
        }
    };
    let new = match load(&cli.new) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("tfstatediff: {}: {e}", cli.new.display());
            return ExitCode::from(2);
        }
    };

    let comparison = compare(&old, &new);

    if !comparison.lineage_matches {
        println!(
            "WARNING: state lineage differs ({} vs {}) — these snapshots are not from the same state history; a diff may not be meaningful.\n",
            comparison.old_lineage, comparison.new_lineage
        );
    }
    println!(
        "Serial: {} -> {}",
        comparison.old_serial, comparison.new_serial
    );

    if comparison.resource_diffs.is_empty() {
        println!("\nNo resource changes.");
    } else {
        println!("\nResource changes:");
        for d in &comparison.resource_diffs {
            match d.kind {
                ResourceChangeKind::Created => println!("  + {} (created)", d.address),
                ResourceChangeKind::Destroyed => println!("  - {} (destroyed)", d.address),
                ResourceChangeKind::Changed => {
                    println!(
                        "  ~ {} ({} attribute(s) changed)",
                        d.address,
                        d.attr_changes.len()
                    );
                    for change in &d.attr_changes {
                        let old_s = change.old.as_deref().unwrap_or("<absent>");
                        let new_s = change.new.as_deref().unwrap_or("<absent>");
                        println!("      {}: {old_s} -> {new_s}", change.path);
                    }
                }
            }
        }
    }

    if comparison.output_diffs.is_empty() {
        println!("\nNo output changes.");
    } else {
        println!("\nOutput changes:");
        for d in &comparison.output_diffs {
            let old_s = d.old.as_deref().unwrap_or("<absent>");
            let new_s = d.new.as_deref().unwrap_or("<absent>");
            println!("  {}: {old_s} -> {new_s}", d.name);
        }
    }

    let has_changes = !comparison.resource_diffs.is_empty() || !comparison.output_diffs.is_empty();
    if has_changes {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
