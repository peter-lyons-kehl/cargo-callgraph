// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::Result;
use petgraph::visit::EdgeRef;
use serde::Serialize;

use crate::graph::CallGraph;

#[derive(Serialize)]
struct JsonOutput {
    nodes: Vec<JsonNode>,
    edges: Vec<JsonEdge>,
}

#[derive(Serialize)]
struct JsonNode {
    id: String,
    name: String,
    qualified_path: String,
    crate_name: String,
    module: Vec<String>,
    file: Option<String>,
    line: Option<u32>,
    col: Option<u32>,
    is_workspace: bool,
    is_trait_method: bool,
}

#[derive(Serialize)]
struct JsonEdge {
    source: String,
    target: String,
    kind: String,
    call_site_file: Option<String>,
    call_site_line: Option<u32>,
}

fn build_json(graph: &CallGraph) -> JsonOutput {
    let mut nodes = Vec::new();
    let mut node_id_map: HashMap<_, String> = HashMap::new();

    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        let id = node.id.0.clone();
        node_id_map.insert(node_idx, id.clone());

        nodes.push(JsonNode {
            id,
            name: node.name.clone(),
            qualified_path: node.qualified_path.clone(),
            crate_name: node.crate_name.clone(),
            module: node.module_path.clone(),
            file: node.file_path.clone(),
            line: node.line,
            col: node.col,
            is_workspace: node.is_workspace,
            is_trait_method: node.is_trait_method,
        });
    }

    let mut edges = Vec::new();
    for edge_ref in graph.graph.edge_references() {
        let source = node_id_map
            .get(&edge_ref.source())
            .cloned()
            .unwrap_or_default();
        let target = node_id_map
            .get(&edge_ref.target())
            .cloned()
            .unwrap_or_default();
        let edge = edge_ref.weight();

        edges.push(JsonEdge {
            source,
            target,
            kind: format!("{:?}", edge.kind),
            call_site_file: edge.call_site_file.clone(),
            call_site_line: edge.call_site_line,
        });
    }

    JsonOutput { nodes, edges }
}

pub fn write(graph: &CallGraph, path: &Path) -> Result<()> {
    let output = build_json(graph);
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &output)?;
    Ok(())
}

pub fn write_to_string(graph: &CallGraph) -> Result<String> {
    let output = build_json(graph);
    Ok(serde_json::to_string_pretty(&output)?)
}
