#!/usr/bin/env python3
"""Verify or refresh the GNU Emacs oracle for Herdr TEXT-mode behavior."""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parent.parent
DEFAULT_CORPUS = ROOT / "tests" / "fixtures" / "emacs_conformance.json"
ORACLE = ROOT / "scripts" / "emacs_conformance.el"
RECORDER = ROOT / "scripts" / "emacs_trace.el"


def run_oracle(emacs: str, corpus_path: Path) -> dict[str, Any]:
    executable = shutil.which(emacs)
    if executable is None:
        raise RuntimeError(
            f"GNU Emacs executable {emacs!r} was not found; install the pinned "
            "version or pass --emacs PATH"
        )
    result = subprocess.run(
        [
            executable,
            "-Q",
            "--batch",
            "--script",
            str(ORACLE),
            str(corpus_path),
        ],
        cwd=ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            "GNU Emacs oracle failed:\n"
            f"stdout:\n{result.stdout}\n"
            f"stderr:\n{result.stderr}"
        )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(
            "GNU Emacs oracle returned invalid JSON:\n"
            f"{result.stdout}\n"
            f"stderr:\n{result.stderr}"
        ) from error


def load_corpus(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as file:
        corpus = json.load(file)
    if corpus.get("schema_version") != 1:
        raise RuntimeError(f"unsupported corpus schema in {path}")
    return corpus


def oracle_observations(result: dict[str, Any]) -> dict[str, dict[str, Any]]:
    observations: dict[str, dict[str, Any]] = {}
    for case in result.get("cases", []):
        name = case.get("name", "<unnamed>")
        if "error" in case:
            raise RuntimeError(f"GNU Emacs case {name!r} failed: {case['error']}")
        state = case.get("state")
        if not isinstance(state, dict):
            raise RuntimeError(f"GNU Emacs case {name!r} returned no state")
        if name in observations:
            raise RuntimeError(f"GNU Emacs returned duplicate case {name!r}")
        observations[name] = case
    return observations


def verify(corpus: dict[str, Any], result: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    expected_version = corpus["reference"]["emacs_version"]
    actual_version = result.get("emacs_version")
    if actual_version != expected_version:
        errors.append(
            f"oracle version mismatch: corpus pins GNU Emacs {expected_version}, "
            f"but {actual_version} ran"
        )

    actual_observations = oracle_observations(result)
    corpus_names = {case["name"] for case in corpus["cases"]}
    extra_names = sorted(set(actual_observations) - corpus_names)
    if extra_names:
        errors.append(f"oracle returned unknown cases: {', '.join(extra_names)}")

    for case in corpus["cases"]:
        name = case["name"]
        expected = case.get("emacs")
        observation = actual_observations.get(name)
        if observation is None:
            errors.append(f"oracle omitted case {name!r}")
            continue
        actual = observation["state"]
        if expected != actual:
            errors.append(
                f"{name}: committed GNU Emacs snapshot differs\n"
                f"  committed: {json.dumps(expected, sort_keys=True)}\n"
                f"  observed:  {json.dumps(actual, sort_keys=True)}"
            )
        expected_steps = case.get("steps")
        if expected_steps is not None:
            expected_oracle_steps = [
                {
                    "keys": step["keys"],
                    "command": step["command"],
                    "emacs": step["emacs"],
                }
                for step in expected_steps
            ]
            actual_steps = [
                {
                    "keys": step["keys"],
                    "command": step["command"],
                    "emacs": step["after"],
                }
                for step in observation.get("steps", [])
            ]
            if expected_oracle_steps != actual_steps:
                errors.append(
                    f"{name}: committed command transitions differ\n"
                    f"  committed: {json.dumps(expected_oracle_steps, sort_keys=True)}\n"
                    f"  observed:  {json.dumps(actual_steps, sort_keys=True)}"
                )
    return errors


def update(corpus_path: Path, corpus: dict[str, Any], result: dict[str, Any]) -> None:
    expected_version = corpus["reference"]["emacs_version"]
    actual_version = result.get("emacs_version")
    if actual_version != expected_version:
        raise RuntimeError(
            f"refusing to update a {expected_version} corpus from GNU Emacs "
            f"{actual_version}; change the pin deliberately first"
        )
    observations = oracle_observations(result)
    for case in corpus["cases"]:
        try:
            observation = observations[case["name"]]
        except KeyError as error:
            raise RuntimeError(f"oracle omitted case {case['name']!r}") from error
        case["emacs"] = observation["state"]
        if "steps" in case:
            refreshed_steps = [
                {
                    "keys": step["keys"],
                    "command": step["command"],
                    "emacs": step["after"],
                }
                for step in observation.get("steps", [])
            ]
            previous_steps = case["steps"]
            if len(previous_steps) == len(refreshed_steps):
                for previous, refreshed in zip(previous_steps, refreshed_steps):
                    for key in ("comparison", "reason", "herdr"):
                        if key in previous:
                            refreshed[key] = previous[key]
            case["steps"] = refreshed_steps
    corpus_path.write_text(json.dumps(corpus, indent=2) + "\n", encoding="utf-8")


def snapshot_projection(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Project a rich interactive snapshot onto Herdr's current contract."""
    kill_ring = snapshot.get("kill_ring", [])
    point = snapshot["point"]
    mark = snapshot.get("mark")
    return {
        "point": {"row": point["row"], "col": point["col"]},
        "mark": None if mark is None else {"row": mark["row"], "col": mark["col"]},
        "mark_active": snapshot["mark_active"],
        "kill_ring_head": kill_ring[0] if kill_ring else None,
    }


def import_trace(
    corpus_path: Path,
    trace_path: Path,
    name: str,
    comparison: str,
    reason: str | None,
) -> None:
    with trace_path.open(encoding="utf-8") as file:
        trace = json.load(file)
    if trace.get("schema_version") != 1 or trace.get("kind") != "herdr-emacs-interactive-trace":
        raise RuntimeError(f"{trace_path} is not a supported Emacs interactive trace")
    steps = trace.get("steps")
    if not isinstance(steps, list) or not steps:
        raise RuntimeError("the trace contains no completed commands")
    if any(
        step.get("depth", 0) != 0
        or step.get("before", {}).get("minibuffer") is not None
        or step.get("after", {}).get("minibuffer") is not None
        for step in steps
    ):
        raise RuntimeError(
            "the trace contains recursive minibuffer commands; it is valid diagnostic "
            "data, but importing nested command transitions is not supported yet"
        )
    if comparison == "known-deviation":
        raise RuntimeError(
            "interactive imports start as exact cases; add a reviewed Herdr snapshot "
            "and deviation reason in the corpus if the test exposes a deliberate difference"
        )
    if reason:
        raise RuntimeError("--reason is only valid with --comparison known-deviation")

    corpus = load_corpus(corpus_path)
    if any(case.get("name") == name for case in corpus["cases"]):
        raise RuntimeError(f"the corpus already contains a case named {name!r}")

    initial = trace["initial_state"]
    projected_steps = []
    key_sequences = []
    for step in steps:
        keys = step.get("keys")
        if not isinstance(keys, str) or not keys:
            raise RuntimeError(f"trace step {step.get('index', '?')} has no canonical keys")
        key_sequences.append(keys)
        projected_steps.append(
            {
                "keys": keys,
                "command": step.get("command", "unknown"),
                "emacs": snapshot_projection(step["after"]),
            }
        )

    case = {
        "name": name,
        "text": trace["source"]["text"],
        "start": {
            "row": initial["point"]["row"],
            "col": initial["point"]["col"],
        },
        "keys": " ".join(key_sequences),
        "comparison": comparison,
        "emacs": projected_steps[-1]["emacs"],
        "steps": projected_steps,
        "recorded_with": {
            "emacs_version": trace["reference"]["emacs_version"],
            "source_sha256": trace["source"]["sha256"],
        },
    }
    corpus["cases"].append(case)
    corpus_path.write_text(json.dumps(corpus, indent=2) + "\n", encoding="utf-8")


def record_trace(
    emacs: str,
    input_path: Path,
    output_path: Path,
    row: int,
    col: int,
    terminal: bool,
) -> None:
    executable = shutil.which(emacs)
    if executable is None:
        raise RuntimeError(f"GNU Emacs executable {emacs!r} was not found")
    if not input_path.is_file():
        raise RuntimeError(f"fixture text file does not exist: {input_path}")
    if row < 0 or col < 0:
        raise RuntimeError("--row and --col must be zero or greater")
    expression = (
        "(herdr-emacs-trace-record-file "
        f"{json.dumps(str(input_path))} {json.dumps(str(output_path))} {row} {col} t)"
    )
    command = [executable, "-Q"]
    if terminal:
        command.append("--no-window-system")
    command.extend(["--load", str(RECORDER), "--eval", expression])
    result = subprocess.run(command, cwd=ROOT, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"GNU Emacs recorder exited with status {result.returncode}")
    if not output_path.is_file():
        raise RuntimeError(
            f"Emacs exited without writing {output_path}; finish the recording with C-c C-c"
        )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "action",
        choices=("verify", "update", "record", "import-trace"),
        nargs="?",
        default="verify",
    )
    parser.add_argument("input", nargs="?", type=Path, help="fixture text or trace JSON")
    parser.add_argument("--emacs", default="emacs", help="GNU Emacs executable")
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--output", type=Path, help="output path for record")
    parser.add_argument("--name", help="corpus case name for import-trace")
    parser.add_argument("--comparison", choices=("exact", "known-deviation"), default="exact")
    parser.add_argument("--reason")
    parser.add_argument("--row", type=int, default=0, help="zero-based starting row")
    parser.add_argument("--col", type=int, default=0, help="zero-based starting column")
    parser.add_argument("--terminal", action="store_true", help="run Emacs in this terminal")
    args = parser.parse_args()

    corpus_path = args.corpus.resolve()
    try:
        if args.action == "record":
            if args.input is None or args.output is None:
                raise RuntimeError("record requires INPUT and --output TRACE.json")
            record_trace(
                args.emacs,
                args.input.resolve(),
                args.output.resolve(),
                args.row,
                args.col,
                args.terminal,
            )
            print(f"recorded {args.output.resolve()}")
            return 0
        if args.action == "import-trace":
            if args.input is None or not args.name:
                raise RuntimeError("import-trace requires TRACE.json and --name NAME")
            import_trace(
                corpus_path,
                args.input.resolve(),
                args.name,
                args.comparison,
                args.reason,
            )
            print(f"imported {args.name!r} into {corpus_path}")
            return 0
        corpus = load_corpus(corpus_path)
        result = run_oracle(args.emacs, corpus_path)
        if args.action == "update":
            update(corpus_path, corpus, result)
            display_path = (
                corpus_path.relative_to(ROOT)
                if corpus_path.is_relative_to(ROOT)
                else corpus_path
            )
            print(f"updated {display_path} from GNU Emacs {result['emacs_version']}")
            return 0
        errors = verify(corpus, result)
    except (KeyError, OSError, RuntimeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        print(
            "run `just emacs-conformance-update` after reviewing the reference change",
            file=sys.stderr,
        )
        return 1

    exact = sum(case["comparison"] == "exact" for case in corpus["cases"])
    deviations = len(corpus["cases"]) - exact
    print(
        f"GNU Emacs {result['emacs_version']} agrees with "
        f"{len(corpus['cases'])} committed oracle snapshots "
        f"({exact} exact, {deviations} recorded deviations)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
