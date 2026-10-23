"""Verify the Gemma 4 corpus with the independent Hugging Face tokenizers engine."""

import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]


def main():
    import tokenizers

    if tokenizers.__version__ != "0.22.2":
        raise ValueError("Install tokenizers==0.22.2 to reproduce the fixture.")
    fixture_dir = ROOT / "tests/fixtures/tokenizer"
    fixture = json.loads((fixture_dir / "gemma4.json").read_text(encoding="utf-8"))
    model = (ROOT / fixture["model"]).read_bytes()
    actual_hash = hashlib.sha256(model).hexdigest()
    if actual_hash != fixture["sha256"]:
        raise ValueError(f"Gemma 4 SHA-256 mismatch: {actual_hash}")
    tokenizer = tokenizers.Tokenizer.from_str(model.decode("utf-8"))
    for case in fixture["cases"]:
        if tokenizer.encode(case["text"]).ids != case["ids"]:
            raise ValueError(f"Gemma 4 reference mismatch: {case['name']}")
    print(f"Gemma 4: {len(fixture['cases'])} reference cases passed; model SHA-256 verified.")


if __name__ == "__main__":
    try:
        main()
    except (ImportError, ValueError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
