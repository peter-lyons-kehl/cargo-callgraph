// SPDX-License-Identifier: AGPL-3.0
//
use std::collections::HashSet;
use std::path::Path;

use anyhow::Context;
use ra_ap_base_db::{Crate, CrateOrigin};
use ra_ap_ide::{Analysis, AnalysisHost};
use ra_ap_ide_db::RootDatabase;
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, RustLibSource};
use ra_ap_vfs::Vfs;

pub struct LoadedProject {
    pub analysis: Analysis,
    pub host: AnalysisHost,
    pub db: RootDatabase,
    pub vfs: Vfs,
    pub workspace_crates: HashSet<Crate>,
}

pub fn load_project(manifest_path: &Path) -> anyhow::Result<LoadedProject> {
    let manifest_path = manifest_path
        .canonicalize()
        .with_context(|| format!("canonicalizing manifest path: {}", manifest_path.display()))?;

    let cargo_config = CargoConfig {
        all_targets: true,
        sysroot: Some(RustLibSource::Discover),
        sysroot_src: None,
        rustc_source: None,
        ..Default::default()
    };

    let load_config = LoadCargoConfig {
        load_out_dirs_from_check: true,
        with_proc_macro_server: ProcMacroServerChoice::Sysroot,
        prefill_caches: true,
        num_worker_threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2),
        proc_macro_processes: 1,
    };

    let progress = |msg: String| {
        if std::env::var("CALLGRAPH_VERBOSE").is_ok() {
            eprintln!("[ra] {}", msg);
        }
    };

    let (db, vfs, _proc_macro) =
        load_workspace_at(&manifest_path, &cargo_config, &load_config, &progress)
            .context("loading workspace with rust-analyzer")?;

    let host = AnalysisHost::with_database(db.clone());
    let analysis = host.analysis();

    // Identify workspace-member crates by checking CrateOrigin::Local
    let workspace_crates: HashSet<Crate> = ra_ap_base_db::all_crates(&db)
        .iter()
        .copied()
        .filter(|&krate| matches!(krate.data(&db).origin, CrateOrigin::Local { .. }))
        .collect();

    Ok(LoadedProject {
        analysis,
        host,
        db,
        vfs,
        workspace_crates,
    })
}
