import json
import tempfile
import unittest
from pathlib import Path

from scripts import emacs_conformance


def snapshot(row: int, col: int, *, minibuffer=None):
    return {
        "point": {"row": row, "col": col, "char_offset": col, "byte_offset": col},
        "mark": None,
        "mark_active": False,
        "kill_ring": [],
        "minibuffer": minibuffer,
    }


class EmacsConformanceTest(unittest.TestCase):
    def test_trace_repeat_runs_finds_adjacent_single_chord_commands(self):
        steps = [
            {"index": 7, "depth": 0, "keys": "C-n", "command": "next-line"},
            {"index": 8, "depth": 0, "keys": "C-n", "command": "next-line"},
            {"index": 9, "depth": 0, "keys": "C-n", "command": "next-line"},
            {"index": 10, "depth": 0, "keys": "M-w", "command": "kill-ring-save"},
        ]
        self.assertEqual(
            emacs_conformance.trace_repeat_runs(steps),
            [
                {
                    "start_step": 7,
                    "end_step": 9,
                    "count": 3,
                    "keys": "C-n",
                    "command": "next-line",
                }
            ],
        )

    def test_snapshot_projection_keeps_only_the_asserted_contract(self):
        self.assertEqual(
            emacs_conformance.snapshot_projection(snapshot(2, 3)),
            {
                "point": {"row": 2, "col": 3},
                "mark": None,
                "mark_active": False,
                "kill_ring_head": None,
            },
        )

    def test_import_trace_adds_stepwise_exact_case(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus_path = root / "corpus.json"
            trace_path = root / "trace.json"
            corpus_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "reference": {"emacs_version": "30.2"},
                        "cases": [],
                    }
                ),
                encoding="utf-8",
            )
            trace_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "kind": "herdr-emacs-interactive-trace",
                        "reference": {"emacs_version": "30.2"},
                        "source": {"text": "abc", "sha256": "fixture-hash"},
                        "initial_state": snapshot(0, 0),
                        "steps": [
                            {
                                "index": 0,
                                "depth": 0,
                                "keys": "C-f",
                                "command": "forward-char",
                                "before": snapshot(0, 0),
                                "after": snapshot(0, 1),
                            },
                            {
                                "index": 1,
                                "depth": 0,
                                "keys": "C-f",
                                "command": "forward-char",
                                "before": snapshot(0, 1),
                                "after": snapshot(0, 2),
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            emacs_conformance.import_trace(
                corpus_path, trace_path, "forward-once", "exact", None
            )

            case = json.loads(corpus_path.read_text(encoding="utf-8"))["cases"][0]
            self.assertEqual(case["keys"], "C-f C-f")
            self.assertEqual(case["emacs"]["point"], {"row": 0, "col": 2})
            self.assertEqual(case["steps"][0]["command"], "forward-char")
            self.assertNotIn("input_kind", case["steps"][0])
            self.assertEqual(case["steps"][1]["input_kind"], "repeat")
            self.assertEqual(case["recorded_with"]["repeat_runs"][0]["count"], 2)

    def test_import_trace_rejects_recursive_minibuffer_session(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            corpus_path = root / "corpus.json"
            trace_path = root / "trace.json"
            corpus_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "reference": {"emacs_version": "30.2"},
                        "cases": [],
                    }
                ),
                encoding="utf-8",
            )
            before = snapshot(0, 0)
            after = snapshot(0, 0, minibuffer={"prompt": "Goto line: "})
            trace_path.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "kind": "herdr-emacs-interactive-trace",
                        "reference": {"emacs_version": "30.2"},
                        "source": {"text": "abc", "sha256": "fixture-hash"},
                        "initial_state": before,
                        "steps": [
                            {
                                "index": 0,
                                "depth": 0,
                                "keys": "M-g g",
                                "command": "goto-line",
                                "before": before,
                                "after": after,
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(RuntimeError, "recursive minibuffer"):
                emacs_conformance.import_trace(
                    corpus_path, trace_path, "goto", "exact", None
                )


if __name__ == "__main__":
    unittest.main()
