//! Plugin picker grouping, search scoring and chain-order helpers.

use std::collections::BTreeMap;

use michost_builtin::BuiltinKind;
use michost_graph::{ChainEntry, PluginSpec};
use michost_host_abi::PluginDescriptor;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PickerGrouping {
    #[default]
    Vendor,
    Category,
}

pub(crate) struct PickerCache {
    pub(crate) generation: u64,
    pub(crate) search: String,
    pub(crate) grouping: PickerGrouping,
    pub(crate) groups: BTreeMap<String, Vec<(usize, u32)>>,
    pub(crate) shown: usize,
}

pub(crate) fn builtin_spec(kind: BuiltinKind) -> PluginSpec {
    PluginSpec {
        path: kind.path().to_string(),
        class_id: kind.path().to_string(),
        name: kind.name().to_string(),
        vendor: kind.vendor().to_string(),
        main_inputs: 2,
        main_outputs: 2,
        sidechain_inputs: 0,
    }
}

pub(crate) fn builtin_descriptor(kind: BuiltinKind) -> PluginDescriptor {
    PluginDescriptor {
        path: kind.path().to_string(),
        class_id: kind.path().to_string(),
        name: kind.name().to_string(),
        vendor: kind.vendor().to_string(),
        category: format!("Built-in / {}", kind.category()),
        main_inputs: 2,
        main_outputs: 2,
        sidechain_inputs: 0,
        flags: 0,
    }
}

pub(crate) fn build_picker_cache(
    plugins: &[PluginDescriptor],
    generation: u64,
    search: String,
    grouping: PickerGrouping,
) -> PickerCache {
    let mut groups: BTreeMap<String, Vec<(usize, u32)>> = BTreeMap::new();
    for (index, descriptor) in plugins.iter().enumerate() {
        let score = if search.is_empty() {
            0
        } else {
            match descriptor_score(&search, descriptor) {
                Some(score) => score,
                None => continue,
            }
        };
        let key = match grouping {
            PickerGrouping::Vendor => {
                if descriptor.vendor.is_empty() {
                    "Unknown vendor".to_string()
                } else {
                    descriptor.vendor.clone()
                }
            }
            PickerGrouping::Category => {
                let category = display_category(&descriptor.category);
                if category.is_empty() {
                    "Uncategorised".to_string()
                } else {
                    category
                }
            }
        };
        groups.entry(key).or_default().push((index, score));
    }
    for entries in groups.values_mut() {
        if !search.is_empty() {
            entries.sort_by(|left, right| {
                right
                    .1
                    .cmp(&left.1)
                    .then_with(|| plugins[left.0].name.cmp(&plugins[right.0].name))
            });
        } else if grouping == PickerGrouping::Category {
            entries.sort_by(|left, right| {
                plugins[left.0]
                    .name
                    .to_lowercase()
                    .cmp(&plugins[right.0].name.to_lowercase())
            });
        }
    }
    let shown = groups.values().map(Vec::len).sum();
    PickerCache {
        generation,
        search,
        grouping,
        groups,
        shown,
    }
}

pub(crate) fn display_category(category: &str) -> String {
    category
        .split('|')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" / ")
}

pub(crate) fn normalize_search(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(|character| character.to_lowercase())
        .collect()
}

pub(crate) fn fuzzy_score(query: &str, text: &str) -> Option<u32> {
    let query = normalize_search(query);
    if query.is_empty() {
        return Some(0);
    }
    let target = normalize_search(text);
    if target.is_empty() {
        return None;
    }
    if let Some(position) = target.find(&query) {
        let prefix_bonus = if position == 0 { 200 } else { 0 };
        return Some(1200 + prefix_bonus - position.min(500) as u32);
    }
    let query_chars: Vec<char> = query.chars().collect();
    let mut matched = 0usize;
    let mut gaps = 0u32;
    let mut previous = None;
    for (position, character) in target.chars().enumerate() {
        if matched < query_chars.len() && character == query_chars[matched] {
            if let Some(previous_position) = previous {
                gaps = gaps.saturating_add((position - previous_position - 1) as u32);
            }
            previous = Some(position);
            matched += 1;
        }
    }
    if matched != query_chars.len() {
        return None;
    }
    Some(600u32.saturating_sub(gaps.min(599)))
}

pub(crate) fn descriptor_score(query: &str, descriptor: &PluginDescriptor) -> Option<u32> {
    let name = fuzzy_score(query, &descriptor.name).map(|score| score + 50);
    let vendor = fuzzy_score(query, &descriptor.vendor);
    let category = fuzzy_score(query, &descriptor.category);
    [name, vendor, category].into_iter().flatten().max()
}

pub(crate) fn move_chain_entry(chain: &mut Vec<ChainEntry>, from: usize, to: usize) -> bool {
    if from == to || from >= chain.len() || to >= chain.len() {
        return false;
    }
    let entry = chain.remove(from);
    chain.insert(to, entry);
    true
}
