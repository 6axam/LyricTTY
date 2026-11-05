#!/usr/bin/env python3
"""Enrich one serialized Track without downloading audio or changing its source.

Usage: python3 scripts/resolve_track.py --request FILE
Success writes one Track JSON to stdout; failure writes a safe error to stderr.
Missing metadata stays an error: filenames and title splits are not artist tags.
"""

import argparse
import json
import math
from pathlib import Path
import selectors
import subprocess
import sys
import time
from urllib.parse import unquote, urlsplit


MAX_REQUEST = 64 * 1024
MAX_OUTPUT = 4 * 1024 * 1024
MAX_DURATION = 900


def reject_constant(_value):
    raise ValueError("JSON must not contain non-finite numbers")


def text(value):
    return (isinstance(value, str) and bool(value.strip())
            and not any(ord(char) < 32 or ord(char) == 127 for char in value))


def duration(value, allow_missing=False):
    if allow_missing and (value is None or (not isinstance(value, bool) and value == 0)):
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float, str)):
        raise ValueError("invalid track duration")
    try:
        result = float(value)
    except (ValueError, OverflowError):
        raise ValueError("invalid track duration") from None
    if not math.isfinite(result) or not 0 < result <= MAX_DURATION:
        raise ValueError("track duration must be in (0, 900] seconds")
    return result


def run_json(command, timeout):
    """Bound pipe memory and wall time; inherit the parent's process group.

    Never echo child stderr or command arguments: either may contain signed URLs.
    No new session/group is created, so group cancellation reaches the child.
    """
    try:
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL,
                                 stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    except OSError:
        raise RuntimeError(command[0] + " could not be started") from None
    deadline = time.monotonic() + timeout
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(child.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError(command[0] + " timed out")
                for key, _events in selector.select(remaining):
                    block = key.fileobj.read1(65536)
                    if not block:
                        selector.unregister(key.fileobj)
                        break
                    if len(output) + len(block) > MAX_OUTPUT:
                        raise RuntimeError(command[0] + " output exceeds 4 MiB")
                    output.extend(block)
            try:
                child.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                raise RuntimeError(command[0] + " timed out") from None
        if child.returncode:
            raise RuntimeError(command[0] + " failed")
    finally:
        if child.poll() is None:
            child.kill()
        child.wait()
        child.stdout.close()
    try:
        result = json.loads(output, parse_constant=reject_constant)
    except (ValueError, UnicodeError, RecursionError):
        raise ValueError(command[0] + " returned invalid JSON") from None
    if not isinstance(result, dict):
        raise ValueError(command[0] + " returned invalid metadata")
    return result


def first_tag(tags, *keys):
    for key in keys:
        value = tags.get(key)
        if text(value):
            return value.strip()
    return None


def local_metadata(url, parsed):
    if parsed.scheme:
        if parsed.netloc not in ("", "localhost") or parsed.query or parsed.fragment:
            raise ValueError("file URL must identify a local file")
        path = Path(unquote(parsed.path))
    else:
        path = Path(url)
    path = path.expanduser().resolve()
    if not path.is_file():
        raise ValueError("local media file does not exist")
    info = run_json([
        "ffprobe", "-v", "error", "-show_entries",
        "format=duration:format_tags=title,artist,album,album_artist",
        "-of", "json", str(path),
    ], 8)
    info = info.get("format")
    if not isinstance(info, dict):
        raise ValueError("ffprobe returned no format metadata")
    tags = info.get("tags", {})
    if not isinstance(tags, dict):
        raise ValueError("ffprobe returned invalid tags")
    tags = {key.casefold(): value for key, value in tags.items()}
    return {
        "title": first_tag(tags, "title"),
        "artist": first_tag(tags, "artist", "album_artist"),
        "album": first_tag(tags, "album"),
        "duration": duration(info.get("duration")),
    }


def remote_metadata(url, parsed):
    if not parsed.hostname or parsed.username is not None or parsed.password is not None:
        raise ValueError("HTTP media URL must have a host and no credentials")
    info = run_json([
        "yt-dlp", "--ignore-config", "--no-playlist", "--dump-single-json",
        "--skip-download", "--no-warnings", "--socket-timeout", "8",
        "--retries", "0", "--extractor-retries", "0", "--", url,
    ], 25)
    if ("entries" in info or info.get("_type", "video") != "video"
            or info.get("is_live") or info.get("is_upcoming")
            or info.get("live_status") in ("is_live", "is_upcoming", "post_live")):
        raise ValueError("source must be one finite recording, not a livestream or playlist")
    artist = first_tag(info, "artist")
    if artist is None:
        artists = info.get("artists")
        if isinstance(artists, list) and artists and all(text(item) for item in artists):
            artist = ", ".join(item.strip() for item in artists)
    return {
        "title": first_tag(info, "track", "title"),
        "artist": artist or first_tag(info, "creator", "uploader", "channel"),
        "album": first_tag(info, "album"),
        "duration": duration(info.get("duration")),
        "artwork_url": first_tag(info, "thumbnail"),
    }


def resolve(track):
    if not isinstance(track, dict):
        raise ValueError("request must contain a Track object")
    for key in ("id", "title", "artist", "album"):
        value = track.get(key)
        if not isinstance(value, str) or (value.strip() and not text(value)):
            raise ValueError("track." + key + " must be a valid string")
    for key in ("url", "artwork_url"):
        value = track.get(key)
        if value is not None and (not isinstance(value, str) or (value and not text(value))):
            raise ValueError("track." + key + " must be a string or null")
    raw_duration = track.get("duration")
    if isinstance(raw_duration, str):
        raise ValueError("track.duration must be a number")
    known_duration = duration(raw_duration, allow_missing=True)
    if text(track["title"]) and text(track["artist"]) and known_duration is not None:
        return track
    url = track.get("url")
    if not text(url):
        raise ValueError("incomplete Track requires its actual media URL or local path")
    try:
        parsed = urlsplit(url)
    except ValueError:
        raise ValueError("invalid media URL") from None
    if parsed.scheme in ("", "file"):
        metadata = local_metadata(url, parsed)
    elif parsed.scheme in ("http", "https"):
        metadata = remote_metadata(url, parsed)
    else:
        raise ValueError("unsupported media URL scheme")
    result = dict(track)
    # A native playlist's display title can be a filename or video label.
    # Once we probe the real source, prefer its actual title tag.
    if text(metadata.get("title")):
        result["title"] = metadata["title"]
    for key in ("title", "artist", "album", "artwork_url"):
        if not text(result.get(key)) and text(metadata.get(key)):
            result[key] = metadata[key]
    if known_duration is None:
        result["duration"] = metadata["duration"]
    if not text(result.get("title")) or not text(result.get("artist")):
        raise ValueError("source metadata must provide a title and artist")
    duration(result.get("duration"))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--request", required=True, metavar="FILE")
    args = parser.parse_args()
    try:
        with open(args.request, "rb") as request:
            data = request.read(MAX_REQUEST + 1)
        if len(data) > MAX_REQUEST:
            raise ValueError("request exceeds 64 KiB")
        try:
            track = json.loads(data, parse_constant=reject_constant)
        except (ValueError, UnicodeError, RecursionError):
            raise ValueError("request is not valid JSON") from None
        result = resolve(track)
        print(json.dumps(result, ensure_ascii=False, allow_nan=False))
        return 0
    except (OSError, ValueError, RuntimeError, OverflowError):
        # OSError/URL parser messages can include private paths or signed URLs.
        error = sys.exc_info()[1]
        message = str(error) if isinstance(error, (ValueError, RuntimeError)) else "request or media could not be read"
        print("resolve_track: " + message, file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("resolve_track: cancelled", file=sys.stderr)
        return 130


if __name__ == "__main__":
    sys.exit(main())
