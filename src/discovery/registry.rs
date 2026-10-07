//! Read-only registry access for the launchers that record installs there.

use std::collections::{BTreeMap, BTreeSet};
use winreg::{RegKey, enums::*};
pub(super) fn reg_values(hive: usize, path: &str) -> Vec<BTreeMap<String, String>> {
    let mut rows = vec![];
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        if let Ok(key) = RegKey::predef(hive as _).open_subkey_with_flags(path, KEY_READ | view) {
            let map = key
                .enum_values()
                .filter_map(|r| {
                    r.ok().and_then(|(name, _)| {
                        key.get_value::<String, _>(&name)
                            .ok()
                            .map(|value| (name, value))
                    })
                })
                .collect();
            if !rows.contains(&map) {
                rows.push(map);
            }
        }
    }
    rows
}
pub(super) fn reg_children(hive: usize, path: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let mut names = BTreeSet::new();
    for view in [KEY_WOW64_32KEY, KEY_WOW64_64KEY] {
        if let Ok(key) = RegKey::predef(hive as _).open_subkey_with_flags(path, KEY_READ | view) {
            names.extend(key.enum_keys().filter_map(Result::ok));
        }
    }
    names
        .into_iter()
        .flat_map(|name| {
            reg_values(hive, &format!("{path}\\{name}"))
                .into_iter()
                .map(move |v| (name.clone(), v))
        })
        .collect()
}
