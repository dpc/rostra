# Retained display sources

`Database::read_native_media_for_display(author, reference)` requires
`native-media-jpeg`. It returns the existing `NativeMediaRead`, with original
encoded bytes and original dimensions. It neither rescales nor rewrites media.
Callers must use the author of the actual retained source post. The reader shares
the old readers' mandatory envelope, signature, full identity, lifecycle,
signed/stored length, hash and complete CBOR preallocation checks.
It performs no remote reads, fetch scheduling, writes or signatures.

## Separate source and bitmap limits

| Format | Source geometry | Other admission |
| --- | --- | --- |
| PNG | <=8192 per axis and <=32,000,000 pixels | Noninterlaced above either old limit; complete static raster validation |
| WebP | <=2048 per axis and <=4,000,000 pixels | Existing static WebP policy |
| JPEG | <=2048 per axis and <=4,000,000 pixels | Existing restricted JFIF baseline policy |

All formats still require <=1,000,000 encoded bytes, <=2,000,128 signed body
bytes and <=2,000,137 raw stored-wrapper bytes. The reader returns
`Unsupported` for a large interlaced PNG, even if core could stream its passes:
Android's Skia interlaced path can allocate the entire source before sampling.
Small interlaced PNGs
remain supported. Source geometry or encoded overages are `TooLarge`; malformed
admitted sources are `Invalid`. `Unsupported` does not certify validity.

Callers must opt in through the retained-display reader. Feature unification does
not alter `read_native_media`, `read_native_media_with_jpeg`, either own-image listing,
avatar display, external media, upload normalization or reuse insertion.
Continue using the old reader for reuse insertion. Successful display validation
is not reuse authority.

## PNG validation and resource accounting

`rostra-core::retained_display::validate_retained_png_for_display` checks the
source dimensions and large-PNG noninterlace condition before entering the
shared PNG validator. The complete container walk rejects animation, truncated
chunks, absent IEND and trailing bytes. The 1,000,000-byte source cap bounds IDAT
collection. A separate zlib pass requires end-of-stream, checksum and
complete compressed-input consumption; it rejects inflation beyond
`8 * width * height + 7 * height` bytes. Its fixed scratch is 8192 bytes; it may
produce at most one scratch block beyond the cap before rejecting. At the
source caps the conservative global bound is 256,057,344 bytes of decompression
work per pass, not a retained pixel allocation.

The pinned `png` 0.18.1 reader then checks CRCs and decodes all rows with
`next_row()`, followed by `finish()`. This path does not call `next_frame()` or
allocate a full output raster. Identity transformations retain packed/16-bit
source samples; an output row is at most 65,536 bytes. Upstream
`Reader::read_until_image_data` reserves one output line, not a frame;
`UnfilteringBuffer` starts at at most 128 KiB, compacts consumed rows and grows
in 8192-byte increments. Its compaction threshold is
`max(128 KiB, round_up(4 * raw_row_bytes, 64))`, at most 262,208 bytes.
Re-audit these paths on codec upgrades. As in the old presentation policy, the
validator disables text and ICC payload decoding but does not strip source bytes.
The PNG decoder's unchanged 64 MiB budget is best-effort, not a process RSS cap.

The reader keeps the existing JPEG and WebP geometry limits: their
validators allocate full decoded pixels, and Android sampling cannot reduce
those earlier Rust allocations. PNG validation performs two bounded inflation
passes, followed by a host decode. It adds no CPU deadline, interruptibility,
panic recovery, allocator guarantee or RSS guarantee.

## Host obligations

Acquire one process-wide retained-validation permit, shared across accounts,
display and reuse, **before** calling the reader. Hold it through actual
completion, including after dismissal; do not queue copied media buffers.
The reader validates inline in `block_in_place`, not an isolated worker.
Dispatch it off the UI executor, discard stale results and re-read exact
identity/lifecycle on later display. A later deletion cannot revoke bytes
already returned.

Before bitmap allocation, independently check MIME and original dimensions,
and check the IHDR noninterlace flag for oversized PNG. Choose a sampling factor
that satisfies **both** <=2048 per bitmap axis and <=4,000,000 bitmap pixels;
enforce those bounds again on the decoded result. Thumbnails should use the
host's smaller target. Bound host allocations and bitmap lifetimes separately.
Core success does not establish Android decoder agreement, color/orientation
agreement or platform allocation bounds. Do not describe Android sampling
as bounding full-source decode for other formats or interlaced PNG.
