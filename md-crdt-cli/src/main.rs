use clap::{Parser, Subcommand};
use md_crdt_filesync::Vault;
use serde::Serialize;
use std::env;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Shows the status of files in the vault
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct FileStatus {
    path: String,
    status: String,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Status { json } => {
            let current_dir = env::current_dir().expect("Failed to get current directory");
            let vault = Vault::open(&current_dir).expect("Failed to open vault");
            let files = vault.files();
            let state_root = current_dir.join(".mdcrdt").join("state");

            let mut statuses = Vec::new();
            for file in files {
                let relative_path = file
                    .strip_prefix(&current_dir)
                    .unwrap_or(&file)
                    .to_string_lossy()
                    .into_owned();
                let mut state_path = state_root.join(&relative_path);
                state_path.set_extension("mdcrdt");
                let status = if state_path.exists() {
                    "Tracked"
                } else {
                    "Untracked"
                };
                statuses.push(FileStatus {
                    path: relative_path,
                    status: status.to_string(),
                });
            }

            let is_dirty = statuses.iter().any(|s| s.status == "Untracked");

            if *json {
                let output = serde_json::json!({
                    "files": statuses,
                });
                println!("{}", serde_json::to_string_pretty(&output).unwrap());
                if is_dirty {
                    std::process::exit(1);
                }
                std::process::exit(0);
            } else if !is_dirty {
                println!("Vault is clean.");
                std::process::exit(0);
            } else {
                for file in statuses {
                    if file.status == "Untracked" {
                        println!("Untracked: {}", file.path);
                    }
                }
                std::process::exit(1);
            }
        }
    }
}
