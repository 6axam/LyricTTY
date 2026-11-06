# Known issues

## Intermittent loss of browser audio

Reported on 2026-11-06 while Opera was playing music with LyricTTY open. Audio can disappear during playback. The cause and whether playback pauses or continues silently are not yet confirmed.

Initial inspection: the external-player path sends controls over MPRIS; alignment loads separate audio for analysis, and CAVA reads the playback monitor. No entries were available from PipeWire, pipewire-pulse or WirePlumber in the two-hour user-journal window inspected. This does not rule out a browser, device, scheduling or LyricTTY issue.

At the next occurrence, correlate the timestamp with:

- Opera's MPRIS playback status and position, and whether the browser continues advancing silently.
- The browser audio stream's mute, volume, corked state and selected output device.
- PipeWire/WirePlumber errors or xruns and CPU/memory pressure during alignment.
- LyricTTY playback/volume commands, optional `prepare_before_playback` pauses, and CAVA starting or stopping.

Status: open; not reproduced or attributed. Preserve the report until the cause is identified and a fix is verified.
