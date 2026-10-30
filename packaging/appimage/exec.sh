# Sourced last by the AppImage's AppRun (which sets $this_dir): starts the app.
#
# The bundle is built on Debian 13, so its libraries need that glibc (and libstdc++). Hosts
# with an older one get the copies in usr/lib/glibc and usr/lib/libstdc++, the C library
# through its own dynamic loader, given the library path on the command line so that the
# programs we start (yt-dlp, ffmpeg...) keep using the host's. Such a host's libraries are
# older too, so the ones the excludelist leaves to the host come from usr/lib/excluded then
# (pango needs a newer harfbuzz than Ubuntu 22.04's), all but the GL drivers'. Hosts with a
# newer glibc keep theirs: the host's GL drivers, which GTK and mpv load, may need it.

# Whether version $1 is older than $2.
older() {
    [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1)" = "$1" ]
}

host_libs="$({ ldconfig -p || /sbin/ldconfig -p; } 2>/dev/null || true)"
libs=""

# libstdc++.so.6.0.33 -> 6.0.33; no libstdc++ on the host counts as the oldest.
host_stdcxx="$(printf '%s\n' "$host_libs" | sed -n 's/^[[:space:]]*libstdc++\.so\.6 (libc6,x86-64) => //p' | head -n1)"
host_stdcxx="$([ -n "$host_stdcxx" ] && readlink -f "$host_stdcxx" || true)"
host_stdcxx="${host_stdcxx##*/libstdc++.so.}"
bundled_stdcxx="$(cd "$this_dir"/usr/lib/libstdc++ && echo libstdc++.so.6.*)"
bundled_stdcxx="${bundled_stdcxx#libstdc++.so.}"
if [ -z "$host_stdcxx" ] || older "$host_stdcxx" "$bundled_stdcxx"; then
    libs="$this_dir/usr/lib/libstdc++"
fi

# "glibc 2.35"; nothing on musl systems.
host_glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
host_glibc="${host_glibc#glibc }"
bundled_glibc="$(cat "$this_dir"/usr/lib/glibc/version)"
if [ -z "$host_glibc" ] || older "$host_glibc" "$bundled_glibc"; then
    exec "$this_dir"/usr/lib/glibc/ld-linux-x86-64.so.2 \
        --library-path "$this_dir/usr/lib/glibc:$this_dir/usr/lib/excluded${libs:+:$libs}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
        --argv0 "$this_dir"/AppRun.wrapped \
        "$this_dir"/usr/bin/cathode "$@"
fi

[ -n "$libs" ] && export LD_LIBRARY_PATH="$libs${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
exec "$this_dir"/AppRun.wrapped "$@"
