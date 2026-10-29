// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::{HashSet, VecDeque};

use ra_ap_base_db::Crate;
use ra_ap_hir::{AsAssocItem, ModuleDef, attach_db};
use ra_ap_ide::{CallHierarchyConfig, FilePosition, NavigationTarget, TryToNav};
use ra_ap_ide_db::{RootDatabase, ra_fixture::RaFixtureConfig};

use crate::graph::{CallGraph, Edge, EdgeKind, Node, NodeId};
use crate::project::LoadedProject;
use crate::resolve::resolve_trait_impls;

pub fn build(project: &LoadedProject, graph: &mut CallGraph) -> anyhow::Result<()> {
    attach_db(&project.db, || build_inner(project, graph))
}

fn build_inner(project: &LoadedProject, graph: &mut CallGraph) -> anyhow::Result<()> {
    let db = &project.db;
    let analysis = &project.analysis;

    let mut work_queue: VecDeque<(petgraph::graph::NodeIndex, NavigationTarget)> = VecDeque::new();
    let mut visited: HashSet<NodeId> = HashSet::new();

    // Seed the queue with all functions in workspace crates.
    for krate in ra_ap_hir::Crate::all(db) {
        if !project.workspace_crates.contains(&krate.base()) {
            continue;
        }
        let modules = krate.modules(db);
        for module in modules {
            let decls = module.declarations(db);
            for decl in decls {
                if let ModuleDef::Function(func) = decl {
                    if let Some(nav) = nav_target_for_function(db, func) {
                        let module_path = module_path_for_function(db, func);
                        let is_trait = func
                            .as_assoc_item(db)
                            .and_then(|a| a.container_trait(db))
                            .is_some();
                        let mut node =
                            nav_target_to_node(db, analysis, &project.vfs, &nav, true);
                        node.module_path = module_path;
                        node.is_trait_method = is_trait;
                        let id = node.id.clone();
                        let idx = graph.get_or_insert_node(node);
                        if visited.insert(id) {
                            work_queue.push_back((idx, nav));
                        }
                    }
                }
            }
        }
    }

    eprintln!(
        "Discovered {} workspace functions to analyze",
        work_queue.len()
    );

    let config = CallHierarchyConfig {
        exclude_tests: false,
        ra_fixture: RaFixtureConfig::default(),
    };

    let goto_impl_config = ra_ap_ide::GotoImplementationConfig {
        filter_adjacent_derive_implementations: true,
    };

    while let Some((caller_idx, caller_nav)) = work_queue.pop_front() {
        let position = FilePosition {
            file_id: caller_nav.file_id,
            offset: caller_nav.focus_or_full_range().start(),
        };

        let outgoing = match analysis.outgoing_calls(&config, position) {
            Ok(Some(calls)) => calls,
            Ok(None) => continue,
            Err(_) => continue, // Canceled
        };

        for call_item in outgoing {
            let callee_nav = call_item.target;
            let is_workspace = is_workspace_target(analysis, &callee_nav, &project.workspace_crates);

            // Trait resolution: check if callee is a trait method and find concrete impls.
            // Only resolve for workspace callees to avoid exploding the graph with std trait impls.
            let impls = if is_workspace {
                resolve_trait_impls(analysis, &goto_impl_config, &callee_nav)
            } else {
                Vec::new()
            };

            // Determine whether callee_nav is a trait def or an impl method.
            // goto_implementation on a trait def returns all impls (excluding the def itself).
            // goto_implementation on an impl method returns all impls (including itself).
            let callee_is_impl = impls.iter().any(|i| {
                i.file_id == callee_nav.file_id
                    && i.focus_or_full_range() == callee_nav.focus_or_full_range()
            });
            let is_trait_def = !impls.is_empty() && !callee_is_impl;

            let mut callee_node =
                nav_target_to_node(db, analysis, &project.vfs, &callee_nav, is_workspace);
            callee_node.is_trait_method = is_trait_def;
            let callee_id = callee_node.id.clone();
            let callee_idx = graph.get_or_insert_node(callee_node);

            // Add call site info from the first range.
            let (call_site_file, call_site_line) = call_item
                .ranges
                .first()
                .and_then(|fr| {
                    let file_id = fr.file_id;
                    let line = analysis
                        .file_line_index(file_id)
                        .ok()
                        .map(|idx| idx.line_col(fr.range.start()).line);
                    let path = project.vfs.file_path(file_id).to_string();
                    Some((Some(path), line))
                })
                .unwrap_or((None, None));

            if is_trait_def {
                // Edge to the trait definition.
                graph.add_edge(
                    caller_idx,
                    callee_idx,
                    Edge {
                        kind: EdgeKind::TraitDefCall,
                        call_site_file: call_site_file.clone(),
                        call_site_line,
                    },
                );

                // Additional edges to each concrete implementation.
                for impl_nav in &impls {
                    let is_impl_workspace =
                        is_workspace_target(analysis, impl_nav, &project.workspace_crates);
                    let impl_node =
                        nav_target_to_node(db, analysis, &project.vfs, impl_nav, is_impl_workspace);
                    let impl_id = impl_node.id.clone();
                    let impl_idx = graph.get_or_insert_node(impl_node);
                    graph.add_edge(
                        caller_idx,
                        impl_idx,
                        Edge {
                            kind: EdgeKind::StaticTraitCall,
                            call_site_file: call_site_file.clone(),
                            call_site_line,
                        },
                    );
                    if is_impl_workspace && visited.insert(impl_id) {
                        work_queue.push_back((impl_idx, impl_nav.clone()));
                    }
                }
            } else {
                // Direct call (concrete function or impl method).
                graph.add_edge(
                    caller_idx,
                    callee_idx,
                    Edge {
                        kind: EdgeKind::DirectCall,
                        call_site_file: call_site_file.clone(),
                        call_site_line,
                    },
                );
            }

            // If callee is in workspace and we haven't traversed it yet, queue it.
            if is_workspace && visited.insert(callee_id) {
                work_queue.push_back((callee_idx, callee_nav));
            }
        }
    }

    Ok(())
}

fn nav_target_for_function(
    db: &RootDatabase,
    func: ra_ap_hir::Function,
) -> Option<NavigationTarget> {
    let sema = ra_ap_hir::Semantics::new(db);
    func.try_to_nav(&sema).map(|r| r.call_site)
}

fn module_path_for_function(
    db: &RootDatabase,
    func: ra_ap_hir::Function,
) -> Vec<String> {
    let module = func.module(db);
    let path = module.path_to_root(db);
    path.into_iter()
        .rev()
        .filter_map(|m| m.name(db).map(|n| n.as_str().to_string()))
        .collect()
}

fn nav_target_to_node(
    db: &RootDatabase,
    analysis: &ra_ap_ide::Analysis,
    vfs: &ra_ap_vfs::Vfs,
    nav: &NavigationTarget,
    is_workspace: bool,
) -> Node {
    let qualified_path = qualified_path_for_nav(nav);
    let file_path: Option<String> = Some(vfs.file_path(nav.file_id).to_string());
    let crate_name = crate_name_for_file(db, analysis, nav.file_id);

    let (line, col) = analysis
        .file_line_index(nav.file_id)
        .ok()
        .map(|idx| {
            let lc = idx.line_col(nav.focus_or_full_range().start());
            (Some(lc.line), Some(lc.col))
        })
        .unwrap_or((None, None));

    Node {
        id: NodeId::new(&crate_name, &qualified_path, file_path.as_deref(), line),
        name: nav.name.to_string(),
        qualified_path,
        crate_name,
        module_path: Vec::new(),
        file_path,
        line,
        col,
        is_workspace,
        is_trait_method: false,
    }
}

fn crate_name_for_file(
    db: &RootDatabase,
    analysis: &ra_ap_ide::Analysis,
    file_id: ra_ap_vfs::FileId,
) -> String {
    match analysis.crates_for(file_id) {
        Ok(crates) if !crates.is_empty() => {
            let krate = &crates[0];
            krate
                .extra_data(db)
                .display_name
                .as_ref()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        }
        _ => "unknown".to_string(),
    }
}

fn is_workspace_target(
    analysis: &ra_ap_ide::Analysis,
    nav: &NavigationTarget,
    workspace_crates: &HashSet<Crate>,
) -> bool {
    match analysis.crates_for(nav.file_id) {
        Ok(crates) if !crates.is_empty() => workspace_crates.contains(&crates[0]),
        _ => false,
    }
}

fn qualified_path_for_nav(nav: &NavigationTarget) -> String {
    match &nav.container_name {
        Some(container) => format!("{}::{}", container, nav.name),
        None => nav.name.to_string(),
    }
}
