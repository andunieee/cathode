# AppRun hook, sourced by the AppImage's AppRun (which sets $this_dir).
#
# libmpv and libavdevice link libpipewire-0.3 and libjack, which the AppImage
# excludelist keeps out of the bundle: they have to match the host's PipeWire/JACK
# daemon, and libpipewire loads its plugins from the host. A host without them
# could not start the app at all though, so the release workflow ships copies in
# usr/lib/fallback/<soname>/ and they only go on the library path when the host
# has no library with that soname.
host_libs="$({ ldconfig -p || /sbin/ldconfig -p; } 2>/dev/null || true)"
for dir in "$this_dir"/usr/lib/fallback/*/; do
    dir="${dir%/}"
    lib="${dir##*/}"
    case "$host_libs" in
        *"$lib (libc6,x86-64)"*) ;;
        *) export LD_LIBRARY_PATH="$dir${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" ;;
    esac
done
unset host_libs dir lib
