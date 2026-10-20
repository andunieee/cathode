# Lumpla
![demo](assets/demo.jpg)

Lumpla is a YouTube client based on [yt-dlp](https://github.com/yt-dlp/yt-dlp), built with the Slint UI framework and libmpv. It may work.

Search for a video, hit play, and Lumpla downloads it with yt-dlp into a cache directory and plays the result in the built-in player.

Under the hood, MPV doesn't get its own window. Lumpla gets MPV to draw into a framebuffer which is bound to a texture and rendered underneath the UI, so UI elements can be overlaid on the video.

# Features
- [x] Search YouTube (with thumbnails)
- [x] Download through yt-dlp and play from a cache (`~/.cache/lumpla`)
- [x] Play/Pause
- [x] Seekbar and seeking (kinda works)
- [ ] Download progress
- [ ] Everything else

# Installation
## Dependencies
* rustc
* libmpv (and its development headers)
* yt-dlp and ffmpeg, available in your `PATH`

## Running
Open the search page:
```
cargo run
```

Or play a local file directly:
```
cargo run -- path/to/file.mp4
```
