# Restricted retained JPEG validation

This package is a separately named in-tree fork, not a general JPEG decoder or a
replacement for transitive `jpeg-decoder` dependencies. Its only operation is
`validate(&[u8])`, returning dimensions or a typed rejection after pixels are
dropped. No decoder, completion counters, scaling, metadata extraction or
configurable decoding API is exported. There are no Cargo features or external
dependencies: platform-independent scalar code and Immediate workers are fixed.

## Fixed subset

Exactly one first thumbnail-free JFIF APP0, version 1.01/1.02, fixed 14-byte
structure, valid units/nonzero densities. One 8-bit baseline SOF0 and one scan:
grayscale ID 1 with 1x1 sampling, or interleaved YCbCr IDs 1/2/3 with Y sampling
1x1, 2x1 or 2x2 and chroma 1x1. Complete bounded, non-redefined 8-bit nonzero
quantization tables and canonical baseline Huffman tables are required. The scan
must contain exactly the dimension-derived blocks/MCUs and terminate at EOI/EOF.

No EXIF, ICC, comments, Adobe markers, thumbnails, other metadata, progressive,
multiple scans, restarts, arithmetic/lossless/extended precision, other sampling,
marker fill or trailing data. This is deliberately narrower than JPEG validity;
Unsupported does not certify that an excluded file is valid.

## Completion invariant

Borrowed preflight independently identifies the scan span, counts unstuffed bytes,
and derives geometry. The private decoder records every bit removal at
`consume_bits`, separately from speculative lookahead. In the admitted single
restart-free scan, synthetic refill can only append a suffix: consumed bits must
not exceed actual entropy bits. Postflight also checks exact fetched real bytes,
blocks, MCUs, one scan, EOI, dimensions/output length, and 0..7 remaining one bits.
Strict baseline run checks reject nonzero-run and ZRL overshoot and progressive
EOB-run shortcuts. No individual decoder statistic is an admission mechanism.

## Resource and failure model

Before decoder construction: encoded input <=1,000,000 bytes, axes 1..2048,
pixels <=4,000,000, <=32 header segments, segment ends <=65,536. File/axis/pixel
overage is TooLarge; malformed admitted content is Invalid; excluded coding,
metadata or header complexity is Unsupported. Zero dimensions are malformed.

Conservative requested buffers: padded planes <=12,582,912 bytes; RGB output
<=12,000,000 (gray reuses a plane); row coefficients plus overlapping replacement
<=163,840; upsampler lines <=12,288, plus bounded tables/descriptors. The large
buffers total about 24.8 MB, **not** an enforced allocator/RSS cap. Input, capacity
overhead, stacks, database storage and host bitmaps are separate. The 12 MB
decoder output-estimate guard is not a scratch cap.

No hard CPU deadline, cancellation, panic freedom, crash recovery or Android
agreement is promised. No catch_unwind converts faults into validation success.
Known reachable panics must be fixed. Hosts bound concurrency; see the DB
retained JPEG contract for the shared permit lifetime. Decoder success does not
authenticate authors or signed bytes. Original bytes are never rewritten.

## Ownership and updates

The Rostra core media maintenance role owns this fork, not a named individual or
an assumed upstream support commitment. Track upstream correctness/security
changes and deliberately assess their relevance to this fixed subset. There is
no automatic semver replacement. Any change to refill/bit removal, block loops,
worker selection, allocation geometry or preflight requires focused regression
tests and independent security review. Preserve default reader/avatar/upload/
normalizer policy. If maintenance cannot continue, disable JPEG eligibility and
preserve opaque original access; do not fall back to unpatched decode success.

See `provenance/README.md` for exact source identities and regression origin.
