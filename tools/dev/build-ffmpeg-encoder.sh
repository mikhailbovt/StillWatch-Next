#!/usr/bin/env bash
set -euo pipefail

if (( $# != 4 )) || [[ "$1" != linux-x64 && "$1" != windows-x64 ]]; then
    echo 'Usage: build-ffmpeg-encoder.sh linux-x64|windows-x64 EMPTY_WORK_DIRECTORY EMPTY_INSTALL_PREFIX JOBS' >&2
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

ffmpeg_version=8.1.3
ffmpeg_archive="ffmpeg-$ffmpeg_version.tar.xz"
ffmpeg_sha=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
openh264_version=2.7.0
openh264_archive="openh264-$openh264_version.tar.gz"
openh264_sha=5178775c8b440073e70920e97d195efff721c0e56d0a78cc82c919e95d54924d
ffmpeg_url="https://ffmpeg.org/releases/$ffmpeg_archive"
openh264_url="https://codeload.github.com/cisco/openh264/tar.gz/refs/tags/v$openh264_version"

cd "$work"
curl --fail --silent --show-error --location --retry 3 --output "$ffmpeg_archive" "$ffmpeg_url"
curl --fail --silent --show-error --location --retry 3 --output "$openh264_archive" "$openh264_url"
printf '%s  %s\n' "$ffmpeg_sha" "$ffmpeg_archive" "$openh264_sha" "$openh264_archive" | sha256sum --check --status
tar -xf "$ffmpeg_archive"
tar -xf "$openh264_archive"

options=(
    --prefix="$prefix" --arch=x86_64 --disable-shared --enable-static --disable-autodetect
    --disable-doc --disable-ffprobe --disable-ffplay --disable-everything --enable-ffmpeg
    --enable-avcodec --enable-avformat --enable-avfilter --enable-avutil --enable-swscale --enable-network
    --enable-libopenh264 --pkg-config-flags=--static
    --enable-demuxer=rawvideo --enable-decoder=rawvideo --enable-encoder=libopenh264
    --enable-muxer=rtsp,rtp --enable-protocol=pipe,tcp,udp,rtp
    --enable-filter=buffer,buffersink,scale,format,trim,setpts,null
)
codec_options=("PREFIX=$work/openh264" ARCH=x86_64)
if [[ "$target" == windows-x64 ]]; then
    codec_options+=(OS=mingw_nt CC=x86_64-w64-mingw32-gcc CXX=x86_64-w64-mingw32-g++ AR=x86_64-w64-mingw32-ar)
    options+=(--target-os=mingw32 --cross-prefix=x86_64-w64-mingw32- --enable-cross-compile
        --pkg-config=pkg-config --extra-ldflags='-static -static-libgcc -static-libstdc++')
fi
cd "openh264-$openh264_version"
make -j "$jobs" "${codec_options[@]}" install-static > "$work/codec.log" 2>&1 || { tail -n 35 "$work/codec.log"; exit 1; }

cd "$work/ffmpeg-$ffmpeg_version"
export PKG_CONFIG_LIBDIR="$work/openh264/lib/pkgconfig"
./configure "${options[@]}" > "$work/configure.log" 2>&1 || { tail -n 35 "$work/configure.log"; exit 1; }
make -j "$jobs" > "$work/build.log" 2>&1 || { tail -n 35 "$work/build.log"; exit 1; }
make install-progs > "$work/install.log" 2>&1 || { tail -n 35 "$work/install.log"; exit 1; }

license="$prefix/share/stillwatch-encoder"
install -d "$license"
install -m 0644 COPYING.LGPLv2.1 "$license/FFmpeg-LICENSE"
install -m 0644 "$work/openh264-$openh264_version/LICENSE" "$license/OpenH264-LICENSE"
install -m 0644 "$work/$openh264_archive" "$license/$openh264_archive"
printf '%s\n' "$ffmpeg_url" "$ffmpeg_sha" "$openh264_url" "$openh264_sha" > "$license/source.txt"
printf '%q\n' "${codec_options[@]}" "${options[@]}" > "$license/configure.txt"
printf 'Built FFmpeg/OpenH264 media encoder in %s\n' "$prefix"
