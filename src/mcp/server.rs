// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rmcp::{
    handler::server::wrapper::Parameters,
    model::*,
    schemars, tool, tool_handler, tool_router, ServerHandler,
};

use crate::graph::CallGraph;
use crate::traverse::TraversalGraph;

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GenerateParams {
    /// Path to Cargo.toml (auto-detected if omitted)
    pub manifest_path: Option<String>,
    /// Path to a pre-generated JSON callgraph (skips generation if provided)
    pub json_path: Option<String>,
    /// Force regeneration even if cached
    pub reload: Option<bool>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TraverseParams {
    /// Path to Cargo.toml or previously-used manifest path
    pub manifest_path: Option<String>,
    /// Node name or qualified path pattern to start from
    pub node_pattern: String,
    /// Maximum traversal depth (default: 3)
    pub depth: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct FindPathsParams {
    /// Path to Cargo.toml or previously-used manifest path
    pub manifest_path: Option<String>,
    /// Starting node pattern
    pub from_pattern: String,
    /// Target node pattern
    pub to_pattern: String,
    /// Maximum path length (default: 5)
    pub max_depth: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    /// Path to Cargo.toml or previously-used manifest path
    pub manifest_path: Option<String>,
    /// Pattern to search for in node names and qualified paths
    pub pattern: String,
}

#[derive(Debug, Clone)]
pub struct CallgraphMcpServer {
    cache: Arc<Mutex<HashMap<String, TraversalGraph>>>,
}

impl CallgraphMcpServer {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn ensure_graph(
        &self,
        manifest_path: &Option<String>,
        json_path: &Option<String>,
        reload: bool,
    ) -> Result<TraversalGraph, String> {
        let key = if let Some(json) = json_path {
            json.clone()
        } else {
            let manifest = match manifest_path {
                Some(p) => PathBuf::from(p),
                None => find_manifest()
                    .ok_or("Could not find Cargo.toml. Use manifest_path.".to_string())?,
            };
            manifest.to_string_lossy().to_string()
        };

        // 1. In-memory cache
        if !reload {
            if let Ok(mem) = self.cache.lock() {
                if let Some(g) = mem.get(&key) {
                    return Ok(g.clone());
                }
            }
        }

        let graph = if let Some(json) = json_path {
            TraversalGraph::load(Path::new(&json)).map_err(|e| e.to_string())?
        } else {
            let manifest = PathBuf::from(&key);

            // 2. Disk cache (mtime-validated)
            if !reload && crate::cache::is_valid(&manifest) {
                if let Some(json_str) = crate::cache::read_graph(&manifest) {
                    let tmp = std::env::temp_dir().join(format!("callgraph-mcp-{}.json", std::process::id()));
                    if std::fs::write(&tmp, &json_str).is_ok() {
                        if let Ok(g) = TraversalGraph::load(&tmp) {
                            let _ = std::fs::remove_file(&tmp);
                            if let Ok(mut mem) = self.cache.lock() {
                                mem.insert(key, g.clone());
                            }
                            return Ok(g);
                        }
                    }
                }
            }

            // 3. Generate from scratch
            let project = crate::project::load_project(&manifest).map_err(|e| e.to_string())?;
            let mut graph = CallGraph::new();
            crate::collector::build(&project, &mut graph).map_err(|e| e.to_string())?;
            let json_str =
                crate::output::json::write_to_string(&graph).map_err(|e| e.to_string())?;

            // Save to disk cache
            let _ = crate::cache::write(&manifest, &json_str);

            // Load into TraversalGraph
            let tmp = std::env::temp_dir().join(format!("callgraph-mcp-{}.json", std::process::id()));
            std::fs::write(&tmp, &json_str).map_err(|e| e.to_string())?;
            let result = TraversalGraph::load(&tmp).map_err(|e| e.to_string())?;
            let _ = std::fs::remove_file(&tmp);
            result
        };

        if let Ok(mut mem) = self.cache.lock() {
            mem.insert(key, graph.clone());
        }

        Ok(graph)
    }

    fn with_graph<F, T>(&self, manifest_path: &Option<String>, f: F) -> Result<T, String>
    where
        F: FnOnce(&TraversalGraph) -> Result<T, String>,
    {
        let graph = self.ensure_graph(manifest_path, &None, false)?;
        f(&graph)
    }
}

fn find_manifest() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
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

fn format_node(node: &crate::graph::Node) -> String {
    let mut lines = vec![
        format!("Name: {}", node.name),
        format!("Path: {}", node.qualified_path),
        format!("Crate: {}", node.crate_name),
    ];
    if !node.module_path.is_empty() {
        lines.push(format!("Module: {}", node.module_path.join("::")));
    }
    if let Some(ref file) = node.file_path {
        if let Some(line) = node.line {
            lines.push(format!("Location: {}:{}", file, line + 1));
        } else {
            lines.push(format!("File: {}", file));
        }
    }
    lines.push(format!("Workspace: {}", node.is_workspace));
    lines.push(format!("Trait method: {}", node.is_trait_method));
    lines.join("\n")
}

fn tree_to_string(nodes: &[crate::traverse::TreeNode], _outgoing: bool) -> String {
    let mut output = Vec::new();
    for (i, root) in nodes.iter().enumerate() {
        tree_node_to_lines(root, &[], i == nodes.len() - 1, &mut output);
    }
    output.join("\n")
}

fn tree_node_to_lines(
    node: &crate::traverse::TreeNode,
    prefix: &[bool],
    is_last: bool,
    output: &mut Vec<String>,
) {
    let label = if node.node.qualified_path.contains("::") {
        node.node.qualified_path.clone()
    } else if let Some(line) = node.node.line {
        format!("{} (L{})", node.node.qualified_path, line + 1)
    } else {
        node.node.qualified_path.clone()
    };

    let line = if prefix.is_empty() {
        label
    } else {
        let branch = if is_last { "└── " } else { "├── " };
        let indent: String = prefix
            .iter()
            .map(|&last| if last { "    " } else { "│   " })
            .collect();
        format!("{}{}{}", indent, branch, label)
    };
    output.push(line);

    let mut new_prefix = prefix.to_vec();
    new_prefix.push(is_last);

    for (i, child) in node.children.iter().enumerate() {
        let child_is_last = i == node.children.len() - 1;
        let mut child_prefix = new_prefix.clone();

        let edge_kind = node.edge_kinds.get(&child.node.id.0);
        let has_annotation = matches!(
            edge_kind,
            Some(crate::graph::EdgeKind::StaticTraitCall)
                | Some(crate::graph::EdgeKind::TraitDefCall)
                | Some(crate::graph::EdgeKind::DynamicDispatch)
        );

        if has_annotation {
            let branch = if child_is_last { "└── " } else { "├── " };
            let indent: String = child_prefix
                .iter()
                .map(|&last| if last { "    " } else { "│   " })
                .collect();
            let ann = match edge_kind {
                Some(crate::graph::EdgeKind::StaticTraitCall) => "impl",
                Some(crate::graph::EdgeKind::TraitDefCall) => "trait",
                Some(crate::graph::EdgeKind::DynamicDispatch) => "dyn",
                _ => "",
            };
            output.push(format!("{}{}[{}]", indent, branch, ann));
            child_prefix.push(child_is_last);
        }

        tree_node_to_lines(child, &child_prefix, child_is_last, output);
    }
}

fn find_paths_bfs(
    graph: &TraversalGraph,
    from: &crate::graph::NodeId,
    to: &crate::graph::NodeId,
    max_depth: usize,
) -> Vec<Vec<String>> {
    let mut results = Vec::new();
    let mut queue: VecDeque<(crate::graph::NodeId, Vec<String>)> = VecDeque::new();
    queue.push_back((from.clone(), vec![format_label_by_id(graph, from)]));

    while let Some((current, path)) = queue.pop_front() {
        if path.len() > max_depth {
            continue;
        }
        if &current == to && path.len() > 1 {
            results.push(path);
            continue;
        }
        if let Some(neighbors) = graph.outgoing.get(&current) {
            for (next_id, _kind) in neighbors {
                if !path.contains(&format_label_by_id(graph, next_id)) {
                    let mut new_path = path.clone();
                    new_path.push(format_label_by_id(graph, next_id));
                    queue.push_back((next_id.clone(), new_path));
                }
            }
        }
    }

    results
}

fn format_label_by_id(graph: &TraversalGraph, id: &crate::graph::NodeId) -> String {
    if let Some(node) = graph.nodes.get(id) {
        if node.qualified_path.contains("::") {
            node.qualified_path.clone()
        } else if let Some(line) = node.line {
            format!("{} (L{})", node.qualified_path, line + 1)
        } else {
            node.qualified_path.clone()
        }
    } else {
        id.0.clone()
    }
}

#[tool_router]
impl CallgraphMcpServer {
    #[tool(description = "Generate or load a callgraph for a Rust workspace. Returns a summary of the graph.")]
    fn generate_callgraph(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        match self.ensure_graph(&params.manifest_path, &params.json_path, params.reload.unwrap_or(false)) {
            Ok(graph) => {
                let crate_names: HashSet<_> = graph.nodes.values().map(|n| n.crate_name.clone()).collect();
                let workspace_nodes = graph.nodes.values().filter(|n| n.is_workspace).count();
                format!(
                    "Callgraph loaded:\n- Nodes: {}\n- Edges: {}\n- Crates: {}\n- Workspace nodes: {}\n\nUse traverse_outgoing, traverse_incoming, search_nodes, or get_node_details to query.",
                    graph.nodes.len(),
                    graph.outgoing.values().map(|v| v.len()).sum::<usize>(),
                    crate_names.into_iter().collect::<Vec<_>>().join(", "),
                    workspace_nodes
                )
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    #[tool(description = "Traverse outgoing edges from a node to see what it calls.")]
    fn traverse_outgoing(&self, Parameters(params): Parameters<TraverseParams>) -> String {
        let depth = params.depth.unwrap_or(3);
        self.with_graph(&params.manifest_path, |graph| {
            let matches = graph.find_nodes(&params.node_pattern);
            if matches.is_empty() {
                return Err(format!("No node matched pattern: {}", params.node_pattern));
            }
            let start_ids: Vec<_> = matches.iter().map(|m| m.id.clone()).collect();
            let trees = graph.traverse(&start_ids, depth, true);
            if trees.is_empty() {
                return Ok("No outgoing edges found.".to_string());
            }
            Ok(tree_to_string(&trees, true))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Traverse incoming edges to a node to see what calls it.")]
    fn traverse_incoming(&self, Parameters(params): Parameters<TraverseParams>) -> String {
        let depth = params.depth.unwrap_or(3);
        self.with_graph(&params.manifest_path, |graph| {
            let matches = graph.find_nodes(&params.node_pattern);
            if matches.is_empty() {
                return Err(format!("No node matched pattern: {}", params.node_pattern));
            }
            let start_ids: Vec<_> = matches.iter().map(|m| m.id.clone()).collect();
            let trees = graph.traverse(&start_ids, depth, false);
            if trees.is_empty() {
                return Ok("No incoming edges found.".to_string());
            }
            Ok(tree_to_string(&trees, false))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Find all call paths between two nodes up to a maximum depth.")]
    fn find_call_paths(&self, Parameters(params): Parameters<FindPathsParams>) -> String {
        let max_depth = params.max_depth.unwrap_or(5);
        self.with_graph(&params.manifest_path, |graph| {
            let from_matches = graph.find_nodes(&params.from_pattern);
            let to_matches = graph.find_nodes(&params.to_pattern);
            if from_matches.is_empty() {
                return Err(format!("No node matched 'from' pattern: {}", params.from_pattern));
            }
            if to_matches.is_empty() {
                return Err(format!("No node matched 'to' pattern: {}", params.to_pattern));
            }

            let mut all_paths = Vec::new();
            for from in &from_matches {
                for to in &to_matches {
                    let paths = find_paths_bfs(graph, &from.id, &to.id, max_depth);
                    for path in paths {
                        all_paths.push(path.join(" -> "));
                    }
                }
            }

            if all_paths.is_empty() {
                Ok("No paths found within depth limit.".to_string())
            } else {
                Ok(format!(
                    "Found {} path(s):\n{}",
                    all_paths.len(),
                    all_paths.join("\n")
                ))
            }
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Search for nodes by name or qualified path pattern.")]
    fn search_nodes(&self, Parameters(params): Parameters<SearchParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let matches = graph.find_nodes(&params.pattern);
            if matches.is_empty() {
                return Ok("No nodes found.".to_string());
            }
            let lines: Vec<_> = matches
                .iter()
                .map(|n| {
                    if n.qualified_path.contains("::") {
                        n.qualified_path.clone()
                    } else if let Some(line) = n.line {
                        format!("{} (L{})", n.qualified_path, line + 1)
                    } else {
                        n.qualified_path.clone()
                    }
                })
                .collect();
            Ok(format!(
                "Found {} node(s):\n{}",
                lines.len(),
                lines.join("\n")
            ))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Get detailed information about a specific node.")]
    fn get_node_details(&self, Parameters(params): Parameters<SearchParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let matches = graph.find_nodes(&params.pattern);
            if matches.is_empty() {
                return Err(format!("No node matched pattern: {}", params.pattern));
            }
            if matches.len() > 1 {
                let lines: Vec<_> = matches
                    .iter()
                    .map(|n| {
                        if n.qualified_path.contains("::") {
                            n.qualified_path.clone()
                        } else if let Some(line) = n.line {
                            format!("{} (L{})", n.qualified_path, line + 1)
                        } else {
                            n.qualified_path.clone()
                        }
                    })
                    .collect();
                return Ok(format!(
                    "Multiple nodes matched ({}). Be more specific:\n{}",
                    matches.len(),
                    lines.join("\n")
                ));
            }
            Ok(format_node(matches[0]))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "List crates present in the loaded callgraph.")]
    fn list_crates(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let mut crates: Vec<_> = graph
                .nodes
                .values()
                .map(|n| n.crate_name.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            crates.sort();
            Ok(format!("Crates in workspace:\n{}", crates.join("\n")))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Summarize the callgraph: entry points, deepest chains, hotspots, and leaf functions.")]
    fn summarize_graph(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let summary = graph.summarize();
            let mut lines = vec![
                format!("Nodes: {} | Edges: {}", summary.total_nodes, summary.total_edges),
                format!("Max call depth: {}", summary.max_depth.saturating_sub(1)),
                format!("Most connected node(s) (degree {}): {}", summary.max_degree, summary.hotspots.join(", ")),
                format!("Entry point(s): {}", if summary.entry_points.is_empty() { "none".into() } else { summary.entry_points.join(", ") }),
                format!("Leaf function(s): {}", if summary.leaves.is_empty() { "none".into() } else { summary.leaves.join(", ") }),
            ];
            if !summary.deepest_chain_starts.is_empty() {
                lines.push(format!("Deepest chain(s) start from: {}", summary.deepest_chain_starts.join(", ")));
            }
            Ok(lines.join("\n"))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Explore the workspace structure grouped by module/crate.")]
    fn explore_workspace(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let groups = graph.explore_modules();
            if groups.is_empty() {
                return Ok("No workspace modules found.".to_string());
            }
            let mut modules: Vec<_> = groups.keys().cloned().collect();
            modules.sort();
            let mut lines = Vec::new();
            for m in modules {
                let funcs = &groups[&m];
                lines.push(format!("\n[{}] — {} function(s)", m, funcs.len()));
                for f in funcs.iter().take(10) {
                    lines.push(format!("  • {}", f));
                }
                if funcs.len() > 10 {
                    lines.push(format!("  ... and {} more", funcs.len() - 10));
                }
            }
            Ok(lines.join("\n"))
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Find workspace functions with no callers (potential dead code).")]
    fn find_dead_code(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let dead = graph.find_dead_code();
            if dead.is_empty() {
                Ok("No orphaned workspace functions found.".to_string())
            } else {
                Ok(format!("Found {} orphaned workspace function(s):\n{}", dead.len(), dead.join("\n")))
            }
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Find cycles (recursive call loops) in the workspace graph.")]
    fn find_cycles(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let cycles = graph.find_cycles(10);
            if cycles.is_empty() {
                Ok("No cycles found within depth 10.".to_string())
            } else {
                let lines: Vec<_> = cycles.iter().map(|c| c.join(" → ")).collect();
                Ok(format!("Found {} cycle(s):\n{}", lines.len(), lines.join("\n")))
            }
        })
        .unwrap_or_else(|e| e)
    }

    #[tool(description = "Find bottleneck functions (high in-degree) in the workspace.")]
    fn find_bottlenecks(&self, Parameters(params): Parameters<GenerateParams>) -> String {
        self.with_graph(&params.manifest_path, |graph| {
            let results = graph.find_bottlenecks(3);
            if results.is_empty() {
                Ok("No bottlenecks found (min in-degree: 3).".to_string())
            } else {
                let lines: Vec<_> = results.iter().map(|(name, deg)| format!("{} (in-degree: {})", name, deg)).collect();
                Ok(format!("Found {} bottleneck(s):\n{}", lines.len(), lines.join("\n")))
            }
        })
        .unwrap_or_else(|e| e)
    }
}

#[tool_handler(router = Self::tool_router())]
impl ServerHandler for CallgraphMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .build()
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(
            Implementation::new("cargo-callgraph-mcp", env!("CARGO_PKG_VERSION"))
            .with_title("cargo-callgraph MCP Server")
            .with_description("MCP server for Rust callgraph analysis")
        )
        .with_instructions( "MCP server for Rust callgraph analysis. Use generate_callgraph first to load a workspace, then query with traverse_outgoing, traverse_incoming, find_call_paths, search_nodes, get_node_details, list_crates, summarize_graph, explore_workspace, find_dead_code, find_cycles, or find_bottlenecks.")
    }
}
