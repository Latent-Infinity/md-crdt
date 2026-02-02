use clap::{Parser, Subcommand};
use md_crdt_filesync::{IngestResult, Vault};
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
    /// Initialize a vault
    Init,
    /// Flush current state to storage
    Flush,
    /// Ingest changes from files
    Ingest,
    /// Sync (ingest + report dirty)
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
        Commands::Status { json } => status_command(*json),
        Commands::Init => init_command(),
        Commands::Flush => flush_command(),
        Commands::Ingest => ingest_command(),
        Commands::Sync => sync_command(),
    }
}

fn status_command(json: bool) {
    let current_dir = env::current_dir().expect("Failed to get current directory");
    let vault = match Vault::open(&current_dir) {
        Ok(vault) => vault,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
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

    if json {
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

fn init_command() {
    let current_dir = env::current_dir().expect("Failed to get current directory");
    let vault = match Vault::open(&current_dir) {
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

fn flush_command() {
    let current_dir = env::current_dir().expect("Failed to get current directory");
    let vault = match Vault::open(&current_dir) {
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

fn ingest_command() {
    let current_dir = env::current_dir().expect("Failed to get current directory");
    let vault = match Vault::open(&current_dir) {
        Ok(vault) => vault,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    let result = match vault.ingest() {
        Ok(result) => result,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    match result {
        IngestResult::NoOp => println!("Ingest complete: no changes"),
        IngestResult::Changed => println!("Ingest complete: changes detected"),
    }
}

fn sync_command() {
    let current_dir = env::current_dir().expect("Failed to get current directory");
    let vault = match Vault::open(&current_dir) {
        Ok(vault) => vault,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    let result = match vault.ingest() {
        Ok(result) => result,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };

    match result {
        IngestResult::NoOp => {
            println!("Sync complete: clean");
            std::process::exit(0);
        }
        IngestResult::Changed => {
            println!("Sync complete: changes detected");
            std::process::exit(2);
        }
    }
}
