//! THE sync invariant: any interleaving of local edits and update
//! exchanges converges, and materialization round-trips exactly.

use onyx_sync::SyncDoc;
use proptest::prelude::*;

#[derive(Debug, Clone)]
enum Step {
    /// Device (0/1/2) sets its text to a variant string.
    Edit(u8, String),
    /// Device `from` sends everything `to` hasn't seen.
    Send(u8, u8),
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0u8..3, "[a-z \n]{0,24}").prop_map(|(device, text)| Step::Edit(device, text)),
        (0u8..3, 0u8..3).prop_map(|(from, to)| Step::Send(from, to)),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Three devices, random edits and partial syncs; a final full
    /// exchange must leave all three with identical text.
    #[test]
    fn any_interleaving_converges(steps in proptest::collection::vec(step_strategy(), 1..30)) {
        let devices = [SyncDoc::new(1), SyncDoc::new(2), SyncDoc::new(3)];

        for step in &steps {
            match step {
                Step::Edit(device, text) => {
                    devices[*device as usize].set_text(text).unwrap();
                }
                Step::Send(from, to) => {
                    if from != to {
                        let update = devices[*from as usize]
                            .export_from(&devices[*to as usize].version())
                            .unwrap();
                        devices[*to as usize].import(&update).unwrap();
                    }
                }
            }
        }

        // Full mesh exchange (two rounds guarantee full propagation).
        for _ in 0..2 {
            for from in 0..3 {
                for to in 0..3 {
                    if from != to {
                        let update = devices[from]
                            .export_from(&devices[to].version())
                            .unwrap();
                        devices[to].import(&update).unwrap();
                    }
                }
            }
        }

        prop_assert_eq!(devices[0].text(), devices[1].text());
        prop_assert_eq!(devices[1].text(), devices[2].text());
    }

    /// materialize(set_text(s)) == s for arbitrary unicode.
    #[test]
    fn materialization_roundtrips(text in "\\PC{0,200}") {
        let doc = SyncDoc::from_text(1, &text).unwrap();
        prop_assert_eq!(doc.text(), text.clone());
        // And again after an edit cycle through a different string.
        doc.set_text("interim").unwrap();
        doc.set_text(&text).unwrap();
        prop_assert_eq!(doc.text(), text);
    }

    /// Snapshot/restore never changes the text.
    #[test]
    fn snapshot_is_faithful(text in "\\PC{0,100}") {
        let doc = SyncDoc::from_text(1, &text).unwrap();
        let restored = SyncDoc::from_snapshot(2, &doc.snapshot().unwrap()).unwrap();
        prop_assert_eq!(restored.text(), text);
    }
}
