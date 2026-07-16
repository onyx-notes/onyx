//! In-memory link graph: dense adjacency over interned note ids.
//!
//! Graph view, backlink panes, and analytics all read from here — never
//! from SQLite on a hot path. ~2 MB for a 100k-note / 1M-link vault.
//! Built from the index in one pass; rebuilt on `BulkChange` (cheap), and
//! incrementally patched by the app layer later.

use std::collections::HashMap;

use crate::index::{Index, IndexError};
use crate::paths::NoteId;

/// Immutable snapshot of the vault's link structure.
pub struct LinkGraph {
    ids: Vec<NoteId>,
    index_of: HashMap<NoteId, u32>,
    outgoing: Vec<Vec<u32>>,
    incoming: Vec<Vec<u32>>,
}

impl LinkGraph {
    /// Build from the metadata index. Resolution follows the same rules as
    /// `Index::resolve` (exact path first, then shortest-path basename).
    pub fn build(index: &Index) -> Result<Self, IndexError> {
        let (nodes, raw_edges) = index.graph_data()?;

        let ids: Vec<NoteId> = nodes.iter().map(|node| node.id).collect();
        let index_of: HashMap<NoteId, u32> = ids
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position as u32))
            .collect();

        // Resolution maps, one pass: exact path, and best (shortest, then
        // lexicographically first) note per basename.
        let mut by_path: HashMap<&str, u32> = HashMap::with_capacity(nodes.len());
        let mut by_name: HashMap<&str, (usize, &str, u32)> = HashMap::new();
        for node in &nodes {
            let position = index_of[&node.id];
            by_path.insert(node.lookup_path.as_str(), position);
            let candidate = (node.path_key.len(), node.path_key.as_str(), position);
            by_name
                .entry(node.lookup_name.as_str())
                .and_modify(|best| {
                    if (candidate.0, candidate.1) < (best.0, best.1) {
                        *best = candidate;
                    }
                })
                .or_insert(candidate);
        }

        let mut outgoing = vec![Vec::new(); ids.len()];
        let mut incoming = vec![Vec::new(); ids.len()];
        for (source_id, target_key) in &raw_edges {
            let Some(&source) = index_of.get(source_id) else {
                continue;
            };
            let target = by_path
                .get(target_key.as_str())
                .copied()
                .or_else(|| by_name.get(target_key.as_str()).map(|best| best.2));
            let Some(target) = target else {
                continue; // unresolved link — not an edge
            };
            if target == source {
                continue; // self-links don't shape the graph
            }
            outgoing[source as usize].push(target);
            incoming[target as usize].push(source);
        }
        for adjacency in outgoing.iter_mut().chain(incoming.iter_mut()) {
            adjacency.sort_unstable();
            adjacency.dedup();
        }

        Ok(Self {
            ids,
            index_of,
            outgoing,
            incoming,
        })
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn edge_count(&self) -> usize {
        self.outgoing.iter().map(Vec::len).sum()
    }

    /// Notes this note links to.
    pub fn outgoing(&self, id: NoteId) -> Vec<NoteId> {
        self.neighbors(id, &self.outgoing)
    }

    /// Notes linking to this note.
    pub fn incoming(&self, id: NoteId) -> Vec<NoteId> {
        self.neighbors(id, &self.incoming)
    }

    /// Notes with no links in either direction.
    pub fn orphans(&self) -> Vec<NoteId> {
        (0..self.ids.len())
            .filter(|&position| {
                self.outgoing[position].is_empty() && self.incoming[position].is_empty()
            })
            .map(|position| self.ids[position])
            .collect()
    }

    /// Top `limit` notes by incoming-link count (the "hub notes" seed for
    /// analytics), descending.
    pub fn most_linked(&self, limit: usize) -> Vec<(NoteId, usize)> {
        let mut ranked: Vec<(NoteId, usize)> = (0..self.ids.len())
            .map(|position| (self.ids[position], self.incoming[position].len()))
            .filter(|(_, degree)| *degree > 0)
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        ranked.truncate(limit);
        ranked
    }

    fn neighbors(&self, id: NoteId, adjacency: &[Vec<u32>]) -> Vec<NoteId> {
        self.index_of
            .get(&id)
            .map(|&position| {
                adjacency[position as usize]
                    .iter()
                    .map(|&neighbor| self.ids[neighbor as usize])
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::fs::MemFs;
    use crate::paths::NotePath;
    use crate::vault::{Vault, VaultConfig};

    fn setup(files: &[(&str, &str)]) -> (Vault, Index) {
        let vault = Vault::new(Arc::new(MemFs::new()), VaultConfig::default());
        for (path, content) in files {
            vault
                .write(&NotePath::new(path).unwrap(), content.as_bytes())
                .unwrap();
        }
        let mut index = Index::open_in_memory([0; 16]).unwrap();
        index.rebuild(&vault).unwrap();
        (vault, index)
    }

    fn id(vault: &Vault, path: &str) -> NoteId {
        vault.note_id(&NotePath::new(path).unwrap())
    }

    #[test]
    fn edges_follow_resolution() {
        let (vault, index) = setup(&[
            ("a.md", "[[b]] [[folder/c]] [[Ghost]]"),
            ("b.md", "[[a]]"),
            ("folder/c.md", "no links"),
            ("orphan.md", "nothing"),
        ]);
        let graph = LinkGraph::build(&index).unwrap();

        assert_eq!(graph.len(), 4);
        assert_eq!(graph.edge_count(), 3); // Ghost is unresolved

        let a = id(&vault, "a.md");
        let b = id(&vault, "b.md");
        let c = id(&vault, "folder/c.md");

        let mut outgoing_a = graph.outgoing(a);
        outgoing_a.sort();
        let mut expected = vec![b, c];
        expected.sort();
        assert_eq!(outgoing_a, expected);
        assert_eq!(graph.incoming(a), vec![b]);
        assert_eq!(graph.incoming(c), vec![a]);
        assert_eq!(graph.orphans(), vec![id(&vault, "orphan.md")]);
    }

    #[test]
    fn duplicate_links_dedupe_and_self_links_drop() {
        let (vault, index) = setup(&[("a.md", "[[b]] [[b]] [[a]]"), ("b.md", "x")]);
        let graph = LinkGraph::build(&index).unwrap();
        assert_eq!(graph.edge_count(), 1);
        assert_eq!(graph.outgoing(id(&vault, "a.md")).len(), 1);
    }

    #[test]
    fn most_linked_ranks_by_in_degree() {
        let (vault, index) = setup(&[
            ("hub.md", "the hub"),
            ("x.md", "[[hub]]"),
            ("y.md", "[[hub]] [[x]]"),
            ("z.md", "[[hub]]"),
        ]);
        let graph = LinkGraph::build(&index).unwrap();
        let ranked = graph.most_linked(10);
        assert_eq!(ranked[0], (id(&vault, "hub.md"), 3));
        assert_eq!(ranked[1], (id(&vault, "x.md"), 1));
    }

    #[test]
    fn unknown_note_has_no_neighbors() {
        let (_, index) = setup(&[("a.md", "x")]);
        let graph = LinkGraph::build(&index).unwrap();
        assert!(graph.outgoing(NoteId::from_bytes([9; 16])).is_empty());
    }

    #[test]
    fn corpus_graph_builds_and_resolves() {
        let fs = Arc::new(MemFs::new());
        use crate::fs::VaultFs;
        onyx_testkit::generate(onyx_testkit::CorpusConfig::SMALL, |relative, content| {
            fs.write_atomic(&NotePath::new(relative).unwrap(), content.as_bytes())
                .unwrap();
        });
        let vault = Vault::new(fs, VaultConfig::default());
        let mut index = Index::open_in_memory([0; 16]).unwrap();
        index.rebuild(&vault).unwrap();

        let graph = LinkGraph::build(&index).unwrap();
        assert_eq!(graph.len(), 100);
        assert!(graph.edge_count() > 50, "corpus is link-dense");
    }
}
