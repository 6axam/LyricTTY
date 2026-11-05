#!/usr/bin/env python3
"""Conservative, transient alignment of sink-monitor audio to known lyrics."""

import os

# Set these before loading Vosk (including libraries it loads indirectly).
os.environ["OMP_NUM_THREADS"] = "1"
os.environ["OPENBLAS_NUM_THREADS"] = "1"

import argparse
import json
import math
from pathlib import Path, PurePosixPath
import re
import selectors
import signal
import stat
import subprocess
import sys
import tempfile
import time
import urllib.request
import zipfile
import wave


MODELS = {
    "ru": "vosk-model-small-ru-0.22",
    "en": "vosk-model-small-en-us-0.15",
}
LEXICAL = re.compile(r"[^\W_]+(?:['’][^\W_]+)*", re.UNICODE)
BYTES_PER_SECOND = 32000
MAX_ZIP = 100 * 1024 * 1024
MAX_UNPACKED = 350 * 1024 * 1024


def diagnostic(message):
    print(message, file=sys.stderr, flush=True)


def normalize(text):
    return text.lower().replace("ё", "е")


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError("expected a finite number")
    value = float(value)
    if not math.isfinite(value):
        raise ValueError("expected a finite number")
    return value


def valid_model(path):
    model_file = path / "am" / "final.mdl"
    return model_file.is_file() and model_file.stat().st_size > 0


def download_model(language, models_dir):
    name = MODELS[language]
    root = Path(models_dir).expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    destination = root / name
    if valid_model(destination):
        diagnostic(f"Using cached model: {destination}")
        return
    if destination.exists():
        raise ValueError(f"Incomplete model exists; move or remove it first: {destination}")
    deadline = time.monotonic() + 600
    url = f"https://alphacephei.com/vosk/models/{name}.zip"
    diagnostic(f"Downloading official model: {url}")
    with tempfile.TemporaryDirectory(prefix=".alignment-", dir=root) as temporary:
        work = Path(temporary)
        archive = work / "model.zip"
        request = urllib.request.Request(url, headers={"User-Agent": "lyrictty-alignment/1"})
        with urllib.request.urlopen(request, timeout=20) as response, archive.open("wb") as out:
            if not response.geturl().startswith("https://alphacephei.com/"):
                raise ValueError("unexpected model download redirect")
            length = response.headers.get("Content-Length")
            if length is not None and int(length) > MAX_ZIP:
                raise ValueError("model archive exceeds download limit")
            received = 0
            while True:
                if time.monotonic() > deadline:
                    raise TimeoutError("model download exceeded 600 seconds")
                chunk = response.read(64 * 1024)
                if not chunk:
                    break
                received += len(chunk)
                if received > MAX_ZIP:
                    raise ValueError("model archive exceeds download limit")
                out.write(chunk)
        extracted = work / "extracted"
        extracted.mkdir()
        with zipfile.ZipFile(archive) as zipped:
            entries = zipped.infolist()
            if len(entries) > 4096 or sum(item.file_size for item in entries) > MAX_UNPACKED:
                raise ValueError("model archive exceeds extraction limits")
            expanded = 0
            for item in entries:
                relative = PurePosixPath(item.filename)
                mode = item.external_attr >> 16
                if (relative.is_absolute() or ".." in relative.parts
                        or "\\" in item.filename or not relative.parts
                        or relative.parts[0] != name
                        or stat.S_ISLNK(mode)
                        or (stat.S_IFMT(mode) not in (0, stat.S_IFREG, stat.S_IFDIR))):
                    raise ValueError("unsafe path or file type in model archive")
                target = extracted.joinpath(*relative.parts)
                if not target.resolve().is_relative_to(extracted.resolve()):
                    raise ValueError("model archive path escapes extraction directory")
                if item.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                    continue
                target.parent.mkdir(parents=True, exist_ok=True)
                with zipped.open(item) as source, target.open("xb") as out:
                    while True:
                        if time.monotonic() > deadline:
                            raise TimeoutError("model setup exceeded 600 seconds")
                        chunk = source.read(64 * 1024)
                        if not chunk:
                            break
                        expanded += len(chunk)
                        if expanded > MAX_UNPACKED:
                            raise ValueError("model exceeds extraction limit")
                        out.write(chunk)
        model = extracted / name
        if not valid_model(model):
            raise ValueError("archive has no valid am/final.mdl")
        model.rename(destination)
    diagnostic(f"Model ready: {destination}")


class Line:
    def __init__(self, data):
        self.start, self.end = number(data["start"]), number(data["end"])
        if self.end < self.start or not isinstance(data["text"], str):
            raise ValueError("invalid lyric line")
        self.text = data["text"]
        self.tokens = list(LEXICAL.finditer(self.text))
        self.expected = [normalize(token.group()) for token in self.tokens]
        self.committed = {}
        self.partial = {}
        self.cursor = 0
        self.last_output = None
        # Seed trustworthy cached pieces, including partially aligned lines.
        words = data.get("words", [])
        if words:
            try:
                exact = "".join(word["text"] for word in words) == self.text
                existing = [normalize(token.group()) for word in words
                            for token in LEXICAL.finditer(word["text"])]
                previous = self.start
                token_index = 0
                for word in words:
                    start, end = number(word["start"]), number(word["end"])
                    count = len(list(LEXICAL.finditer(word["text"])))
                    if (count == 1 and (exact or existing == self.expected)
                            and self.start <= start < end <= self.end and start >= previous):
                        self.committed[token_index] = (start, end)
                        previous = end
                    token_index += count
            except (KeyError, TypeError, ValueError):
                pass
        self.accurate = bool(self.tokens) and len(self.committed) == len(self.tokens)

    def accept(self, items, offset, rate, final):
        cursor = self.cursor
        found = {}
        previous_end = self.committed.get(cursor - 1, (self.start, self.start))[1]
        for item in items:
            try:
                confidence = number(item.get("conf", 0))
                token = normalize(item["word"])
                begin, end = number(item["start"]), number(item["end"])
            except (KeyError, TypeError, ValueError):
                continue
            # Vosk partials can contain zero-duration tokens: they stay untimed.
            if confidence < 0.7 or end <= begin or begin < 0 or not token:
                continue
            candidates = []
            for index in range(cursor, len(self.expected)):
                expected = self.expected[index]
                if token == expected:
                    candidates.append((1.0, -index, index))
            if not candidates:
                continue
            index = max(candidates)[2]
            if index in self.committed:
                previous_end = self.committed[index][1]
                cursor = index + 1
                continue
            start_song, end_song = offset + begin * rate, offset + end * rate
            # Clip measured bounds only; never invent intervals for missing words.
            start_song, end_song = max(self.start, start_song), min(self.end, end_song)
            prior_end = max((interval[1] for i, interval in self.committed.items()
                             if i < index), default=self.start)
            next_start = min((interval[0] for i, interval in self.committed.items()
                              if i > index), default=self.end)
            if not (start_song < end_song and start_song >= max(previous_end, prior_end)
                    and end_song <= next_start):
                continue
            found[index] = (start_song, end_song)
            previous_end = end_song
            cursor = index + 1
        if final:
            self.partial = {}
        else:
            self.partial.update(found)
        if final:
            self.committed.update(found)
            self.cursor = cursor

    def emit(self, index):
        intervals = self.committed | self.partial
        pieces = []
        end = 0
        for token_index, token in enumerate(self.tokens):
            if token.start() > end:
                pieces.append({"text": self.text[end:token.start()],
                               "start": self.start, "end": self.start})
            start_time, end_time = intervals.get(token_index, (self.start, self.start))
            pieces.append({"text": token.group(), "start": start_time, "end": end_time})
            end = token.end()
        if end < len(self.text):
            pieces.append({"text": self.text[end:], "start": self.start, "end": self.start})
        payload = {"line": index, "words": pieces,
                   "matched": len(intervals), "total": len(self.tokens)}
        serialized = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        if serialized != self.last_output:
            print(serialized, flush=True)
            self.last_output = serialized


class Playback:
    def __init__(self, data):
        self.position = number(data["position"])
        self.rate = number(data.get("rate", 1))
        self.playing = data["playing"]
        self.wall = time.monotonic()
        if not isinstance(self.playing, bool) or not 0 < self.rate <= 8:
            raise ValueError("playing must be boolean and rate must be in (0, 8]")

    def at(self, wall=None):
        return self.position + ((time.monotonic() if wall is None else wall) - self.wall) * (
            self.rate if self.playing else 0)


class Aligner:
    def __init__(self, lines, playback, model, recognizer_class):
        self.lines, self.playback = lines, playback
        self.model, self.recognizer_class = model, recognizer_class
        self.selector = selectors.DefaultSelector()
        self.selector.register(sys.stdin.fileno(), selectors.EVENT_READ, "stdin")
        self.capture = None
        self.recognizer = None
        self.active = None
        self.input_buffer = b""
        self.cpu_debt = 0.0
        self.last_wall = time.monotonic()

    def stop(self, flush=False):
        if self.capture is not None:
            process, self.capture = self.capture, None
            self.selector.unregister(process.stdout)
            process.terminate()
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            process.stdout.close()
        if self.active is not None:
            if flush and self.recognizer is not None:
                self.consume(self.recognizer.FinalResult(), final=True)
            line = self.lines[self.active]
            for index, interval in line.partial.items():
                line.committed.setdefault(index, interval)
            line.partial = {}
            line.cursor = 0
            line.accurate = len(line.committed) == len(line.tokens)
            line.emit(self.active)
        self.recognizer = None
        self.active = None

    def start(self, index):
        sink = subprocess.run(["pactl", "get-default-sink"], capture_output=True,
                              text=True, timeout=3, check=True).stdout.strip()
        if not sink or "\n" in sink or "\r" in sink:
            raise ValueError("pactl did not return a default output sink")
        line = self.lines[index]
        # Follow the known word order; suffixes allow starting mid-line.
        # A bag of isolated words discarded exactly the sequence we need.
        grammar = [" ".join(line.expected[start:])
                   for start in range(min(len(line.expected), 80))] + ["[unk]"]
        self.recognizer = self.recognizer_class(
            self.model, 16000, json.dumps(grammar, ensure_ascii=False))
        self.recognizer.SetWords(True)
        self.recognizer.SetPartialWords(True)
        self.rec_wall = time.monotonic()
        self.offset = self.playback.at(self.rec_wall)
        self.samples = 0
        self.pcm_tail = b""
        self.capture = subprocess.Popen([
            "parec", f"--device={sink}.monitor", "--rate=16000", "--channels=1",
            "--format=s16le", "--raw"], stdout=subprocess.PIPE, stderr=sys.stderr,
            bufsize=0)
        os.set_blocking(self.capture.stdout.fileno(), False)
        self.selector.register(self.capture.stdout, selectors.EVENT_READ, "audio")
        self.active = index
        line.emit(index)

    def consume(self, data, final=False):
        result = json.loads(data)
        if final:
            # A final result can omit an already credible partial observation.
            # Preserve it rather than sending zeros over the parent's cache.
            line = self.lines[self.active]
            for index, interval in line.partial.items():
                line.committed.setdefault(index, interval)
            line.partial = {}
        self.lines[self.active].accept(
            result.get("result" if final else "partial_result", []),
            self.offset, self.playback.rate, final)
        self.lines[self.active].emit(self.active)

    def update(self, data):
        update = Playback(data)
        discontinuity = (update.playing != self.playback.playing
                         or update.rate != self.playback.rate
                         or abs(update.position - self.playback.at(update.wall)) > 0.25)
        if discontinuity:
            self.stop()
        elif self.recognizer is not None:
            # Recognizer time is relative to its own creation, not the song.
            self.offset = update.position - (update.wall - self.rec_wall) * update.rate
        self.playback = update

    def run(self):
        try:
            while True:
                now = time.monotonic()
                self.cpu_debt = max(0.0, self.cpu_debt - (now - self.last_wall) * 0.25)
                self.last_wall = now
                position = self.playback.at(now)
                index = next((i for i, line in enumerate(self.lines)
                              if line.start <= position < line.end), None)
                if (not self.playback.playing or index is None
                        or self.lines[index].accurate or not self.lines[index].tokens):
                    if self.capture is not None:
                        self.stop(flush=True)
                elif self.capture is None or self.active != index:
                    self.stop(flush=True)
                    cpu_start = time.process_time()
                    self.start(index)
                    self.cpu_debt += time.process_time() - cpu_start
                # Stop polling readable audio while repaying CPU debt. The pipe
                # is bounded; stale audio triggers a new recognizer below.
                throttled = (self.capture is not None and self.samples > 0
                             and self.cpu_debt > 0.0125)
                if throttled:
                    self.selector.unregister(self.capture.stdout)
                events = self.selector.select(timeout=0.05)
                if throttled:
                    self.selector.register(self.capture.stdout, selectors.EVENT_READ, "audio")
                # Playback commands always precede audio from the same poll.
                events.sort(key=lambda event: event[0].data != "stdin")
                for key, _ in events:
                    if key.data == "stdin":
                        chunk = os.read(sys.stdin.fileno(), 65536)
                        if not chunk:
                            return
                        self.input_buffer += chunk
                        if len(self.input_buffer) > 1024 * 1024:
                            raise ValueError("stdin update exceeds 1 MiB")
                        while b"\n" in self.input_buffer:
                            raw, self.input_buffer = self.input_buffer.split(b"\n", 1)
                            if raw.strip():
                                try:
                                    self.update(json.loads(raw))
                                except (ValueError, KeyError, TypeError) as error:
                                    diagnostic(f"Ignoring invalid playback update: {error}")
                    elif self.capture is not None and key.fileobj is self.capture.stdout:
                        chunk = os.read(key.fd, 6400)
                        if not chunk:
                            raise RuntimeError("parec stopped; output monitor capture ended")
                        # Drop overload audio instead of letting recognizer lag grow.
                        # Reset its sample origin whenever samples are discarded.
                        if (self.samples > 0 and
                                time.monotonic() - self.rec_wall - self.samples / 16000 > 0.75):
                            self.stop()
                            continue
                        chunk = self.pcm_tail + chunk
                        self.pcm_tail = chunk[-1:] if len(chunk) % 2 else b""
                        if self.pcm_tail:
                            chunk = chunk[:-1]
                        if not chunk:
                            continue
                        if self.samples == 0:
                            # Account for parec startup: Vosk's sample zero is
                            # the beginning of captured PCM, not Popen time.
                            self.rec_wall = time.monotonic() - len(chunk) / BYTES_PER_SECOND
                            self.offset = self.playback.at(self.rec_wall)
                        self.samples += len(chunk) // 2
                        cpu_start = time.process_time()
                        if self.recognizer.AcceptWaveform(chunk):
                            self.consume(self.recognizer.Result(), final=True)
                        else:
                            self.consume(self.recognizer.PartialResult())
                        self.cpu_debt += time.process_time() - cpu_start
        finally:
            # FinalResult contains acoustic observations only; never flush across
            # a pause/seek, where its clock would no longer describe this audio.
            if self.recognizer is not None and self.active is not None:
                try:
                    self.consume(self.recognizer.FinalResult(), final=True)
                except (ValueError, RuntimeError, BrokenPipeError):
                    pass
            self.stop()
            self.selector.close()


def run_offline(request, lines, model_path, model_class, recognizer_class):
    from prepare_audio import prepare_audio, status

    # Include model construction in the CPU budget; sleep repays any CPU bursts.
    cpu_origin, wall_origin = time.process_time(), time.monotonic()

    def throttle():
        delay = (time.process_time() - cpu_origin) / 0.4 - (time.monotonic() - wall_origin)
        while delay > 0:
            time.sleep(min(delay, 0.25))
            delay = (time.process_time() - cpu_origin) / 0.4 - (time.monotonic() - wall_origin)

    parent = request.get("work_dir")
    if parent is not None:
        if not isinstance(parent, str) or not Path(parent).is_dir():
            raise ValueError("work_dir must name an existing temporary directory")
    # Always own a child directory, even when Rust supplies the parent workdir.
    with tempfile.TemporaryDirectory(prefix="alignment-audio-", dir=parent) as temporary:
        audio_path = prepare_audio(request.get("track"), Path(temporary))
        cpu_origin, wall_origin = time.process_time(), time.monotonic()
        with wave.open(str(audio_path), "rb") as audio:
            duration = audio.getnframes() / 16000
            # LRC's final display window can extend past the audio's end.
            # Clip the sample slice, preserving the provider's timeline/cache key.
            if any(line.start < 0 or line.start >= duration for line in lines):
                raise ValueError("lyric intervals extend outside the prepared audio")
            status("loading_model")
            model = model_class(str(model_path))
            throttle()
            status("aligning_lyrics")
            for index, line in enumerate(lines):
                if not line.accurate and line.tokens and line.end > line.start:
                    grammar = [" ".join(line.expected[start:])
                               for start in range(min(len(line.expected), 80))] + ["[unk]"]
                    recognizer = recognizer_class(
                        model, 16000, json.dumps(grammar, ensure_ascii=False))
                    recognizer.SetWords(True)
                    line.cursor = 0
                    line.partial = {}
                    # Sample zero describes line.start; measured results are
                    # clipped by Line.accept, never interpolated across silence.
                    first = int(line.start * 16000)
                    last = min(audio.getnframes(), math.ceil(line.end * 16000))
                    audio.setpos(first)
                    remaining = last - first
                    throttle()
                    while remaining > 0:
                        chunk = audio.readframes(min(3200, remaining))
                        if not chunk:
                            raise ValueError("unexpected end of prepared WAV")
                        remaining -= len(chunk) // 2
                        if recognizer.AcceptWaveform(chunk):
                            # Consume every endpoint: FinalResult alone loses
                            # phrases preceding an internal silence endpoint.
                            result = json.loads(recognizer.Result())
                            line.accept(result.get("result", []), line.start, 1, True)
                        throttle()
                    result = json.loads(recognizer.FinalResult())
                    line.accept(result.get("result", []), line.start, 1, True)
                    line.accurate = len(line.committed) == len(line.tokens)
                    del recognizer
                    throttle()
                line.emit(index)
            print(json.dumps({"prepared": True,
                              "matched": sum(len(line.committed) for line in lines),
                              "total": sum(len(line.tokens) for line in lines)}), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true",
                        help="prepare exact track audio and align before playback")
    parser.add_argument("--request", type=Path)
    parser.add_argument("--model", type=Path)
    parser.add_argument("--download-model", choices=MODELS)
    parser.add_argument("--models-dir", type=Path)
    args = parser.parse_args()
    if args.download_model:
        if args.models_dir is None or args.request or args.model or args.offline:
            parser.error("download mode requires --models-dir and no runtime arguments")
        download_model(args.download_model, args.models_dir)
        return
    if args.request is None or args.model is None or args.models_dir is not None:
        parser.error("runtime requires --request JSON_PATH --model MODEL_DIR")
    if not valid_model(args.model):
        raise ValueError(f"model missing am/final.mdl: {args.model}")
    with args.request.open(encoding="utf-8") as source:
        request = json.load(source)
    lines = [Line(item) for item in request["lines"]]
    playback = None if args.offline else Playback(request)
    try:
        os.nice(10)
        if hasattr(os, "sched_getaffinity"):
            os.sched_setaffinity(0, {min(os.sched_getaffinity(0))})
    except OSError as error:
        diagnostic(f"Could not lower scheduling priority: {error}")
    try:
        from vosk import KaldiRecognizer, Model, SetLogLevel
    except ImportError as error:
        raise RuntimeError("Vosk unavailable; run scripts/setup-alignment.sh ru (or en)") from error
    SetLogLevel(-1)
    def terminate(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)
    if args.offline:
        run_offline(request, lines, args.model, Model, KaldiRecognizer)
        return
    diagnostic("Listening only to the default sink monitor; no audio is saved. "
               "Small Vosk models use roughly 300 MB at runtime; singing recognition is limited.")
    model = Model(str(args.model))
    aligner = Aligner(lines, playback, model, KaldiRecognizer)

    aligner.run()


if __name__ == "__main__":
    try:
        main()
    except (KeyboardInterrupt, BrokenPipeError):
        pass
    except Exception as error:
        diagnostic(f"alignment: {error}")
        sys.exit(1)
