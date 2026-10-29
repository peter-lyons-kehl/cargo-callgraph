// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::HashMap;

use petgraph::graph::{Graph, NodeIndex};
use serde::{Deserialize, Serialize};

/// Stable identifier for a node, used for deduplication across traversal.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn new(
        _crate_name: &str,
        qualified_path: &str,
        file_path: Option<&str>,
        line: Option<u32>,
    ) -> Self {
        let loc = match (file_path, line) {
            (Some(f), Some(l)) => format!("@{}:{}", f, l),
            (Some(f), None) => format!("@{}", f),
            _ => String::new(),
        };
        NodeId(format!("{}{}", qualified_path, loc))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    pub qualified_path: String,
    pub crate_name: String,
    #[serde(rename = "module")]
    pub module_path: Vec<String>,
    #[serde(rename = "file")]
    pub file_path: Option<String>,
    pub line: Option<u32>,
    pub col: Option<u32>,
    pub is_workspace: bool,
    pub is_trait_method: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeKind {
    DirectCall,
    StaticTraitCall,
    TraitDefCall,
    DynamicDispatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Edge {
    pub kind: EdgeKind,
    pub call_site_file: Option<String>,
    pub call_site_line: Option<u32>,
}

pub struct CallGraph {
    pub graph: Graph<Node, Edge>,
    pub node_index_by_id: HashMap<NodeId, NodeIndex>,
}

impl CallGraph {
    pub fn new() -> Self {
        Self {
            graph: Graph::new(),
            node_index_by_id: HashMap::new(),
        }
    }

    /// Get or insert a node. Returns the NodeIndex.
    pub fn get_or_insert_node(&mut self, node: Node) -> NodeIndex {
        if let Some(&idx) = self.node_index_by_id.get(&node.id) {
            idx
        } else {
            let id = node.id.clone();
            let idx = self.graph.add_node(node);
            self.node_index_by_id.insert(id, idx);
            idx
        }
    }

    pub fn add_edge(&mut self, from: NodeIndex, to: NodeIndex, edge: Edge) {
        self.graph.add_edge(from, to, edge);
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

impl Default for CallGraph {
    fn default() -> Self {
        Self::new()
    }
}
