//! Fixture pack for ablation task family 2 (cross-file reference
//! resolution on a lexically collision-dense target). The vocabulary is
//! deliberately narrow: `lookup`, `resolve`, `entry`, and `index`
//! recur across types, methods, and free functions, so a text query
//! for any one of them returns many candidates.

pub struct EntryIndex {
    pub slots: Vec<(String, u32)>,
}

#[derive(Clone)]
pub struct IndexEntry {
    pub name: String,
    pub weight: u32,
}

pub struct ResolveTable {
    pub rows: Vec<IndexEntry>,
}

impl EntryIndex {
    pub fn lookup(&self, key: &str) -> Option<u32> {
        self.slots
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| *v)
    }

    pub fn resolve(&self, key: &str) -> u32 {
        self.lookup(key).unwrap_or(0)
    }
}

impl IndexEntry {
    pub fn lookup(&self) -> u32 {
        self.weight
    }

    pub fn resolve(&self) -> String {
        self.name.clone()
    }
}

impl ResolveTable {
    pub fn lookup(&self, key: &str) -> Option<IndexEntry> {
        self.rows.iter().find(|e| e.name == key).cloned()
    }

    pub fn resolve_entry(&self, key: &str) -> u32 {
        self.lookup(key).map(|e| e.lookup()).unwrap_or(0)
    }
}

/// Resolve `key` against `table`, falling back to `index`.
pub fn resolve(table: &ResolveTable, index: &EntryIndex, key: &str) -> u32 {
    let via_index = index.resolve(key);
    let via_table = table.resolve_entry(key);
    if via_index > via_table {
        via_index
    } else {
        via_table
    }
}

pub fn entry_resolve(table: &ResolveTable, key: &str) -> String {
    match table.lookup(key) {
        Some(e) => e.resolve(),
        None => String::new(),
    }
}

pub fn index_of(index: &EntryIndex, key: &str) -> usize {
    index
        .slots
        .iter()
        .position(|(k, _)| k == key)
        .unwrap_or(usize::MAX)
}

pub fn build(seed: u32) -> (ResolveTable, EntryIndex) {
    let table = ResolveTable {
        rows: vec![
            IndexEntry { name: String::from("alpha"), weight: seed },
            IndexEntry { name: String::from("beta"), weight: seed + 1 },
        ],
    };
    let index = EntryIndex {
        slots: vec![
            (String::from("alpha"), seed * 2),
            (String::from("beta"), seed * 2 + 1),
        ],
    };
    (table, index)
}

pub fn aggregate(seed: u32) -> u32 {
    let (table, index) = build(seed);
    let mut total = 0;
    for key in ["alpha", "beta"] {
        total += resolve(&table, &index, key);
        total += index.lookup(key).unwrap_or(0);
        total += table.lookup(key).map(|e| e.lookup()).unwrap_or(0);
    }
    total += entry_resolve(&table, "alpha").len() as u32;
    total += index_of(&index, "beta") as u32;
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_is_stable_for_small_seed() {
        assert_eq!(aggregate(2), 29);
    }

    #[test]
    fn entry_resolve_finds_known_key() {
        let (table, _) = build(1);
        assert_eq!(entry_resolve(&table, "alpha"), "alpha");
    }
}
