<p align="center"><img src="assets/io.github.andunieee.Cathode.svg" alt="Cathode icon" width="128"></p>

# Cathode
![Cathode playing a video in fullscreen](assets/fullscreen.jpg)

Cathode is a YouTube client for the desktop. Search for a video, hit play, and watch it in the built-in player. It may work.

# Features

## Search
![Search results](assets/search.jpg)

- Search YouTube, with thumbnails, and the matching channels shown above the videos
- Titles in lists in the default language you choose, instead of YouTube's English ones
- Sort by relevance or date, and filter the results
- Paste a YouTube link to open the video directly

## Video pages
![A video page](assets/video.jpg)

- Description, upload date and chapters, with chapters you can play one at a time
- Play, queue, see related videos, save to a collection, or allow in restricted mode
- Choose the video quality

## Queue and player
![The queue and the player](assets/player.jpg)

- A queue next to the player on every page, with downloads in progress shown in it
- Playback moves on to the next video by itself
- Drag videos to reorder the queue
- Picks up where you left off when you reopen the app
- Seekbar, playback speed, volume and fullscreen
- Subtitles in your default language and in the language of the video, automatic captions included, switched in the player settings, with the CC button or with `C`

## Links from the browser
- Cathode handles `cathode://play/?url=<YouTube link>` links, and opens a running window instead of a new one
- `packaging/cathode.user.js` is a userscript (Tampermonkey, Violentmonkey) that sends the page you are on to Cathode with `Alt+C`
- Or from a terminal: `cathode https://youtu.be/dQw4w9WgXcQ`

## Channels
![A channel page](assets/channel.jpg)

- Browse a channel's videos and search inside it
- Playlist pages too

## Subscriptions and collections
![The library sidebar](assets/library.jpg)

- Subscribe to channels without an account, and see their latest videos on the home page
- Keep videos you like in collections, or import a folder of videos you already have

## Downloads
![The downloaded videos page](assets/cached.jpg)

- Watched videos are downloaded and cleaned up after 48 hours unless you save them
- A page to queue, save or delete downloaded videos, one by one or in bulk

## Restricted mode
![Restricted mode settings](assets/restricted-settings.jpg)

- Parental controls: only the videos, channels, playlists and collections you allow can be watched
- Can be protected with a password

![Restricted mode on](assets/restricted.jpg)

## And also
- Back and forward navigation, with Alt+Left/Right and the mouse buttons
- Watch history
- Settings for download quality, sponsor segment skipping, speed limit, proxy and browser cookies for age-restricted videos

# Installation
## One-liners
Each one downloads the latest release.

**Linux** (Flatpak):
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/cathode/releases/latest | grep -o 'https://[^"]*\.flatpak')" -o /tmp/cathode.flatpak && flatpak install --user -y /tmp/cathode.flatpak
```

**macOS** (Apple Silicon):
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/cathode/releases/latest | grep -o 'https://[^"]*macos-arm64[^"]*\.zip')" -o /tmp/cathode.zip && ditto -x -k /tmp/cathode.zip /Applications
```

**Windows** (PowerShell), unzips to `%LOCALAPPDATA%\Cathode`, launch `cathode.bat`:
```
$u=(irm https://api.github.com/repos/andunieee/cathode/releases/latest).assets.browser_download_url -match 'windows-x64'; iwr $u -OutFile $env:TEMP\cathode.zip; Expand-Archive -Force $env:TEMP\cathode.zip $env:LOCALAPPDATA\Cathode
```

## Releases
Every `v*` tag publishes, on GitHub Releases: `.flatpak`, `.deb` (Debian 13+, Ubuntu 25.04+), `.rpm` (Fedora, with [RPM Fusion](https://rpmfusion.org) for `mpv-libs`), `.AppImage` and `.tar.gz` for Linux, a `.zip` for Windows and a `.zip` with `cathode.app` for Apple Silicon macOS.

## Building from source
### Dependencies
* rustc
* GTK 4.10+ and libadwaita 1.7+ (and their development headers)
* libmpv (and its development headers)
* Nothing else: on startup Cathode downloads yt-dlp (kept up to date daily), and ffmpeg/ffprobe and deno if they aren't in your `PATH`, into the `tools` folder of the cache (`~/.cache/cathode/tools` on Linux), verifying their checksums

### Running
Open the search page:
```
cargo run
```
