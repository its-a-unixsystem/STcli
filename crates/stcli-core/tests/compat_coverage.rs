//! Compatibility coverage ratchet (workstream E, issue #45).
//!
//! Loads the compatibility profile and every fixture suite, collects all
//! case ids, and verifies that every exact macro, hard-unsupported macro,
//! preset field, hard-unsupported scope entry, and lore boundary has at
//! least one fixture case — unless it is explicitly listed in the
//! checked-in allowlist at `compat/coverage-allowlist.json`.
//!
//! The allowlist may only shrink: adding a new uncovered entry without
//! listing it fails (new gap), and leaving a now-covered entry in the
//! allowlist also fails (stale entry). This turns the corpus into a
//! CI-enforced ratchet that drives incremental completeness.

use std::collections::BTreeSet;

use serde::Deserialize;
use stcli_core::{CompatibilityProfile, collect_case_ids};

/// Allowlist JSON structure mirroring the profile's coverage surface.
#[derive(Debug, Deserialize)]
struct CoverageAllowlist {
    macros: AllowlistMacros,
    preset_fields: Vec<String>,
    scope: AllowlistScope,
    lore: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AllowlistMacros {
    exact: Vec<String>,
    hard_unsupported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AllowlistScope {
    hard_unsupported: Vec<String>,
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Extracts the macro name from a case id like `macro.<name>.<variant>`.
/// Returns `None` for ids that don't start with `macro.` or have no
/// middle segment.
fn macro_name_from_id(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("macro.")?;
    let end = rest.find('.')?;
    Some(&rest[..end])
}

/// Extracts the preset field name from `preset.<field>.<variant>`.
fn preset_name_from_id(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("preset.")?;
    let end = rest.find('.')?;
    Some(&rest[..end])
}

/// Extracts the scope entry name from `scope.<entry>.<variant>`.
fn scope_name_from_id(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("scope.")?;
    let end = rest.find('.')?;
    Some(&rest[..end])
}

/// Extracts the lore boundary name from `lore.<name>` or `lore.<name>.<variant>`.
fn lore_name_from_id(id: &str) -> Option<&str> {
    let rest = id.strip_prefix("lore.")?;
    match rest.find('.') {
        Some(end) => Some(&rest[..end]),
        None => Some(rest),
    }
}

#[test]
fn coverage_ratchet_reports_every_uncovered_entry() {
    let root = workspace_root();
    let profile_path = root.join("compat/profiles/sillytavern-1.18-core.json");
    let fixtures_path = root.join("compat/fixtures");
    let allowlist_path = root.join("compat/coverage-allowlist.json");

    let profile = CompatibilityProfile::load(&profile_path).unwrap();
    let case_ids = collect_case_ids(&fixtures_path).unwrap();
    let allowlist_source = std::fs::read_to_string(&allowlist_path).unwrap();
    let allowlist: CoverageAllowlist = serde_json::from_str(&allowlist_source).unwrap();

    // --- Collect covered names from case ids ---

    let covered_macros: BTreeSet<String> = case_ids
        .iter()
        .filter_map(|id| macro_name_from_id(id).map(|s| s.to_owned()))
        .collect();

    let covered_presets: BTreeSet<String> = case_ids
        .iter()
        .filter_map(|id| preset_name_from_id(id).map(|s| s.to_owned()))
        .collect();

    let covered_scope: BTreeSet<String> = case_ids
        .iter()
        .filter_map(|id| scope_name_from_id(id).map(|s| s.to_owned()))
        .collect();

    let covered_lore: BTreeSet<String> = case_ids
        .iter()
        .filter_map(|id| lore_name_from_id(id).map(|s| s.to_owned()))
        .collect();

    let allowlist_macros_exact: BTreeSet<String> = allowlist.macros.exact.iter().cloned().collect();
    let allowlist_macros_hard: BTreeSet<String> =
        allowlist.macros.hard_unsupported.iter().cloned().collect();
    let allowlist_presets: BTreeSet<String> = allowlist.preset_fields.iter().cloned().collect();
    let allowlist_scope: BTreeSet<String> =
        allowlist.scope.hard_unsupported.iter().cloned().collect();
    let allowlist_lore: BTreeSet<String> = allowlist.lore.iter().cloned().collect();

    let mut failures: Vec<String> = Vec::new();
    if !allowlist_macros_exact.is_empty()
        || !allowlist_macros_hard.is_empty()
        || !allowlist_presets.is_empty()
        || !allowlist_scope.is_empty()
        || !allowlist_lore.is_empty()
    {
        failures.push(
            "the compatibility coverage allowlist reached zero and cannot grow again".to_owned(),
        );
    }

    // --- Exact macros: every name must be covered or allowlisted ---

    for name in &profile.macros.exact {
        let covered = covered_macros.contains(name);
        let allowed = allowlist_macros_exact.contains(name);
        if !covered && !allowed {
            failures.push(format!(
                "exact macro '{name}' has zero fixture cases and is not in the allowlist"
            ));
        }
        if covered && allowed {
            failures.push(format!(
                "exact macro '{name}' is covered but still in the allowlist (stale entry — remove it)"
            ));
        }
    }

    // --- Hard-unsupported macros: every name must be covered or allowlisted ---

    for name in &profile.macros.hard_unsupported {
        let covered = covered_macros.contains(name);
        let allowed = allowlist_macros_hard.contains(name);
        if !covered && !allowed {
            failures.push(format!(
                "hard-unsupported macro '{name}' has zero fixture cases and is not in the allowlist"
            ));
        }
        if covered && allowed {
            failures.push(format!(
                "hard-unsupported macro '{name}' is covered but still in the allowlist (stale entry — remove it)"
            ));
        }
    }

    // --- Preset fields: every field must be covered or allowlisted ---

    for name in profile.preset_fields.keys() {
        let covered = covered_presets.contains(name);
        let allowed = allowlist_presets.contains(name);
        if !covered && !allowed {
            failures.push(format!(
                "preset field '{name}' has zero fixture cases and is not in the allowlist"
            ));
        }
        if covered && allowed {
            failures.push(format!(
                "preset field '{name}' is covered but still in the allowlist (stale entry — remove it)"
            ));
        }
    }

    // --- Scope hard-unsupported: every entry must be covered or allowlisted ---

    for name in &profile.scope.hard_unsupported {
        let covered = covered_scope.contains(name);
        let allowed = allowlist_scope.contains(name);
        if !covered && !allowed {
            failures.push(format!(
                "scope hard-unsupported '{name}' has zero fixture cases and is not in the allowlist"
            ));
        }
        if covered && allowed {
            failures.push(format!(
                "scope hard-unsupported '{name}' is covered but still in the allowlist (stale entry — remove it)"
            ));
        }
    }

    // --- Lore boundaries: every boundary must be covered or allowlisted ---

    let lore_boundaries = [
        "recursion-at-limit",
        "recursion-over-limit",
        "budget-exact-fit",
        "budget-one-over",
        "key-case-sensitivity",
        "key-whole-word",
        "key-regex",
        "selective-secondary-key",
        "ordering-ties",
        "position-classes",
        "scan-depth-edges",
        "disabled-entries",
        "probability-zero",
        "probability-hundred",
    ];

    for name in &lore_boundaries {
        let covered = covered_lore.contains(*name);
        let allowed = allowlist_lore.contains(*name);
        if !covered && !allowed {
            failures.push(format!(
                "lore boundary '{name}' has zero fixture cases and is not in the allowlist"
            ));
        }
        if covered && allowed {
            failures.push(format!(
                "lore boundary '{name}' is covered but still in the allowlist (stale entry — remove it)"
            ));
        }
    }

    // --- Allowlist entries that don't exist in the profile (phantom entries) ---

    for name in &allowlist_macros_exact {
        if !profile.macros.exact.contains(name) {
            failures.push(format!(
                "allowlist exact macro '{name}' does not exist in the profile (phantom entry)"
            ));
        }
    }
    for name in &allowlist_macros_hard {
        if !profile.macros.hard_unsupported.contains(name) {
            failures.push(format!(
                "allowlist hard-unsupported macro '{name}' does not exist in the profile (phantom entry)"
            ));
        }
    }
    for name in &allowlist_presets {
        if !profile.preset_fields.contains_key(name) {
            failures.push(format!(
                "allowlist preset field '{name}' does not exist in the profile (phantom entry)"
            ));
        }
    }
    for name in &allowlist_scope {
        if !profile.scope.hard_unsupported.contains(name) {
            failures.push(format!(
                "allowlist scope entry '{name}' does not exist in the profile (phantom entry)"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "compatibility coverage ratchet failures:\n  - {}\n\n\
         To fix: add fixture cases for uncovered entries, or add them to \
         compat/coverage-allowlist.json if they are known-missing. \
         Remove stale allowlist entries that are now covered.",
        failures.join("\n  - ")
    );
}
