#!/usr/bin/env python3
"""Obtain and validate transient audio for offline lyric alignment (no playback)."""

import json
import math
from pathlib import Path
import re
import subprocess
import unicodedata
from urllib.parse import unquote, urlparse
import wave

MAX_DURATION = 900
MAX_WAV = 32 * 1024 * 1024
MAX_SOURCE = 24 * 1024 * 1024


def status(message):
    print(json.dumps({"status": message}), flush=True)


def run(command, timeout):
    # Inherit the caller's process group so cancellation also reaches children.
    try:
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                timeout=timeout, check=False)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f"{command[0]} exceeded {timeout}s timeout") from error
    if result.returncode:
        detail = result.stderr.decode("utf-8", "replace")[-2000:].strip()
        raise RuntimeError(f"{command[0]} failed: {detail}")
    return result.stdout.decode("utf-8", "replace")


def duration_value(value):
    if value is None or value == 0:
        return None
    if isinstance(value, bool):
        raise ValueError("invalid track duration")
    result = float(value)
    if not math.isfinite(result) or not 0 < result <= MAX_DURATION:
        raise ValueError("track duration must be in (0, 900] seconds")
    return result


def tolerance(duration):
    return max(2.0, min(4.0, duration * 0.015))


def check_duration(actual, expected):
    if not math.isfinite(actual) or not 0 < actual <= MAX_DURATION:
        raise ValueError("audio duration must be in (0, 900] seconds")
    if expected is not None and abs(actual - expected) > tolerance(expected):
        raise ValueError(f"audio duration {actual:.2f}s does not match track {expected:.2f}s")


def normalized(value):
    # Keep every lexical word, including live/remix/version qualifiers.
    return " ".join(re.findall(r"[^\W_]+", unicodedata.normalize(
        "NFKC", value).casefold(), re.UNICODE))


def artist_key(value):
    # Topic channels sometimes use the Latin spelling of a Cyrillic artist.
    latin = ("a", "b", "v", "g", "d", "e", "e", "zh", "z", "i", "i", "k", "l", "m", "n", "o", "p", "r", "s", "t", "u", "f", "kh", "ts", "ch", "sh", "shch", "", "y", "", "e", "yu", "ya")
    table = dict(zip("абвгдеёжзийклмнопрстуфхцчшщъыьэюя", latin))
    return "".join(table.get(c, c) for c in normalized(value) if c != " ")


def candidate_matches(item, track, expected):
    title, artist = normalized(track.get("title") or ""), normalized(track.get("artist") or "")
    if not title or not artist or expected is None or item.get("is_live"):
        return False
    try:
        check_duration(duration_value(item.get("duration")), expected)
    except (TypeError, ValueError):
        return False
    candidate_title = normalized(item.get("title") or "")
    artists = [normalized(item.get(key) or "") for key in ("artist", "creator", "uploader", "channel")]
    artists = [name.removesuffix(" topic").strip() for name in artists]
    artist_matches = any(artist_key(name) == artist_key(artist) for name in artists)
    # An explicit artist-title video title is also acceptable, without dropping
    # any parenthetical/version words or allowing fuzzy similarity.
    return ((candidate_title == title and artist_matches)
            or candidate_title in (f"{artist} {title}", f"{title} {artist}"))


def prepare_audio(track, work):
    if not isinstance(track, dict):
        raise ValueError("offline request requires a track object")
    expected = duration_value(track.get("duration"))
    url = track.get("url")
    if url is not None and not isinstance(url, str):
        raise ValueError("track.url must be a string or null")
    status("preparing_audio")
    source = None
    if url:
        parsed = urlparse(url)
        if parsed.scheme == "file":
            if parsed.netloc not in ("", "localhost"):
                raise ValueError("file URL must refer to a local file")
            source = Path(unquote(parsed.path)).expanduser().resolve()
        elif not parsed.scheme:
            source = Path(url).expanduser().resolve()
        elif parsed.scheme not in ("http", "https"):
            url = None  # Service-specific links need an exact public-recording lookup.
        if source is not None and not source.is_file():
            raise ValueError("track local audio file does not exist")
    common = ["yt-dlp", "--ignore-config", "--no-playlist", "--socket-timeout", "10",
              "--retries", "1", "--extractor-retries", "1", "--fragment-retries", "1",
              "--no-progress", "--no-warnings"]
    if not url:
        if expected is None or not track.get("artist") or not track.get("title"):
            raise ValueError("audio search requires artist, title, and duration")
        status("searching_audio")
        artist_title = f"{track['artist']} {track['title']}"
        album_query = f"{artist_title} {track.get('album') or ''} provided to youtube"
        candidate = None
        for query in (album_query, artist_title):
            results = json.loads(run(common + ["--dump-single-json", "--skip-download", "--flat-playlist",
                "--playlist-end", "5", "--", f"ytsearch5:{query}"], 40))
            candidate = next((item for item in results.get("entries", [])[:5]
                              if isinstance(item, dict) and candidate_matches(item, track, expected)), None)
            if candidate is not None:
                break
        if candidate is None:
            raise ValueError("no matching recording found; open the exact source URL or local file in LyricTTY")
        url = candidate.get("webpage_url") or candidate.get("original_url") or candidate.get("url")
        if not url or urlparse(url).scheme not in ("http", "https"):
            raise ValueError("search result has no supported public URL")
    if source is None:
        # Known direct audio URLs can be decoded without a site extractor.
        suffix = Path(urlparse(url).path).suffix.lower()
        if suffix in {".mp3", ".m4a", ".aac", ".ogg", ".opus", ".flac", ".wav"}:
            source = url
        else:
            status("downloading_audio")
            output = run(common + ["--max-filesize", "24M", "-f",
                "bestaudio[abr<=96]/worstaudio/bestaudio", "-o", str(work / "source.%(ext)s"),
                "--print", "after_move:filepath", "--", url], 240)
            paths = [Path(line).resolve() for line in output.splitlines() if line.strip()]
            if len(paths) != 1 or paths[0].parent != work.resolve() or not paths[0].is_file():
                raise ValueError("yt-dlp did not return one downloaded audio file")
            source = paths[0]
            if source.stat().st_size > MAX_SOURCE:
                raise ValueError("download exceeds 24 MiB")
    status("validating_audio")
    network = ["-rw_timeout", "10000000"] if isinstance(source, str) else []
    probe = json.loads(run(["ffprobe", "-v", "error", *network, "-show_entries",
        "format=duration", "-of", "json", str(source)], 30))
    check_duration(float(probe["format"]["duration"]), expected)
    destination = work / "audio.wav"
    status("decoding_audio")
    run(["ffmpeg", "-threads", "1", "-nostdin", "-hide_banner", "-loglevel", "error",
         *network, "-i", str(source), "-vn", "-threads", "1", "-ac", "1", "-ar", "16000",
         "-c:a", "pcm_s16le", "-t", "900", "-y", str(destination)], 180)
    if destination.stat().st_size > MAX_WAV:
        raise ValueError("decoded WAV exceeds 32 MiB")
    with wave.open(str(destination), "rb") as audio:
        if (audio.getnchannels(), audio.getsampwidth(), audio.getframerate(), audio.getcomptype()) != (1, 2, 16000, "NONE"):
            raise ValueError("decoder did not produce mono 16 kHz PCM")
        check_duration(audio.getnframes() / 16000, expected)
    return destination
