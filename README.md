# Lumpla
![demo](assets/demo.jpg)

Lumpla is a YouTube client based on [yt-dlp](https://github.com/yt-dlp/yt-dlp), built with GTK4, libadwaita and libmpv. It may work.

Search for a video, open it and hit play, and Lumpla downloads it with yt-dlp into a cache directory and plays the result in the built-in player.

Under the hood, MPV doesn't get its own window. Lumpla gets MPV to draw into a `GtkGLArea`, so the player controls are plain widgets overlaid on the video.

# Features
- [x] Search YouTube (up to 100 results, with thumbnails), sorted by relevance or date
- [x] Channel pages (up to 1000 videos), with search inside the channel
- [x] Video pages with metadata and the upload date
- [x] Back/forward history (also Alt+Left/Right and the mouse buttons)
- [x] Video queue in a side panel: downloads happen one after the other, and playback moves on to the next downloaded video
- [x] Download through yt-dlp, with progress, into a cache (`~/.cache/lumpla`)
- [x] A page listing the cached videos, to queue or delete them one by one or in bulk. The files are the source of truth, `index.json` only remembers their titles and is cleaned up when files are gone
- [x] Play/Pause, seekbar, playback speed (0.5× to 3×), volume, fullscreen
- [x] Parental controls: a restricted profile that only reaches the videos, channels and playlists it allows. There is no search while it is on, and it can be protected with a password. Set it up from the home page, it is saved in `~/.config/lumpla/profiles/default.json`
- [x] Playlist pages
- [x] Local subscriptions, no account needed: a sidebar on the left lists the channels, and the home page shows their latest videos, checked on startup and every 30 minutes (`~/.config/lumpla/subscriptions.json`)
- [x] Watch history, a page listing the videos by day that loads more as you scroll (`~/.config/lumpla/history.jsonl`, one entry per line, the latest first)
- [ ] Everything else

# Installation
## Dependencies
* rustc
* GTK 4.10+ and libadwaita 1.5+ (and their development headers)
* libmpv (and its development headers)
* Nothing else: on startup Lumpla downloads yt-dlp (kept up to date daily), and ffmpeg/ffprobe and deno if they aren't in your `PATH`, into the `tools` folder of the cache (`~/.cache/lumpla/tools` on Linux), verifying their checksums

## Running
Open the search page:
```
cargo run
```

Or play a local file directly:
```
cargo run -- path/to/file.mp4
```
