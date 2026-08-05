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


def oracle_states(result: dict[str, Any]) -> dict[str, dict[str, Any]]:
    states: dict[str, dict[str, Any]] = {}
    for case in result.get("cases", []):
        name = case.get("name", "<unnamed>")
        if "error" in case:
            raise RuntimeError(f"GNU Emacs case {name!r} failed: {case['error']}")
        state = case.get("state")
        if not isinstance(state, dict):
            raise RuntimeError(f"GNU Emacs case {name!r} returned no state")
        if name in states:
            raise RuntimeError(f"GNU Emacs returned duplicate case {name!r}")
        states[name] = state
    return states


def verify(corpus: dict[str, Any], result: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    expected_version = corpus["reference"]["emacs_version"]
    actual_version = result.get("emacs_version")
    if actual_version != expected_version:
        errors.append(
            f"oracle version mismatch: corpus pins GNU Emacs {expected_version}, "
            f"but {actual_version} ran"
        )

    actual_states = oracle_states(result)
    corpus_names = {case["name"] for case in corpus["cases"]}
    extra_names = sorted(set(actual_states) - corpus_names)
    if extra_names:
        errors.append(f"oracle returned unknown cases: {', '.join(extra_names)}")

    for case in corpus["cases"]:
        name = case["name"]
        expected = case.get("emacs")
        actual = actual_states.get(name)
        if actual is None:
            errors.append(f"oracle omitted case {name!r}")
        elif expected != actual:
            errors.append(
                f"{name}: committed GNU Emacs snapshot differs\n"
                f"  committed: {json.dumps(expected, sort_keys=True)}\n"
                f"  observed:  {json.dumps(actual, sort_keys=True)}"
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
    states = oracle_states(result)
    for case in corpus["cases"]:
        try:
            case["emacs"] = states[case["name"]]
        except KeyError as error:
            raise RuntimeError(f"oracle omitted case {case['name']!r}") from error
    corpus_path.write_text(json.dumps(corpus, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("verify", "update"), nargs="?", default="verify")
    parser.add_argument("--emacs", default="emacs", help="GNU Emacs executable")
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    args = parser.parse_args()

    corpus_path = args.corpus.resolve()
    try:
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
