use clap::{Parser, Subcommand};
use md_crdt::filesync::{Vault, VaultSession};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    author,
    version,
    about = "Manage collaborative Markdown vaults",
    long_about = None
)]
struct Cli {
    /// Vault root containing all Markdown files managed by this command
    #[arg(long, global = true, value_name = "PATH", default_value = ".")]
    vault: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show fingerprint tracking status for all Markdown files
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Initialize metadata for the vault
    Init,
    /// Record current Markdown fingerprints for status tracking
    Flush,
    /// Ingest all Markdown files into per-file collaborative sessions
    Ingest,
    /// Ingest all Markdown files and report whether operations were emitted
    Sync,
}

#[derive(Serialize)]
struct FileStatus {
    path: String,
    status: String,
}

fn main() {
    let cli = Cli::parse();

    match &cli.command {
        Commands::Status { json } => status_command(&cli.vault, *json),
        Commands::Init => init_command(&cli.vault),
        Commands::Flush => flush_command(&cli.vault),
        Commands::Ingest => ingest_command(&cli.vault),
        Commands::Sync => sync_command(&cli.vault),
    }
}

fn status_command(vault_root: &Path, json: bool) {
    let vault = match Vault::open(vault_root) {
        Ok(vault) => vault,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    let files = vault.files();
    let state_root = vault.path.join(".mdcrdt").join("state");

    let mut statuses = Vec::new();
    for file in files {
        let relative_path = file
            .strip_prefix(&vault.path)
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

    if json {
        let output = serde_json::json!({
            "files": statuses,
        });
        // serde_json::to_string_pretty only fails for non-serializable types,
        // which is impossible for json! macro output
        match serde_json::to_string_pretty(&output) {
            Ok(pretty) => println!("{pretty}"),
            Err(err) => {
                eprintln!("Error: {err}");
                std::process::exit(1);
            }
        }
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

fn init_command(vault_root: &Path) {
    let vault = match Vault::open(vault_root) {
        Ok(vault) => vault,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = vault.init() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
    println!("Initialized vault");
}

fn flush_command(vault_root: &Path) {
    let vault = match Vault::open(vault_root) {
        Ok(vault) => vault,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    if let Err(err) = vault.flush() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
    println!("Flushed state");
}

fn ingest_command(vault_root: &Path) {
    let mut session = match VaultSession::open(vault_root) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    let report = match session.ingest_all() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    if report.files_changed == 0 {
        println!("Ingest complete: no changes");
    } else {
        println!(
            "Ingest complete: {} file(s) changed, {} op(s)",
            report.files_changed, report.ops_emitted
        );
    }
}

fn sync_command(vault_root: &Path) {
    let mut session = match VaultSession::open(vault_root) {
        Ok(s) => s,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    let report = match session.ingest_all() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };

    if report.files_changed == 0 {
        println!("Sync complete: clean");
        std::process::exit(0);
    } else {
        println!(
            "Sync complete: {} file(s) changed, {} op(s)",
            report.files_changed, report.ops_emitted
        );
        std::process::exit(2);
    }
}
