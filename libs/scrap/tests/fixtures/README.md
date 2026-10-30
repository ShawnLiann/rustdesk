# Test fixtures

## `two_frames.h264`

Two access units of baseline H.264, 160x96 (so a decoder cannot round the size up to a
macroblock multiple without the test noticing), each with its own SPS/PPS. Small enough to keep in
the tree and deterministic, so the decode test does not need a network fetch or a real capture.

```bash
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i "testsrc=size=160x96:rate=10:duration=0.2" \
  -c:v libx264 -profile:v baseline -pix_fmt yuv420p -g 1 -keyint_min 1 -sc_threshold 0 \
  -x264-params "repeat-headers=1:aud=0:slices=1" \
  -f h264 two_frames.h264
```

sha256 `b9ab0394dca43b282b92ae95c092b22f83516ed9bdf81270d2603daf6cf53cdc`

Used by `surface_handoff_tests` in `src/common/codec.rs`.
