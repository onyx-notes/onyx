//! Index behavior tests plus THE invariant: any sequence of incremental
//! updates leaves the index identical to a fresh rebuild.

use std::sync::Arc;

use onyx_core::{Index, MemFs, NotePath, Vault, VaultConfig, VaultEvent, VaultFs};
use proptest::prelude::*;

fn path(text: &str) -> NotePath {
    NotePath::new(text).unwrap()
}

fn vault() -> Vault {
    Vault::new(Arc::new(MemFs::new()), VaultConfig::default())
}

fn indexed(vault: &Vault) -> Index {
    let mut index = Index::open_in_memory([0; 16]).unwrap();
    index.rebuild(vault).unwrap();
    index
}

#[test]
fn indexes_links_tags_headings() {
    let vault = vault();
    vault
        .write(
            &path("a.md"),
            b"---\ntags: [fm-tag]\n---\n# Title\nSee [[b]] and [[folder/c#section|alias]] #inline-tag",
        )
        .unwrap();
    vault.write(&path("b.md"), b"back to [[a]]").unwrap();
    vault.write(&path("folder/c.md"), b"# c").unwrap();

    let index = indexed(&vault);
    assert_eq!(index.note_count().unwrap(), 3);

    let a_id = vault.note_id(&path("a.md"));
    let record = index.note(a_id).unwrap().unwrap();
    assert_eq!(record.title, "a");
    assert!(record.is_markdown);
    assert!(record.word_count.unwrap() > 0);

    // Resolution: basename and path, case-insensitive, with/without .md.
    let b_id = vault.note_id(&path("b.md"));
    assert_eq!(index.resolve("b").unwrap(), Some(b_id));
    assert_eq!(index.resolve("B").unwrap(), Some(b_id));
    assert_eq!(index.resolve("b.md").unwrap(), Some(b_id));
    assert_eq!(
        index.resolve("folder/c").unwrap(),
        Some(vault.note_id(&path("folder/c.md")))
    );
    assert_eq!(
        index.resolve("c").unwrap(),
        Some(vault.note_id(&path("folder/c.md")))
    );
    assert_eq!(index.resolve("nonexistent").unwrap(), None);

    // Backlinks: a links to b, so b's backlinks contain a.
    let backlinks = index.backlinks(b_id).unwrap();
    assert_eq!(backlinks.len(), 1);
    assert_eq!(backlinks[0].src, a_id);

    // Tags: inline + frontmatter both counted.
    let tags = index.tags().unwrap();
    let names: Vec<&str> = tags.iter().map(|t| t.tag.as_str()).collect();
    assert!(names.contains(&"fm-tag"));
    assert!(names.contains(&"inline-tag"));
}

#[test]
fn basename_ambiguity_shortest_path_wins() {
    let vault = vault();
    vault.write(&path("deep/nested/note.md"), b"x").unwrap();
    vault.write(&path("top/note.md"), b"y").unwrap();
    let index = indexed(&vault);
    assert_eq!(
        index.resolve("note").unwrap(),
        Some(vault.note_id(&path("top/note.md")))
    );
    // Full path still reaches the deep one.
    assert_eq!(
        index.resolve("deep/nested/note").unwrap(),
        Some(vault.note_id(&path("deep/nested/note.md")))
    );
}

#[test]
fn attachments_resolve_with_extension() {
    let vault = vault();
    vault.write(&path("assets/Pic.PNG"), b"\x89PNG").unwrap();
    vault.write(&path("a.md"), b"![[pic.png]]").unwrap();
    let index = indexed(&vault);
    assert_eq!(
        index.resolve("pic.png").unwrap(),
        Some(vault.note_id(&path("assets/Pic.PNG")))
    );
    let record = index
        .note(vault.note_id(&path("assets/Pic.PNG")))
        .unwrap()
        .unwrap();
    assert!(!record.is_markdown);
    assert_eq!(record.word_count, None);
}

#[test]
fn unresolved_targets_are_first_class() {
    let vault = vault();
    vault
        .write(&path("a.md"), b"[[Ghost Note]] [[real]]")
        .unwrap();
    vault.write(&path("real.md"), b"exists").unwrap();
    let index = indexed(&vault);
    assert_eq!(index.unresolved_targets().unwrap(), vec!["ghost note"]);
}

#[test]
fn events_keep_index_in_sync() {
    let vault = vault();
    let mut index = Index::open_in_memory([0; 16]).unwrap();

    vault.write(&path("a.md"), b"hello [[b]]").unwrap();
    index
        .handle_event(&vault, &VaultEvent::Created(path("a.md")))
        .unwrap();
    assert_eq!(index.note_count().unwrap(), 1);

    vault.write(&path("a.md"), b"rewritten, no links").unwrap();
    index
        .handle_event(&vault, &VaultEvent::Modified(path("a.md")))
        .unwrap();
    assert!(index.unresolved_targets().unwrap().is_empty());

    vault.remove(&path("a.md")).unwrap();
    index
        .handle_event(&vault, &VaultEvent::Removed(path("a.md")))
        .unwrap();
    assert_eq!(index.note_count().unwrap(), 0);
}

#[test]
fn bulk_change_reconciles_everything() {
    let vault = vault();
    let mut index = Index::open_in_memory([0; 16]).unwrap();
    vault.write(&path("keep.md"), b"stays").unwrap();
    vault.write(&path("gone.md"), b"leaves").unwrap();
    index.rebuild(&vault).unwrap();

    // Changes the index never heard about individually:
    vault.remove(&path("gone.md")).unwrap();
    vault.write(&path("new.md"), b"appeared").unwrap();
    vault.write(&path("keep.md"), b"stays edited").unwrap();

    index.handle_event(&vault, &VaultEvent::BulkChange).unwrap();

    assert_eq!(index.note_count().unwrap(), 2);
    assert!(
        index
            .note(vault.note_id(&path("new.md")))
            .unwrap()
            .is_some()
    );
    assert!(
        index
            .note(vault.note_id(&path("gone.md")))
            .unwrap()
            .is_none()
    );
}

#[test]
fn corpus_indexes_fully() {
    let fs = Arc::new(MemFs::new());
    onyx_testkit::generate(onyx_testkit::CorpusConfig::SMALL, |relative, content| {
        fs.write_atomic(&path(relative), content.as_bytes())
            .unwrap();
    });
    let vault = Vault::new(fs, VaultConfig::default());
    let index = indexed(&vault);
    assert_eq!(index.note_count().unwrap(), 100);
    // Corpus links are constructed to resolve.
    assert!(index.unresolved_targets().unwrap().is_empty());
    assert!(!index.tags().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// THE invariant: incremental == rebuild, for any op sequence.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Write(u8, u8),  // (file slot, content variant)
    Remove(u8),     // file slot
    Rename(u8, u8), // from slot, to slot
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (0u8..8, 0u8..6).prop_map(|(slot, variant)| Op::Write(slot, variant)),
        (0u8..8).prop_map(Op::Remove),
        (0u8..8, 0u8..8).prop_map(|(from, to)| Op::Rename(from, to)),
    ]
}

fn slot_path(slot: u8) -> NotePath {
    // Mix of folders, case variants, unicode, and an attachment slot.
    let name = match slot {
        0 => "notes/alpha.md",
        1 => "notes/Beta.md",
        2 => "beta.md",
        3 => "deep/nested/gamma.md",
        4 => "Ünïcode Nöte.md",
        5 => "assets/image.png",
        6 => "delta.md",
        _ => "notes/epsilon.md",
    };
    path(name)
}

fn content_variant(variant: u8) -> &'static str {
    match variant {
        0 => "",
        1 => "plain text, no structure",
        2 => "# Heading\n[[alpha]] and [[notes/Beta]] #tag",
        3 => "---\ntags: [a, b/c]\n---\nbody [[gamma#Section|alias]]",
        4 => "![[image.png]] and [ext](https://example.com) [[Ghost]]",
        _ => "```\n[[not a link]]\n```\n[[epsilon]] #tag/nested %%[[hidden]]%%",
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Apply random ops with per-op incremental index updates; the result
    /// must equal a from-scratch rebuild of the final vault state.
    #[test]
    fn incremental_equals_rebuild(ops in proptest::collection::vec(op_strategy(), 1..40)) {
        let vault = vault();
        let mut incremental = Index::open_in_memory([0; 16]).unwrap();

        for op in &ops {
            match op {
                Op::Write(slot, variant) => {
                    let target = slot_path(*slot);
                    vault.write(&target, content_variant(*variant).as_bytes()).unwrap();
                    incremental
                        .handle_event(&vault, &VaultEvent::Modified(target))
                        .unwrap();
                }
                Op::Remove(slot) => {
                    let target = slot_path(*slot);
                    let _ = vault.remove(&target); // may not exist — fine
                    incremental
                        .handle_event(&vault, &VaultEvent::Removed(target))
                        .unwrap();
                }
                Op::Rename(from, to) => {
                    let source = slot_path(*from);
                    let destination = slot_path(*to);
                    if source != destination && vault.rename(&source, &destination).is_ok() {
                        // A rename reaches the watcher as remove + create.
                        incremental
                            .handle_event(&vault, &VaultEvent::Removed(source))
                            .unwrap();
                        incremental
                            .handle_event(&vault, &VaultEvent::Created(destination))
                            .unwrap();
                    }
                }
            }
        }

        let mut rebuilt = Index::open_in_memory([0; 16]).unwrap();
        rebuilt.rebuild(&vault).unwrap();

        prop_assert_eq!(incremental.dump().unwrap(), rebuilt.dump().unwrap());
    }
}
