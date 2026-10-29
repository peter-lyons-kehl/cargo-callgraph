// SPDX-License-Identifier: AGPL-3.0
//
use cargo_callgraph::{collector, graph, output, project, traverse};

use std::env;
use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::graph::CallGraph;

#[derive(Parser, Debug)]
#[command(name = "cargo-callgraph")]
#[command(bin_name = "cargo callgraph")]
#[command(about = "Generate and traverse callgraphs for Rust workspaces")]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(flatten)]
    generate: GenerateArgs,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Traverse a pre-generated callgraph JSON file
    Traverse(TraverseArgs),
    /// Start the MCP server (stdio transport)
    Mcp,
}

#[derive(Parser, Debug, Default)]
struct GenerateArgs {
    /// Path to Cargo.toml (auto-detected if omitted)
    #[arg(long)]
    manifest_path: Option<PathBuf>,

    /// Output JSON file
    #[arg(long)]
    json: Option<PathBuf>,

    /// Output Graphviz DOT file
    #[arg(long)]
    dot: Option<PathBuf>,

    /// Follow into dependency crates
    #[arg(long)]
    with_deps: bool,
}

#[derive(Parser, Debug)]
struct TraverseArgs {
    /// Path to the JSON callgraph file
    #[arg(long, short)]
    graph: PathBuf,

    /// Starting node(s), by name or qualified path
    #[arg(long, short = 'i')]
    input: Vec<String>,

    /// Maximum traversal depth
    #[arg(long, short = 'n', default_value = "10")]
    depth: usize,

    /// Traverse outgoing edges (callees); omit for incoming (callers)
    #[arg(long, short = 'o')]
    outgoing: bool,
}

fn find_manifest() -> Option<PathBuf> {
    let mut dir = env::current_dir().ok()?;
    loop {
        let candidate = dir.join("Cargo.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn run_generate(args: GenerateArgs) -> Result<()> {
    let manifest = match args.manifest_path {
        Some(p) => p,
        None => find_manifest()
            .ok_or_else(|| anyhow::anyhow!("Could not find Cargo.toml in current directory or any parent directory. Use --manifest-path to specify one."))?,
    };

    eprintln!("Loading workspace: {}", manifest.display());
    let project = project::load_project(&manifest)?;
    eprintln!(
        "Loaded {} workspace crates",
        project.workspace_crates.len()
    );

    let mut graph = CallGraph::new();
    collector::build(&project, &mut graph)?;

    eprintln!(
        "Callgraph complete: {} nodes, {} edges",
        graph.node_count(),
        graph.edge_count()
    );

    if let Some(path) = args.json {
        output::json::write(&graph, &path)?;
        eprintln!("Wrote JSON: {}", path.display());
    }

    if let Some(path) = args.dot {
        output::dot::write(&graph, &path)?;
        eprintln!("Wrote DOT: {}", path.display());
    }

    Ok(())
}

fn run_traverse(args: TraverseArgs) -> Result<()> {
    let graph = traverse::TraversalGraph::load(&args.graph)?;

    let mut start_nodes = Vec::new();
    for pattern in &args.input {
        let matches = graph.find_nodes(pattern);
        if matches.is_empty() {
            eprintln!("Warning: no node matched pattern: {}", pattern);
        } else {
            for m in matches {
                start_nodes.push(m.id.clone());
            }
        }
    }

    if start_nodes.is_empty() {
        anyhow::bail!("No starting nodes found. Use -i to specify input node(s).");
    }

    let mut seen = std::collections::HashSet::new();
    start_nodes.retain(|id| seen.insert(id.clone()));

    let trees = graph.traverse(&start_nodes, args.depth, args.outgoing);
    traverse::print_tree(&trees, args.outgoing);

    Ok(())
}

async fn run_mcp() -> Result<()> {
    use rmcp::{ServiceExt, transport::stdio};
    let server = cargo_callgraph::mcp::server::CallgraphMcpServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

fn strip_cargo_subcommand(mut args: Vec<String>) -> Vec<String> {
    if args.get(1).map(|a| a == "callgraph").unwrap_or(false) {
        args.remove(1);
    }
    args
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = strip_cargo_subcommand(env::args().collect());
    let cli = Cli::parse_from(args);

    match cli.command {
        Some(Commands::Traverse(t)) => run_traverse(t),
        Some(Commands::Mcp) => run_mcp().await,
        None => run_generate(cli.generate),
    }
}
