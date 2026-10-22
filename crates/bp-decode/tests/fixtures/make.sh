#!/usr/bin/env bash
# Regenerates the PCM fixtures. Requires ffmpeg.
set -euo pipefail
cd "$(dirname "$0")"
f() { ffmpeg -y -hide_banner -loglevel error -f lavfi "$@"; }
f -i "anoisesrc=a=0.5:d=0.5:r=44100:s=1" -ac 2 -c:a flac  -sample_fmt s16 flac_16_44.flac
f -i "anoisesrc=a=0.5:d=0.5:r=96000:s=2" -ac 2 -c:a flac  -sample_fmt s32 -bits_per_raw_sample 24 flac_24_96.flac
f -i "anoisesrc=a=0.5:d=0.5:r=192000:s=3" -ac 2 -c:a flac -sample_fmt s32 -bits_per_raw_sample 24 flac_24_192.flac
f -i "anoisesrc=a=0.5:d=0.5:r=44100:s=4" -ac 2 -c:a pcm_s16le wav_16_44_a.wav
f -i "anoisesrc=a=0.5:d=0.3:r=44100:s=5" -ac 2 -c:a pcm_s16le wav_16_44_b.wav
f -i "anoisesrc=a=0.5:d=0.05:r=44100:s=6" -ac 2 -c:a pcm_s16le wav_16_44_tiny.wav
f -i "anoisesrc=a=0.5:d=0.5:r=96000:s=7" -ac 2 -c:a pcm_s24le wav_24_96.wav
f -i "anoisesrc=a=0.5:d=0.5:r=44100:s=8" -ac 2 -c:a pcm_f32le wav_f32_44.wav
f -i "anoisesrc=a=0.5:d=0.5:r=44100:s=9" -ac 2 -c:a pcm_s16be aiff_16_44.aiff
f -i "anoisesrc=a=0.5:d=0.5:r=48000:s=10" -ac 2 -c:a alac -sample_fmt s32p alac_24_48.m4a
f -i "anoisesrc=a=0.5:d=0.5:r=44100:s=11" -ac 2 -c:a pcm_s24le wav_24_44.wav
f -i "anoisesrc=a=0.5:d=0.2:r=176400:s=12" -ac 2 -c:a pcm_s24le wav_24_176.wav
f -i "anoisesrc=a=0.5:d=0.05:r=8000:s=13" -ac 1 -c:a pcm_mulaw wav_mulaw_8.wav
f -i "anoisesrc=a=0.5:d=0.5:r=44100:s=14" -ac 2 -c:a aac -b:a 256k aac_44.m4a
f -i "sine=f=1000:d=1:r=44100,volume=0.5,atrim=end_sample=22050" -ac 2 -c:a aac -b:a 256k aac_sine_a.m4a
f -i "sine=f=1000:d=1:r=44100,volume=0.5,atrim=start_sample=22050,asetpts=N/SR/TB" -ac 2 -c:a aac -b:a 256k aac_sine_b.m4a
