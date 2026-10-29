// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::graph::{EdgeKind, Node, NodeId};

#[derive(Deserialize)]
struct JsonGraph {
    nodes: Vec<Node>,
    edges: Vec<JsonEdge>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct JsonEdge {
    source: String,
    target: String,
    kind: EdgeKind,
    #[serde(default)]
    call_site_file: Option<String>,
    #[serde(default)]
    call_site_line: Option<u32>,
}

/// Lightweight graph representation for traversal, built from JSON.
#[derive(Debug, Clone)]
pub struct TraversalGraph {
    pub nodes: HashMap<NodeId, Node>,
    /// outgoing edges: node_id -> [(target_id, edge_kind)]
    pub outgoing: HashMap<NodeId, Vec<(NodeId, EdgeKind)>>,
    /// incoming edges: node_id -> [(source_id, edge_kind)]
    pub incoming: HashMap<NodeId, Vec<(NodeId, EdgeKind)>>,
}

impl TraversalGraph {
    pub fn load(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open graph: {}", path.display()))?;
        let json: JsonGraph = serde_json::from_reader(file)
            .with_context(|| format!("Failed to parse graph JSON: {}", path.display()))?;

        let mut nodes = HashMap::new();
        let mut outgoing: HashMap<NodeId, Vec<(NodeId, EdgeKind)>> = HashMap::new();
        let mut incoming: HashMap<NodeId, Vec<(NodeId, EdgeKind)>> = HashMap::new();

        for node in json.nodes {
            nodes.insert(node.id.clone(), node);
        }

        for edge in json.edges {
            let src = NodeId(edge.source);
            let dst = NodeId(edge.target);
            outgoing.entry(src.clone()).or_default().push((dst.clone(), edge.kind));
            incoming.entry(dst).or_default().push((src, edge.kind));
        }

        Ok(TraversalGraph {
            nodes,
            outgoing,
            incoming,
        })
    }

    /// Format a node the same way display tools do (e.g. "main (L8)" or "handlers::init_server").
    pub fn display_label(node: &Node) -> String {
        if node.qualified_path.contains("::") {
            node.qualified_path.clone()
        } else if let Some(line) = node.line {
            format!("{} (L{})", node.qualified_path, line + 1)
        } else {
            node.qualified_path.clone()
        }
    }

    /// Find nodes whose qualified_path, name, or display label matches the pattern.
    pub fn find_nodes(&self, pattern: &str) -> Vec<&Node> {
        let exact = pattern.strip_prefix("exact:").unwrap_or(pattern);
        self.nodes
            .values()
            .filter(|n| {
                let label = Self::display_label(n);
                n.qualified_path == exact
                    || n.name == exact
                    || label == exact
                    || n.qualified_path.contains(pattern)
                    || n.name.contains(pattern)
                    || label.contains(pattern)
            })
            .collect()
    }

    /// Traverse from the given start nodes up to `max_depth`.
    /// `outgoing` = true follows outgoing edges (callees), false follows incoming (callers).
    pub fn traverse(
        &self,
        start_ids: &[NodeId],
        max_depth: usize,
        outgoing: bool,
    ) -> Vec<TreeNode> {
        let mut visited: HashSet<NodeId> = HashSet::new();
        let mut queue: VecDeque<(NodeId, usize)> = VecDeque::new();

        for id in start_ids {
            if self.nodes.contains_key(id) {
                queue.push_back((id.clone(), 0));
                visited.insert(id.clone());
            }
        }

        let adj = if outgoing {
            &self.outgoing
        } else {
            &self.incoming
        };

        // First pass: discover all reachable nodes and build parent->children map.
        let mut children: HashMap<NodeId, Vec<(NodeId, EdgeKind)>> = HashMap::new();

        while let Some((current_id, depth)) = queue.pop_front() {
            if depth >= max_depth {
                continue;
            }
            if let Some(neighbors) = adj.get(&current_id) {
                for (neighbor_id, kind) in neighbors {
                    children
                        .entry(current_id.clone())
                        .or_default()
                        .push((neighbor_id.clone(), *kind));
                    if visited.insert(neighbor_id.clone()) {
                        queue.push_back((neighbor_id.clone(), depth + 1));
                    }
                }
            }
        }

        // Second pass: build tree from start nodes, pruning to visited set.
        let mut roots = Vec::new();
        for id in start_ids {
            if let Some(node) = self.nodes.get(id) {
                roots.push(self.build_tree_node(&node.id, &children, &visited, max_depth, 0));
            }
        }
        roots
    }

    fn build_tree_node(
        &self,
        id: &NodeId,
        children: &HashMap<NodeId, Vec<(NodeId, EdgeKind)>>,
        visited: &HashSet<NodeId>,
        max_depth: usize,
        depth: usize,
    ) -> TreeNode {
        let node = self.nodes.get(id).cloned().unwrap_or_else(|| Node {
            id: id.clone(),
            name: id.0.clone(),
            qualified_path: id.0.clone(),
            crate_name: "unknown".into(),
            module_path: Vec::new(),
            file_path: None,
            line: None,
            col: None,
            is_workspace: false,
            is_trait_method: false,
        });

        let mut child_nodes = Vec::new();
        if depth < max_depth {
            if let Some(child_ids) = children.get(id) {
                for (child_id, _kind) in child_ids {
                    if visited.contains(child_id) {
                        child_nodes.push(self.build_tree_node(
                            child_id, children, visited, max_depth, depth + 1,
                        ));
                    }
                }
            }
        }

        TreeNode { node, edge_kinds: child_ids_to_kinds(children.get(id)), children: child_nodes }
    }

    /// Compute graph analytics: entry points, deepest chain, most connected, leaves.
    pub fn summarize(&self) -> GraphSummary {
        let mut entry_points = Vec::new();
        let mut leaves = Vec::new();
        let mut max_degree = 0usize;
        let mut hotspots = Vec::new();
        let mut max_depth = 0usize;
        let mut deepest = Vec::new();

        for (id, node) in &self.nodes {
            let out_deg = self.outgoing.get(id).map(|v| v.len()).unwrap_or(0);
            let in_deg = self.incoming.get(id).map(|v| v.len()).unwrap_or(0);

            if in_deg == 0 && node.is_workspace {
                entry_points.push(Self::display_label(node));
            }
            if out_deg == 0 && node.is_workspace {
                leaves.push(Self::display_label(node));
            }
            let deg = in_deg + out_deg;
            if deg > max_degree {
                max_degree = deg;
                hotspots.clear();
                hotspots.push(Self::display_label(node));
            } else if deg == max_degree && deg > 0 {
                hotspots.push(Self::display_label(node));
            }

            // Longest outgoing path from this node
            let depth = self.longest_path_depth(id, &mut HashSet::new());
            if depth > max_depth {
                max_depth = depth;
                deepest.clear();
                deepest.push(Self::display_label(node));
            } else if depth == max_depth && depth > 0 {
                deepest.push(Self::display_label(node));
            }
        }

        entry_points.sort();
        leaves.sort();
        hotspots.sort();
        deepest.sort();

        GraphSummary {
            total_nodes: self.nodes.len(),
            total_edges: self.outgoing.values().map(|v| v.len()).sum(),
            entry_points,
            leaves,
            max_degree,
            hotspots,
            max_depth,
            deepest_chain_starts: deepest,
        }
    }

    fn longest_path_depth(&self, id: &NodeId, visited: &mut HashSet<NodeId>) -> usize {
        if !visited.insert(id.clone()) {
            return 0;
        }
        let mut max = 0;
        if let Some(neighbors) = self.outgoing.get(id) {
            for (next, _) in neighbors {
                let d = self.longest_path_depth(next, visited);
                if d > max {
                    max = d;
                }
            }
        }
        visited.remove(id);
        max + 1
    }

    /// Find workspace nodes with zero incoming edges that are not obvious entry points.
    pub fn find_dead_code(&self) -> Vec<String> {
        let mut dead = Vec::new();
        for (id, node) in &self.nodes {
            if !node.is_workspace {
                continue;
            }
            let in_deg = self.incoming.get(id).map(|v| v.len()).unwrap_or(0);
            if in_deg == 0 {
                let name = node.name.to_lowercase();
                let path = node.qualified_path.to_lowercase();
                let is_entry = name == "main"
                    || name.starts_with("test_")
                    || path.contains("::main")
                    || path.contains("#test")
                    || node.is_trait_method;
                if !is_entry {
                    dead.push(Self::display_label(node));
                }
            }
        }
        dead.sort();
        dead
    }

    /// Find simple cycles in the workspace call graph.
    pub fn find_cycles(&self, max_depth: usize) -> Vec<Vec<String>> {
        let mut cycles = Vec::new();
        let mut seen_cycles: HashSet<Vec<String>> = HashSet::new();

        for (start_id, node) in &self.nodes {
            if !node.is_workspace {
                continue;
            }
            let mut path = vec![Self::display_label(node)];
            let mut visited = HashSet::new();
            visited.insert(start_id.clone());
            self.cycle_dfs(start_id, start_id, &mut path, &mut visited, &mut cycles, &mut seen_cycles, max_depth);
        }

        cycles
    }

    fn cycle_dfs(
        &self,
        start: &NodeId,
        current: &NodeId,
        path: &mut Vec<String>,
        visited: &mut HashSet<NodeId>,
        cycles: &mut Vec<Vec<String>>,
        seen: &mut HashSet<Vec<String>>,
        max_depth: usize,
    ) {
        if path.len() > max_depth {
            return;
        }
        if let Some(neighbors) = self.outgoing.get(current) {
            for (next, _) in neighbors {
                if next == start && path.len() > 1 {
                    let mut cycle = path.clone();
                    cycle.push(Self::display_label(self.nodes.get(next).unwrap()));
                    // Normalize cycle for dedup
                    let min_pos = cycle.iter().enumerate().min_by_key(|(_, s)| *s).map(|(i, _)| i).unwrap_or(0);
                    let normalized: Vec<_> = cycle.iter().cycle().skip(min_pos).take(cycle.len()).cloned().collect();
                    if seen.insert(normalized.clone()) {
                        cycles.push(cycle);
                    }
                    continue;
                }
                if visited.insert(next.clone()) {
                    path.push(Self::display_label(self.nodes.get(next).unwrap()));
                    self.cycle_dfs(start, next, path, visited, cycles, seen, max_depth);
                    path.pop();
                    visited.remove(next);
                }
            }
        }
    }

    /// Group nodes by crate and module for exploration.
    pub fn explore_modules(&self) -> HashMap<String, Vec<String>> {
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for node in self.nodes.values() {
            if !node.is_workspace {
                continue;
            }
            let module = if node.module_path.is_empty() {
                node.crate_name.clone()
            } else {
                format!("{}::{}", node.crate_name, node.module_path.join("::"))
            };
            groups.entry(module).or_default().push(Self::display_label(node));
        }
        for list in groups.values_mut() {
            list.sort();
        }
        groups
    }

    /// Find bottleneck nodes (high in-degree workspace nodes).
    pub fn find_bottlenecks(&self, min_in_degree: usize) -> Vec<(String, usize)> {
        let mut results = Vec::new();
        for (id, node) in &self.nodes {
            if !node.is_workspace {
                continue;
            }
            let in_deg = self.incoming.get(id).map(|v| v.len()).unwrap_or(0);
            if in_deg >= min_in_degree {
                results.push((Self::display_label(node), in_deg));
            }
        }
        results.sort_by(|a, b| b.1.cmp(&a.1));
        results
    }
}

#[derive(Debug)]
pub struct GraphSummary {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub entry_points: Vec<String>,
    pub leaves: Vec<String>,
    pub max_degree: usize,
    pub hotspots: Vec<String>,
    pub max_depth: usize,
    pub deepest_chain_starts: Vec<String>,
}

fn child_ids_to_kinds(
    children: Option<&Vec<(NodeId, EdgeKind)>>,
) -> HashMap<String, EdgeKind> {
    let mut map = HashMap::new();
    if let Some(list) = children {
        for (id, kind) in list {
            map.insert(id.0.clone(), *kind);
        }
    }
    map
}

pub struct TreeNode {
    pub node: Node,
    pub edge_kinds: HashMap<String, EdgeKind>,
    pub children: Vec<TreeNode>,
}

pub fn print_tree(nodes: &[TreeNode], outgoing: bool) {
    for (i, root) in nodes.iter().enumerate() {
        print_tree_node(root, &[], i == nodes.len() - 1, outgoing);
    }
}

fn print_tree_node(node: &TreeNode, prefix: &[bool], is_last: bool, outgoing: bool) {
    let label = format_label(&node.node);
    let connector = if prefix.is_empty() {
        label
    } else {
        let branch = if is_last { "└── " } else { "├── " };
        let indent: String = prefix
            .iter()
            .map(|&last| if last { "    " } else { "│   " })
            .collect();
        format!("{}{}{}", indent, branch, label)
    };
    println!("{}", connector);

    let mut new_prefix = prefix.to_vec();
    new_prefix.push(is_last);

    for (i, child) in node.children.iter().enumerate() {
        let edge_label = edge_label_for_kind(
            node.edge_kinds.get(&child.node.id.0),
            outgoing,
        );
        let child_is_last = i == node.children.len() - 1;
        let mut child_prefix = new_prefix.clone();
        if !edge_label.is_empty() {
            // Print edge kind annotation as an intermediate node.
            let branch = if child_is_last { "└── " } else { "├── " };
            let indent: String = child_prefix
                .iter()
                .map(|&last| if last { "    " } else { "│   " })
                .collect();
            println!("{}{}[{}]", indent, branch, edge_label);
            child_prefix.push(child_is_last);
        }
        print_tree_node(child, &child_prefix, child_is_last, outgoing);
    }
}

fn format_label(node: &Node) -> String {
    if node.qualified_path.contains("::") {
        node.qualified_path.clone()
    } else if let Some(line) = node.line {
        format!("{} (L{})", node.qualified_path, line + 1)
    } else {
        node.qualified_path.clone()
    }
}

fn edge_label_for_kind(kind: Option<&EdgeKind>, _outgoing: bool) -> String {
    match kind {
        Some(EdgeKind::DirectCall) => "",
        Some(EdgeKind::StaticTraitCall) => "impl",
        Some(EdgeKind::TraitDefCall) => "trait",
        Some(EdgeKind::DynamicDispatch) => "dyn",
        None => "",
    }
    .to_string()
}
