use crate::app::App;

use super::tests::emacs_app_with_channel_at_size;

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
    #[serde(default)]
    steps: Vec<ConformanceStep>,
}

#[derive(Debug, serde::Deserialize)]
struct ConformanceStep {
    keys: String,
    command: String,
    input_kind: Option<String>,
    emacs: ConformanceSnapshot,
    comparison: Option<String>,
    reason: Option<String>,
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
        let lines: Vec<&str> = case.text.split('\n').collect();
        let width = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let height = lines.len().max(1);
        let width = u16::try_from(width)
            .unwrap_or_else(|_| panic!("{}: fixture line is too wide", case.name));
        let height = u16::try_from(height)
            .unwrap_or_else(|_| panic!("{}: fixture has too many lines", case.name));

        let terminal_bytes = case.text.replace('\n', "\r\n").into_bytes();
        let (mut app, _pane, _rx) = emacs_app_with_channel_at_size(&terminal_bytes, width, height);
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

        if case.steps.is_empty() {
            replay_keys(&mut app, &case.name, &case.keys, None);
        } else {
            let joined = case
                .steps
                .iter()
                .map(|step| step.keys.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                joined, case.keys,
                "{}: steps must cover case.keys",
                case.name
            );
            for (index, step) in case.steps.iter().enumerate() {
                replay_keys(&mut app, &case.name, &step.keys, step.input_kind.as_deref());
                let expected = match step.comparison.as_deref().unwrap_or("exact") {
                    "exact" => {
                        assert!(
                            step.herdr.is_none(),
                            "{}: exact step {} must use the GNU Emacs snapshot",
                            case.name,
                            index
                        );
                        &step.emacs
                    }
                    "known-deviation" => {
                        assert!(
                            step.reason
                                .as_deref()
                                .is_some_and(|reason| !reason.is_empty()),
                            "{}: known-deviation step {} requires a reason",
                            case.name,
                            index
                        );
                        let expected = step.herdr.as_ref().unwrap_or_else(|| {
                            panic!(
                                "{}: known-deviation step {} requires a Herdr snapshot",
                                case.name, index
                            )
                        });
                        assert_ne!(
                            expected, &step.emacs,
                            "{}: remove resolved deviation from step {}",
                            case.name, index
                        );
                        expected
                    }
                    other => panic!(
                        "{}: step {} has unknown comparison mode {other:?}",
                        case.name, index
                    ),
                };
                assert_eq!(
                    herdr_snapshot(&app),
                    *expected,
                    "{}: step {} ({}, {})",
                    case.name,
                    index,
                    step.command,
                    step.keys
                );
            }
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

fn replay_keys(app: &mut App, case_name: &str, keys: &str, input_kind: Option<&str>) {
    let chords = crate::emacs::keymap::parse_key_seq(keys)
        .unwrap_or_else(|| panic!("{case_name}: invalid key sequence {keys:?}"));
    if input_kind == Some("repeat") {
        assert_eq!(
            chords.len(),
            1,
            "{case_name}: repeat delivery requires a single chord"
        );
    } else if let Some(other) = input_kind {
        panic!("{case_name}: unknown input kind {other:?}");
    }
    for chord in chords {
        let mut key = terminal_key(chord);
        if input_kind == Some("repeat") {
            key = key.with_kind(crossterm::event::KeyEventKind::Repeat);
        }
        assert!(
            app.emacs_intercept_key(key),
            "{}: {} must be owned by Herdr TEXT mode",
            case_name,
            crate::emacs::keymap::format_seq(&[chord])
        );
    }
}
