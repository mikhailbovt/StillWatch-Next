#!/usr/bin/env bash
set -euo pipefail

if (( $# != 4 )) || [[ "$1" != linux-x64 && "$1" != windows-x64 ]]; then
    echo 'Usage: build-ffmpeg-capture.sh linux-x64|windows-x64 EMPTY_WORK_DIRECTORY EMPTY_INSTALL_PREFIX JOBS' >&2
    exit 2
fi

target=$1
work=$(realpath -m "$2")
prefix=$(realpath -m "$3")
jobs=$4
if [[ "$work" == "$prefix" || "$prefix" == "$work/"* || ! "$jobs" =~ ^[1-9][0-9]?$ ]]; then
    echo 'Use a separate install directory and between 1 and 99 build jobs' >&2
    exit 2
fi
for directory in "$work" "$prefix"; do
    if [[ -e "$directory" && -n $(find "$directory" -mindepth 1 -maxdepth 1 -print -quit) ]]; then
        echo "Use an empty directory for $directory" >&2
        exit 2
    fi
    install -d "$directory"
done

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
patch="$repo/tools/dev/ffmpeg-same-peer.patch"
version=8.1.3
archive="ffmpeg-$version.tar.xz"
expected=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3

cd "$work"
curl --fail --silent --show-error --location --retry 3 --output "$archive" "https://ffmpeg.org/releases/$archive"
printf '%s  %s\n' "$expected" "$archive" | sha256sum --check --status
tar -xf "$archive"
cd "ffmpeg-$version"
patch --batch --forward -p1 < "$patch"

options=(
    --prefix="$prefix" --arch=x86_64 --enable-shared --disable-static --disable-autodetect
    --disable-doc --disable-programs --enable-network --disable-everything
    --enable-avcodec --enable-avdevice --enable-avformat --enable-avutil --enable-swscale
    --enable-demuxer=mov,matroska,avi,mpegts,rtsp
    --enable-decoder=h264,hevc,av1,vp8,vp9,mpeg2video,mpeg4,mjpeg,rawvideo,h263
    --enable-parser=h264,hevc,av1,vp8,vp9,mpegvideo,mpeg4video,mjpeg,h263
    --enable-protocol=file,tcp,udp,rtp,tls
)
if [[ "$target" == windows-x64 ]]; then
    options+=(--target-os=mingw32 --cross-prefix=x86_64-w64-mingw32- --enable-cross-compile
        --cc=x86_64-w64-mingw32-gcc-win32 --disable-pthreads --enable-w32threads --enable-schannel --enable-indev=dshow)
else
    options+=(--enable-gnutls --enable-indev=v4l2)
fi
./configure "${options[@]}" > "$work/configure.log" 2>&1 || { tail -n 35 "$work/configure.log"; exit 1; }
make -j "$jobs" > "$work/build.log" 2>&1 || { tail -n 35 "$work/build.log"; exit 1; }
make install > "$work/install.log" 2>&1 || { tail -n 35 "$work/install.log"; exit 1; }
if [[ "$target" == windows-x64 ]]; then
    for library in "$prefix"/bin/*.dll; do
        imports=$(x86_64-w64-mingw32-objdump -p "$library")
        if grep -qiE 'DLL Name: (libwinpthread|libgcc|libstdc\+\+)' <<< "$imports"; then
            echo "Unexpected MinGW runtime dependency in $(basename "$library")" >&2
            exit 1
        fi
    done
fi

install -d "$prefix/share/stillwatch-ffmpeg"
install -m 0644 COPYING.LGPLv2.1 "$prefix/share/stillwatch-ffmpeg/LICENSE"
install -m 0644 "$patch" "$prefix/share/stillwatch-ffmpeg/ffmpeg-same-peer.patch"
printf '%s\n' "https://ffmpeg.org/releases/$archive" "$expected" > "$prefix/share/stillwatch-ffmpeg/source.txt"
printf '%q\n' "${options[@]}" > "$prefix/share/stillwatch-ffmpeg/configure.txt"
printf 'Built FFmpeg capture libraries in %s\n' "$prefix"
