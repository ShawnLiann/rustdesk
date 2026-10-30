# Vendored hwcodec — CI stand-in, not for upstream

This directory is a **copy** of `hwcodec`, committed only so this branch can be built and
measured by CI before the dependency PRs land. It is not a fork we intend to maintain here.

## Where it came from

```
upstream base    https://github.com/ShawnLiann/hwcodec  branch pr/macos-pixelbuffer-v2
                 commit 94d44e6 ("declare the pixel-buffer callback type on every platform")
                 → paired with rustdesk-org/hwcodec#53 (decode-side CVPixelBuffer output)
local deltas     - re-enable "h264_videotoolbox" in the codec list (upstream disables it with
                   the comment "h264 encode failed too often, not AV_CODEC_CAP_HARDWARE")
                 - src/ffmpeg_ram/vt_pixbuf_encode.{h,mm}: VTCompressionSession wrapper that
                   takes a BGRA CVPixelBuffer directly (BT.601 tagged) and returns packets
                 - src/ffmpeg_ram/vt_pixbuf.rs: Rust side of that wrapper
                 - src/ffmpeg_ram/encode.rs: `Encoder::encode_pixbuf`, `EncodeFrame: Clone`
                 - build.rs: compile the .mm on macOS, link VideoToolbox/CoreVideo/CoreMedia
                 - scripts/verify-encode-zerocopy.sh, src/ffmpeg_ram/test_vt_pixbuf.m
```

## Why it is vendored instead of pinned

The rustdesk Cargo.toml only ever pins `rustdesk-org/*` revisions; a contributor-fork `rev` is
not something a PR here may commit (this was raised and withdrawn on rustdesk#16732 — see the
CodeRabbit thread on `libs/scrap/src/lib.rs`). Vendoring keeps the fork out of Cargo.toml and
makes the measurement build byte-identical for anyone who checks out the branch.

**Before anything from this branch is proposed upstream**, this directory must be deleted and
replaced by a `rustdesk-org/hwcodec` pin in `libs/scrap/Cargo.toml`, once hwcodec#53 (decode
side) and the matching encode-side entry land there. Deleting `libs/hwcodec-vendor` and the
`[patch]` block in `.cargo/config.toml` must leave a tree that still builds — that is the test
of whether the vendoring was self-contained.

## Refreshing

```bash
git clone https://github.com/ShawnLiann/hwcodec /tmp/hwcodec
git -C /tmp/hwcodec checkout pr/macos-pixelbuffer-v2
# re-apply the local deltas listed above (they are also in this branch's history)
rsync -a --exclude .git --exclude target /tmp/hwcodec/ libs/hwcodec-vendor/
```

## Checking

`scripts/check-vendor-provenance.sh` re-fetches the recorded commit and diffs the whole tree against
this directory, so "what did you change" is answered mechanically instead of by this document:

```
$ libs/hwcodec-vendor/scripts/check-vendor-provenance.sh
added      scripts/verify-encode-zerocopy.sh
...
modified   build.rs
modified   src/ffmpeg_ram/encode.rs
modified   src/ffmpeg_ram/mod.rs
OK: vendor == 94d44e6 + the deltas listed in VENDORING.md
```

Anything outside that list exits non-zero. `--show` prints the diffs themselves.

