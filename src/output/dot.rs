// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use anyhow::Result;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;

use crate::graph::{CallGraph, EdgeKind};

pub fn write(graph: &CallGraph, path: &Path) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);

    writeln!(file, "digraph callgraph {{")?;
    writeln!(file, "    rankdir=TB;")?;
    writeln!(file, "    node [shape=box, fontname=\"Helvetica\"];")?;

    // Group nodes by crate
    let mut crates: BTreeMap<String, Vec<NodeIndex>> = BTreeMap::new();
    for node_idx in graph.graph.node_indices() {
        let node = &graph.graph[node_idx];
        crates
            .entry(node.crate_name.clone())
            .or_default()
            .push(node_idx);
    }

    for (crate_name, nodes) in crates {
        let cluster_id = sanitize_id(&crate_name);
        writeln!(
            file,
            "    subgraph cluster_{} {{",
            cluster_id
        )?;
        writeln!(file, "        label=\"{}\";", escape_label(&crate_name))?;
        writeln!(file, "        style=rounded;")?;
        writeln!(file, "        color=gray;")?;

        for idx in nodes {
            let node = &graph.graph[idx];
            let node_id = format!("node_{}", idx.index());
            // Disambiguate bare names (no container prefix) with line numbers.
            let label = if node.qualified_path.contains("::") {
                escape_label(&node.name)
            } else if let Some(line) = node.line {
                escape_label(&format!("{} (L{})", node.name, line + 1))
            } else {
                escape_label(&node.name)
            };
            let tooltip = escape_label(&node.qualified_path);
            let color = if node.is_workspace {
                ""
            } else {
                ", color=lightblue, style=filled"
            };
            writeln!(
                file,
                "        {} [label=\"{}\", tooltip=\"{}\"{}];",
                node_id, label, tooltip, color
            )?;
        }

        writeln!(file, "    }}")?;
    }

    // Edges
    for edge_ref in graph.graph.edge_references() {
        let source = format!("node_{}", edge_ref.source().index());
        let target = format!("node_{}", edge_ref.target().index());
        let edge = edge_ref.weight();

        let (style, color) = match edge.kind {
            EdgeKind::DirectCall => ("", "black"),
            EdgeKind::TraitDefCall => ("style=dashed", "blue"),
            EdgeKind::StaticTraitCall => ("style=dashed", "green"),
            EdgeKind::DynamicDispatch => ("style=dotted", "red"),
        };

        if style.is_empty() {
            writeln!(file, "    {} -> {} [color={}];", source, target, color)?;
        } else {
            writeln!(
                file,
                "    {} -> {} [{}, color={}];",
                source, target, style, color
            )?;
        }
    }

    writeln!(file, "}}")?;

    Ok(())
}

fn sanitize_id(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
        .collect()
}

fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
