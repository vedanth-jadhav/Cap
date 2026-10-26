import argparse
import hashlib
import json
import pathlib
import statistics
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("binary", type=pathlib.Path)
parser.add_argument("fixtures", type=pathlib.Path)
parser.add_argument("output", type=pathlib.Path)
parser.add_argument("--repetitions", type=int, default=5)
parser.add_argument("--modes", nargs="+", default=["baseline", "core-batch"])
args = parser.parse_args()
assert args.repetitions > 0
args.output.mkdir(parents=True, exist_ok=False)
rows = []
hashes = {}
for case in ["one-hour-1", "one-hour-2", "short-source"]:
    for repetition in range(1, args.repetitions + 1):
        modes = args.modes if repetition % 2 else list(reversed(args.modes))
        for mode in modes:
            name = f"{case}-{repetition}-{mode}"
            wav = args.output / f"{name}.wav"
            log = args.output / f"{name}.log"
            with log.open("w") as stream:
                subprocess.run(
                    [str(args.binary.resolve()), mode, str((args.fixtures / case).resolve()), str(wav.resolve())],
                    stdout=stream, stderr=subprocess.STDOUT, timeout=180, check=True,
                )
            values = [json.loads(line.split(" ", 1)[1]) for line in log.read_text().splitlines() if line.startswith("AUDIO_PREP ")]
            assert len(values) == 1
            with wav.open("rb") as stream:
                digest = hashlib.file_digest(stream, "sha256").hexdigest()
            if case not in hashes:
                hashes[case] = digest
            assert hashes[case] == digest, f"WAV bytes differ for {name}"
            rows.append(dict(values[0], case=case, repetition=repetition, sha256=digest))
            (args.output / "samples.json").write_text(json.dumps(rows, indent=2) + "\n")
            wav.unlink()
        print(case, repetition, "PASS", flush=True)
    summary = {
        c: {
            mode: {
                "median_seconds": statistics.median(x["total_seconds"] for x in rows if x["case"] == c and x["mode"] == mode),
                "min_seconds": min(x["total_seconds"] for x in rows if x["case"] == c and x["mode"] == mode),
                "max_seconds": max(x["total_seconds"] for x in rows if x["case"] == c and x["mode"] == mode),
            }
            for mode in args.modes
        }
        for c in sorted({x["case"] for x in rows})
    }
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary[case]), flush=True)
