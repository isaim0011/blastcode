use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use serde_json::{json, Value};

use blastcode::tools::{format_event, Engine};
use blastcode::mcp;

#[derive(Parser)]
#[command(
    name = "blast",
    version,
    about = "BlastCode: code-graph MCP server and CLI for AI coding agents"
)]
struct Cli {
    /// Workspace root.
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    /// Override the index database location.
    #[arg(long, global = true)]
    db: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Build or update the index.
    Index {
        /// Re-hash every file, ignoring stored mtimes.
        #[arg(long)]
        full: bool,
    },
    /// Run the MCP server on stdio (indexes and watches in the background).
    Serve,
    /// Watch the workspace and print what changes, live.
    Watch {
        /// Poll interval in milliseconds.
        #[arg(long, default_value_t = 1000)]
        interval: u64,
    },
    /// Print the annotated workspace map.
    Map {
        path: Option<String>,
        #[arg(long, default_value_t = 3)]
        depth: u64,
    },
    /// Everything about a file: skeleton, imports, dependents, recent changes.
    Context { file: String },
    /// Print a token-lean outline of a file.
    Skeleton { file: String },
    /// Print the exact source of one symbol.
    Source {
        symbol: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value_t = 0)]
        context: u64,
    },
    /// Fuzzy symbol search.
    Search {
        query: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 20)]
        limit: u64,
    },
    /// Callers, type usages and callees of a symbol.
    Trace {
        symbol: String,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value = "both")]
        direction: String,
    },
    /// Impact analysis (working tree vs git HEAD, or --stdin for proposed content).
    Impact {
        file: String,
        /// Read the proposed full file content from stdin.
        #[arg(long)]
        stdin: bool,
    },
    /// Structural query over the symbol index.
    Query {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        path: Option<String>,
        #[arg(long)]
        calls: Option<String>,
        #[arg(long)]
        called_by: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u64,
    },
    /// Recent workspace change journal.
    Changes {
        #[arg(long)]
        since: Option<i64>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: u64,
    },
    /// Index statistics.
    Stats,
    /// Find affected test files and test symbols for a file or symbol.
    Tests {
        file: Option<String>,
        #[arg(long)]
        symbol: Option<String>,
    },
    /// Pre-flight validation of proposed code edits before saving to disk.
    Verify {
        file: String,
        /// Proposed file content string.
        #[arg(long)]
        patch: Option<String>,
        /// Read proposed file content from stdin.
        #[arg(long)]
        stdin: bool,
    },
    /// Mine Git history for files frequently committed together.
    Coupled {
        file: String,
        #[arg(long, default_value_t = 100)]
        depth: u64,
        #[arg(long, default_value_t = 15)]
        limit: u64,
    },
    /// Detect dead, unreferenced, or orphaned symbols.
    Dead {
        #[arg(long)]
        prefix: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: u64,
    },
}

fn run_tool(engine: &mut Engine, name: &str, args: Value) -> Result<()> {
    println!("{}", engine.call(name, &args)?);
    Ok(())
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("blast: error: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let mut engine = Engine::open(&cli.root, cli.db.clone())?;
    match cli.cmd {
        Cmd::Serve => return mcp::serve(engine),
        Cmd::Index { full } => {
            let st = engine.index_now(full)?;
            println!("{}", serde_json::to_string_pretty(&st)?);
            return Ok(());
        }
        Cmd::Watch { interval } => {
            engine.index_now(false)?;
            // Report only changes from now on.
            engine.poll_events()?;
            eprintln!("blast: watching {} (Ctrl-C to stop)", engine.root.display());
            loop {
                std::thread::sleep(Duration::from_millis(interval.max(200)));
                engine.index_now(false)?;
                for e in engine.poll_events()? {
                    println!("{}", format_event(&e));
                }
            }
        }
        _ => {
            engine.index_now(false)?;
        }
    }
    match cli.cmd {
        Cmd::Map { path, depth } => run_tool(&mut engine, "get_workspace_map", json!({"path": path.unwrap_or_default(), "depth": depth})),
        Cmd::Context { file } => run_tool(&mut engine, "get_file_context", json!({"file_path": file})),
        Cmd::Skeleton { file } => run_tool(&mut engine, "get_file_skeleton", json!({"file_path": file})),
        Cmd::Source { symbol, file, context } => run_tool(
            &mut engine,
            "get_symbol_source",
            json!({"symbol_name": symbol, "file_path": file.unwrap_or_default(), "context_lines": context}),
        ),
        Cmd::Search { query, kind, limit } => run_tool(&mut engine, "search_symbols", json!({"query": query, "kind": kind.unwrap_or_default(), "limit": limit})),
        Cmd::Trace { symbol, file, direction } => run_tool(&mut engine, "trace_symbol", json!({"symbol_name": symbol, "file_path": file.unwrap_or_default(), "direction": direction})),
        Cmd::Impact { file, stdin } => {
            let mut args = json!({"file_path": file});
            if stdin {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                args["new_source"] = json!(s);
            }
            run_tool(&mut engine, "get_impact_radius", args)
        }
        Cmd::Query { kind, name, path, calls, called_by, limit } => run_tool(
            &mut engine,
            "query_graph",
            json!({"kind": kind.unwrap_or_default(), "name": name.unwrap_or_default(), "path_prefix": path.unwrap_or_default(),
                   "calls": calls.unwrap_or_default(), "called_by": called_by.unwrap_or_default(), "limit": limit}),
        ),
        Cmd::Changes { since, file, limit } => {
            let mut args = json!({"file_path": file.unwrap_or_default(), "limit": limit});
            if let Some(s) = since {
                args["since"] = json!(s);
            }
            run_tool(&mut engine, "get_workspace_changes", args)
        }
        Cmd::Stats => {
            println!("{}", serde_json::to_string_pretty(&engine.stats()?)?);
            Ok(())
        }
        Cmd::Tests { file, symbol } => {
            let mut args = json!({});
            if let Some(f) = file {
                args["file_path"] = json!(f);
            }
            if let Some(s) = symbol {
                args["symbol_name"] = json!(s);
            }
            run_tool(&mut engine, "get_affected_tests", args)
        }
        Cmd::Verify { file, patch, stdin } => {
            let content = if let Some(p) = patch {
                p
            } else if stdin {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                std::fs::read_to_string(&file)?
            };
            run_tool(&mut engine, "verify_patch", json!({"file_path": file, "patch": content}))
        }
        Cmd::Coupled { file, depth, limit } => run_tool(
            &mut engine,
            "get_co_changed_files",
            json!({"file_path": file, "commit_depth": depth, "limit": limit}),
        ),
        Cmd::Dead { prefix, limit } => {
            let mut args = json!({"limit": limit});
            if let Some(p) = prefix {
                args["path_prefix"] = json!(p);
            }
            run_tool(&mut engine, "find_dead_code", args)
        }
        Cmd::Serve | Cmd::Index { .. } | Cmd::Watch { .. } => unreachable!(),
    }
}
