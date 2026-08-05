use crate::app::App;

use super::tests::emacs_app_with_channel;

#[derive(Debug, serde::Deserialize)]
struct ConformanceCorpus {
    schema_version: u32,
    cases: Vec<ConformanceCase>,
}

#[derive(Debug, serde::Deserialize)]
struct ConformanceCase {
    name: String,
    text: String,
    start: ConformancePosition,
    keys: String,
    comparison: String,
    reason: Option<String>,
    emacs: ConformanceSnapshot,
    herdr: Option<ConformanceSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
struct ConformancePosition {
    row: u32,
    col: u16,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
struct ConformanceSnapshot {
    point: ConformancePosition,
    mark: Option<ConformancePosition>,
    mark_active: bool,
    kill_ring_head: Option<String>,
}

fn terminal_key(chord: crate::emacs::keymap::Chord) -> crate::input::TerminalKey {
    let mut modifiers = crossterm::event::KeyModifiers::empty();
    if chord.ctrl {
        modifiers.insert(crossterm::event::KeyModifiers::CONTROL);
    }
    if chord.meta {
        modifiers.insert(crossterm::event::KeyModifiers::ALT);
    }
    crate::input::TerminalKey::new(chord.code, modifiers)
}

fn herdr_snapshot(app: &App) -> ConformanceSnapshot {
    let text = app
        .state
        .emacs
        .text_mode
        .as_ref()
        .expect("conformance key sequence must remain in TEXT mode");
    ConformanceSnapshot {
        point: ConformancePosition {
            row: text.point.row,
            col: text.point.col,
        },
        mark: text.mark.map(|mark| ConformancePosition {
            row: mark.row,
            col: mark.col,
        }),
        mark_active: text.mark_active,
        kill_ring_head: app.state.emacs.kill_ring.head().map(str::to_owned),
    }
}

/// Differential contract: the same canonical key sequence and text fixture
/// are run by GNU Emacs (committed in the corpus) and through Herdr's
/// production keymap/command dispatcher here. Known differences have their
/// own asserted Herdr snapshot and a mandatory explanation.
#[tokio::test]
async fn emacs_conformance_corpus_matches() {
    let corpus: ConformanceCorpus = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/emacs_conformance.json"
    ))
    .expect("Emacs conformance corpus must parse");
    assert_eq!(corpus.schema_version, 1, "unsupported corpus schema");

    for case in corpus.cases {
        let lines: Vec<&str> = case.text.lines().collect();
        assert_eq!(
            lines.len(),
            10,
            "{}: fixtures must fill the ten-row test terminal",
            case.name
        );
        assert!(
            lines.iter().all(|line| line.chars().count() < 40),
            "{}: fixture lines must not wrap in the test terminal",
            case.name
        );

        let terminal_bytes = case.text.replace('\n', "\r\n").into_bytes();
        let (mut app, _pane, _rx) = emacs_app_with_channel(&terminal_bytes);
        app.route_client_input(vec![0x18, b'[']); // C-x [: enter TEXT mode
        let text = app
            .state
            .emacs
            .text_mode
            .as_mut()
            .expect("TEXT mode must start for a conformance case");
        text.point = crate::emacs::text_mode::Pos {
            row: case.start.row,
            col: case.start.col,
        };
        text.mark = None;
        text.mark_active = false;

        let chords = crate::emacs::keymap::parse_key_seq(&case.keys)
            .unwrap_or_else(|| panic!("{}: invalid key sequence {:?}", case.name, case.keys));
        for chord in chords {
            assert!(
                app.emacs_intercept_key(terminal_key(chord)),
                "{}: {} must be owned by Herdr TEXT mode",
                case.name,
                crate::emacs::keymap::format_seq(&[chord])
            );
        }

        let actual = herdr_snapshot(&app);
        let expected = match case.comparison.as_str() {
            "exact" => {
                assert!(
                    case.herdr.is_none(),
                    "{}: exact cases must use the GNU Emacs snapshot directly",
                    case.name
                );
                &case.emacs
            }
            "known-deviation" => {
                assert!(
                    case.reason
                        .as_deref()
                        .is_some_and(|reason| !reason.is_empty()),
                    "{}: known deviations require a reason",
                    case.name
                );
                let expected = case.herdr.as_ref().unwrap_or_else(|| {
                    panic!("{}: known deviations require a Herdr snapshot", case.name)
                });
                assert_ne!(
                    expected, &case.emacs,
                    "{}: remove a resolved known deviation",
                    case.name
                );
                expected
            }
            other => panic!("{}: unknown comparison mode {other:?}", case.name),
        };
        assert_eq!(&actual, expected, "{}: {}", case.name, case.keys);
    }
}
