#!/usr/bin/env python3
"""CPU-only forced alignment of known lyrics; complete text coverage or failure."""
import os

try:
    AVAILABLE_THREADS = len(os.sched_getaffinity(0))
except (AttributeError, OSError):
    AVAILABLE_THREADS = os.cpu_count() or 1
try:
    requested_threads = int(os.environ.get("LYRICTTY_ALIGNMENT_THREADS", "0"))
except ValueError:
    requested_threads = 0
THREADS = AVAILABLE_THREADS if requested_threads <= 0 else min(requested_threads, AVAILABLE_THREADS)
THREADS = max(1, THREADS)
for _name in ("OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "MKL_NUM_THREADS"):
    os.environ[_name] = str(THREADS)

import argparse
from contextlib import redirect_stdout
import hashlib
import json
import math
from pathlib import Path
import re
import signal
import sys
import tempfile
import wave

RATE = 16000
VISIBLE = re.compile(r"\S+")
LEXICAL = re.compile(r"[^\W_]", re.UNICODE)
OPTIONS = dict(verbose=None, regroup=False, suppress_silence=False,
               suppress_word_ts=False, vad=False, only_voice_freq=False)


def emit(payload):
    print(json.dumps(payload, ensure_ascii=False, allow_nan=False), flush=True)


def diagnostic(message):
    print(f"alignment: {message}", file=sys.stderr, flush=True)


def number(value):
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError("expected finite numeric timestamp")
    value = float(value)
    if not math.isfinite(value):
        raise ValueError("expected finite numeric timestamp")
    return value


class CoverageError(ValueError):
    pass


class Line:
    def __init__(self, data):
        self.text = data["text"]
        self.start, self.end = number(data["start"]), number(data["end"])
        if not isinstance(self.text, str) or not 0 <= self.start <= self.end:
            raise ValueError("invalid lyric line text or interval")
        self.lexemes = [m for m in VISIBLE.finditer(self.text) if LEXICAL.search(m.group())]
        self.words = None
        if data.get("words"):
            try:
                self.words = self.map_words(data["words"], cached=True)
            except (ValueError, TypeError, KeyError):
                pass
        if not self.lexemes:
            self.words = [{"text": self.text, "start": self.start, "end": self.start}] if self.text else []

    def map_words(self, words, offset=0.0, limit=None, cached=False):
        # Match every non-whitespace character in order. Tokenizer subpieces may
        # split source words, but cannot insert, remove, or substitute characters.
        source = [(i, c) for i, c in enumerate(self.text) if not c.isspace()]
        observed, spans = [], []
        upper = self.end if limit is None else min(self.end, limit)
        for word in words:
            text = word["text"] if cached else word["word"]
            start, end = number(word["start"]) + offset, number(word["end"]) + offset
            if end < start:
                raise CoverageError("reversed acoustic span")
            for char in text:
                if not char.isspace():
                    observed.append(char)
                    spans.append((max(self.start, start), min(upper, end)))
        if "".join(observed) != "".join(c for _, c in source):
            raise CoverageError("tokenizer text does not cover the exact source characters")
        if cached and "".join(w["text"] for w in words) != self.text:
            raise CoverageError("cached text differs from source")
        by_position = dict(zip((i for i, _ in source), spans))
        pieces, cursor = [], 0
        previous = self.start
        for token in self.lexemes:
            acoustic = [by_position[i] for i in range(token.start(), token.end())
                        if LEXICAL.search(self.text[i])]
            if not acoustic or any(not self.start <= a <= b <= upper for a, b in acoustic):
                raise CoverageError(f"missing acoustic span for {token.group()!r}")
            start, end = min(a for a, _ in acoustic), max(b for _, b in acoustic)
            point = start == end
            if point:
                # Preserve the model's point cue for a short particle. Do not
                # manufacture a duration or silently accept collapsed long words.
                lexical_length = sum(bool(LEXICAL.search(c)) for c in token.group())
                before = [span for pos, span in by_position.items() if pos < token.start() and span[1] > span[0]]
                after = [span for pos, span in by_position.items() if pos >= token.end() and span[1] > span[0]]
                if lexical_length > 2 or not before or not after or before[-1][1] > start or after[0][0] < end:
                    raise CoverageError(f"unsupported zero-duration acoustic cue for {token.group()!r}")
            if start < previous:
                raise CoverageError("overlapping source word spans cannot be split without invented timing")
            if token.start() > cursor:
                pieces.append(dict(text=self.text[cursor:token.start()], start=previous, end=previous))
            pieces.append(dict(text=token.group(), start=start, end=end, point=point))
            previous, cursor = end, token.end()
        if cursor < len(self.text):
            pieces.append(dict(text=self.text[cursor:], start=previous, end=previous))
        if "".join(w["text"] for w in pieces) != self.text:
            raise CoverageError("source text reconstruction failed")
        return pieces


def dependencies():
    try:
        with redirect_stdout(sys.stderr):
            import torch
            import stable_whisper
            import whisper
            import numpy as np
            torch.set_num_threads(THREADS)
            torch.set_num_interop_threads(1)
        return torch, stable_whisper, whisper, np
    except ImportError as error:
        raise RuntimeError("install stable-ts (original Whisper), CPU torch and torchaudio; "
                           f"missing dependency: {error}") from error


def validate_model(path, whisper, name):
    expected = whisper._MODELS[name].split("/")[-2]
    if not path.is_file():
        raise ValueError(f"model missing: {path}; run --setup-model --model {name} --models-dir {path.parent}")
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    if digest.hexdigest() != expected:
        raise ValueError(f"Whisper SHA256 validation failed: {path}; rerun --setup-model")


def result_words(result):
    if result is None:
        raise CoverageError("alignment returned no result")
    return [word for segment in result.to_dict()["segments"] for word in segment.get("words", [])]


def run(request, lines, model, np):
    from prepare_audio import prepare_audio, status

    parent = Path(request["work_dir"])
    if not parent.is_dir():
        raise ValueError("work_dir must name an existing temporary directory")
    language = request.get("language")
    if language is None:
        language = "ru" if any(re.search(r"[А-Яа-яЁё]", line.text) for line in lines) else "en"
    if language not in ("ru", "en"):
        raise ValueError("language must be ru or en")
    total = sum(len(line.lexemes) for line in lines)
    matched, failures = 0, []
    failure_reasons = []
    with tempfile.TemporaryDirectory(prefix="alignment-", dir=parent) as temporary:
        pending = any(line.words is None for line in lines)
        audio = None
        if pending:
            path = prepare_audio(request["track"], Path(temporary))
            with wave.open(str(path), "rb") as stream:
                if (stream.getnchannels(), stream.getsampwidth(), stream.getframerate()) != (1, 2, RATE):
                    raise ValueError("prepared audio must be mono 16 kHz PCM16")
                audio = np.frombuffer(stream.readframes(stream.getnframes()), dtype="<i2").astype(np.float32) / 32768.0
        status("aligning_lyrics")
        for index, line in enumerate(lines):
            reason = ""
            if line.words is None:
                first = min(len(audio), math.ceil(line.start * RATE))
                last = min(len(audio), math.floor(line.end * RATE))
                limit = last / RATE
                if last > first:
                    # Keep original cache bounds; only the actual sample window
                    # and the temporary alignment anchor are clipped at EOF.
                    try:
                        with redirect_stdout(sys.stderr):
                            result = model.align_words(audio, [dict(start=first / RATE, end=limit, text=line.text)],
                                                       language=language, normalize_text=False, inplace=False, **OPTIONS)
                        line.words = line.map_words(result_words(result), limit=limit)
                    except Exception as error:
                        reason = str(error)
                    if line.words is None:
                        status(f"retrying_line_{index}")
                        for extra in (dict(dynamic_heads=True), dict(aligner="new")):
                            try:
                                with redirect_stdout(sys.stderr):
                                    result = model.align_words(audio, [dict(start=first / RATE, end=limit, text=line.text)],
                                                               language=language, normalize_text=False, inplace=False,
                                                               **OPTIONS, **extra)
                                line.words = line.map_words(result_words(result), limit=limit)
                                break
                            except Exception as error:
                                reason = str(error)
                    if line.words is None:
                        # Retry only this line. No VAD, silence clipping, word
                        # removal, or heuristic maximum singing-word duration.
                        for heads in (False, True):
                            try:
                                with redirect_stdout(sys.stderr):
                                    result = model.align(audio[first:last], line.text, language=language,
                                                         remove_instant_words=False, original_split=True,
                                                         nonspeech_skip=None, word_dur_factor=None,
                                                         max_word_dur=None, failure_threshold=1.0,
                                                         dynamic_heads=heads, **OPTIONS)
                                line.words = line.map_words(result_words(result), offset=first / RATE, limit=limit)
                                break
                            except Exception as error:
                                reason = str(error)
                else:
                    reason = "source line has no audio samples inside its bounds"
            if line.words is None:
                failures.append(index)
                failure_reasons.append(reason)
                diagnostic(f"line {index}: incomplete coverage (0/{len(line.lexemes)} accepted): {reason}")
                continue
            matched += len(line.lexemes)
            emit(dict(line=index, words=line.words, matched=len(line.lexemes), total=len(line.lexemes)))
        if failures:
            raise CoverageError(f"coverage {matched}/{total}; failed lines {failures}: {failure_reasons[0]}; no prepared result")
        emit(dict(prepared=True, matched=matched, total=total))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline", action="store_true", help="align independently acquired transient track audio")
    parser.add_argument("--request", type=Path)
    parser.add_argument("--model", default="base", choices=("base",))
    parser.add_argument("--models-dir", type=Path, default=Path.home() / ".cache" / "lyrictty" / "whisper")
    parser.add_argument("--setup-model", action="store_true")
    args = parser.parse_args()
    if args.setup_model and (args.request or args.offline):
        parser.error("--setup-model cannot be combined with --request or --offline")
    if not args.setup_model and args.request is None:
        parser.error("alignment requires --request FILE (optionally --offline)")
    try:
        os.nice(10)
    except OSError as error:
        diagnostic(f"could not lower scheduling priority: {error}")
    _, stable_whisper, whisper, np = dependencies()
    root = args.models_dir.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    checkpoint = root / f"{args.model}.pt"
    emit(dict(status="loading_model"))
    if not args.setup_model:
        validate_model(checkpoint, whisper, args.model)
    with redirect_stdout(sys.stderr):
        model = stable_whisper.load_model(args.model if args.setup_model else str(checkpoint),
                                          device="cpu", download_root=str(root))
        model = model.float().eval()
    validate_model(checkpoint, whisper, args.model)
    if args.setup_model:
        emit(dict(status="model_ready"))
        return
    with args.request.open(encoding="utf-8") as stream:
        request = json.load(stream)
    lines = [Line(data) for data in request["lines"]]
    run(request, lines, model, np)


def terminate(_signum, _frame):
    raise KeyboardInterrupt


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate)
    signal.signal(signal.SIGINT, terminate)
    try:
        main()
    except KeyboardInterrupt:
        sys.exit(130)
    except BrokenPipeError:
        sys.exit(1)
    except Exception as error:
        diagnostic(str(error))
        sys.exit(1)
