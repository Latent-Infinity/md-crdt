use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn collect_files(path: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let target = args.next().unwrap_or_else(|| {
        eprintln!("usage: parser_perf <file-or-dir> [repeat]");
        std::process::exit(2);
    });
    let repeat: usize = args
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    let mut files = Vec::new();
    collect_files(Path::new(&target), &mut files)?;
    files.sort();

    let mut rows = Vec::with_capacity(files.len());
    for path in files {
        let data = fs::read(&path)?;
        let input = String::from_utf8_lossy(&data);

        let start = Instant::now();
        for _ in 0..repeat {
            let doc = md_crdt_doc::Parser::parse(&input);
            let _ = doc.serialize(md_crdt_doc::EquivalenceMode::Structural);
        }
        let elapsed = start.elapsed();
        rows.push((elapsed, data.len(), path));
    }

    rows.sort_by_key(|(elapsed, _, _)| std::cmp::Reverse(*elapsed));

    println!("elapsed_ms\tsize_bytes\tpath");
    for (elapsed, size, path) in rows.iter().take(50) {
        println!(
            "{}\t{}\t{}",
            elapsed.as_secs_f64() * 1000.0,
            size,
            path.display()
        );
    }

    Ok(())
}
