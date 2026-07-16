//! Onyx build tasks. Run as `cargo xtask <command>`.
//!
//! Commands:
//! - `corpus <dir> [notes]` — write a deterministic synthetic vault
//! - `bench-index <dir>`    — time a full index build + queries over a vault
//!
//! `bench-index` prints machine-parsable `metric=value` lines; the CI perf
//! gate (criterion baselines land with the app shell) consumes these.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use onyx_core::{Index, LinkGraph, QuickSwitcher, RealFs, SearchIndex, Vault, VaultConfig};
use onyx_testkit::CorpusConfig;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("corpus") => corpus(&args[1..]),
        Some("bench-index") => bench_index(&args[1..]),
        _ => {
            eprintln!("usage: cargo xtask <corpus <dir> [notes] | bench-index <dir>>");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn corpus(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("corpus: missing <dir>")?;
    let notes: usize = args
        .get(1)
        .map(|raw| raw.parse().map_err(|_| "corpus: invalid note count"))
        .transpose()?
        .unwrap_or(CorpusConfig::BENCH_100K.notes);

    let config = CorpusConfig {
        notes,
        ..CorpusConfig::BENCH_100K
    };
    let started = Instant::now();
    let written =
        onyx_testkit::write_to_dir(Path::new(dir), config).map_err(|error| error.to_string())?;
    println!(
        "wrote {written} notes to {dir} in {:.2}s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

fn bench_index(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("bench-index: missing <dir>")?;
    let vault = Vault::new(Arc::new(RealFs::new(dir)), VaultConfig::default());

    // Full metadata index build.
    let started = Instant::now();
    let mut index = Index::open_in_memory([0; 16]).map_err(|error| error.to_string())?;
    index.rebuild(&vault).map_err(|error| error.to_string())?;
    let notes = index.note_count().map_err(|error| error.to_string())?;
    println!("notes={notes}");
    println!("index_rebuild_ms={}", started.elapsed().as_millis());

    // Reconcile over a quiet vault (stat-only fast path).
    let started = Instant::now();
    index.reconcile(&vault).map_err(|error| error.to_string())?;
    println!("reconcile_quiet_ms={}", started.elapsed().as_millis());

    // Link graph construction.
    let started = Instant::now();
    let graph = LinkGraph::build(&index).map_err(|error| error.to_string())?;
    println!(
        "graph_nodes={} graph_edges={}",
        graph.len(),
        graph.edge_count()
    );
    println!("graph_build_ms={}", started.elapsed().as_millis());

    // Full-text index build + a query.
    let started = Instant::now();
    let mut search = SearchIndex::open_in_ram().map_err(|error| error.to_string())?;
    let (nodes, _) = index.graph_data().map_err(|error| error.to_string())?;
    for node in &nodes {
        let record = index
            .note(node.id)
            .map_err(|error| error.to_string())?
            .ok_or("note vanished mid-bench")?;
        if !record.is_markdown {
            continue;
        }
        let body = vault
            .read_text(&record.path)
            .map_err(|error| error.to_string())?;
        search
            .upsert(node.id, record.path.as_str(), &record.title, &body, &[])
            .map_err(|error| error.to_string())?;
    }
    search.commit().map_err(|error| error.to_string())?;
    println!("fts_build_ms={}", started.elapsed().as_millis());

    let started = Instant::now();
    let hits = search
        .search("privacy encryption", 20)
        .map_err(|error| error.to_string())?;
    println!("fts_query_hits={}", hits.len());
    println!("fts_query_us={}", started.elapsed().as_micros());

    // Quick-switcher build + a keystroke-shaped query.
    let mut quick = QuickSwitcher::new();
    for node in &nodes {
        if let Some(record) = index.note(node.id).map_err(|error| error.to_string())? {
            quick.upsert(node.id, &record.title, record.path.as_str(), &[]);
        }
    }
    let started = Instant::now();
    let hits = quick.query("note 42", 20);
    println!("quick_query_hits={}", hits.len());
    println!("quick_query_us={}", started.elapsed().as_micros());

    Ok(())
}
