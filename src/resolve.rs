// SPDX-License-Identifier: AGPL-3.0
//
use ra_ap_ide::{FilePosition, NavigationTarget};

/// Uses rust-analyzer's goto_implementation to find concrete impls of a trait method.
pub fn resolve_trait_impls(
    analysis: &ra_ap_ide::Analysis,
    config: &ra_ap_ide::GotoImplementationConfig,
    nav: &NavigationTarget,
) -> Vec<NavigationTarget> {
    let position = FilePosition {
        file_id: nav.file_id,
        offset: nav.focus_or_full_range().start(),
    };
    match analysis.goto_implementation(config, position) {
        Ok(Some(range_info)) => range_info.info,
        _ => Vec::new(),
    }
}
