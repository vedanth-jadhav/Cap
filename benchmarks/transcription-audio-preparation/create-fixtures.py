import argparse
import json
import pathlib
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("directory", type=pathlib.Path)
args = parser.parse_args()
args.directory.mkdir(parents=True, exist_ok=False)

def ffmpeg(*options):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", *map(str, options)], check=True)

for case, seconds, second_source in [("one-hour-1", 3600, False), ("one-hour-2", 3600, True), ("short-source", 10, False)]:
    directory = args.directory / case
    directory.mkdir()
    ffmpeg("-f", "lavfi", "-i", "color=c=black:s=320x180:r=1", "-t", seconds, "-an", "-c:v", "libx264", "-preset", "ultrafast", "-threads", 2, "-pix_fmt", "yuv420p", directory / "display.mp4")
    ffmpeg("-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000", "-t", seconds, "-ac", 2, "-c:a", "aac", "-b:a", "96k", "-threads", 2, directory / "mic.m4a")
    segment = {"display": {"path": "display.mp4", "fps": 1, "start_time": 0.0}, "mic": {"path": "mic.m4a", "start_time": 0.0}}
    if second_source:
        ffmpeg("-f", "lavfi", "-i", "sine=frequency=660:sample_rate=48000", "-t", seconds, "-ac", 2, "-c:a", "aac", "-b:a", "128k", "-threads", 2, directory / "system.m4a")
        segment["system_audio"] = {"path": "system.m4a", "start_time": 0.0}
    (directory / "recording-meta.json").write_text(json.dumps({"platform": "Linux", "pretty_name": "Transcription preparation benchmark", "sharing": None, "segments": [segment], "status": {"status": "Complete"}}, indent=2) + "\n")
