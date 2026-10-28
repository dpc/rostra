# Source provenance

Baseline: image-rs `jpeg-decoder` 0.3.2, published crate archive:
`https://static.crates.io/crates/jpeg-decoder/jpeg-decoder-0.3.2.crate`.
Archive SHA256:
`00810f1d8b74be64b13dbf3db89ac67740615d6c891f0e7b6179326533011a07`.
The archive declares upstream commit
`eb2d7c0f6a2d0298aba7a7f8b9ca1440353e8f8c`; the archive checksum, not
that declaration, identifies the source bytes. Original MIT/Apache-2.0 licenses
are included unchanged; all retained upstream source keeps that attribution.

Starting reviewed temporary phase-2 patch SHA256:
`f4c6b3fd536d8d78dcb2638bfc28881310df71ed7697c361b8c5cd436edc8edf`
(`phase2.patch`, retained verbatim). Preflight starting SHA256:
`dc148b74aea360a706ad75605a38c8f20e6e7deef1b3871674f74e3cdaea2e61`.
Complete temporary experiment bundle BLAKE3:
`6ab1919894359f64a13e8e92802260353a8280c85653b815ccf448633aa3e2bf`,
SHA256 `5397e4f9795603fcfa3c4b848150e2a2cf6199e9ceaaa52fe8422f096dd1f0ce`.
Those temporary reviews established feasibility only, not production approval.

Production changes preserve the patched entropy/block mechanism, privately
compose preflight/decode/postflight, remove all upstream public exports and the
experimental getter name, freeze former platform_independent/rayon switches to
true/false independent of Cargo features, retain explicit Immediate selection,
and map budget failures to the production taxonomy. Upstream implementation
modules remain recognizable, including unreachable excluded-format machinery.
Formatting and narrowly documented lint compatibility changes are not new codec
algorithms. `source-sha256.txt` identifies the final production Rust sources;
the containing reviewed Rostra commit identifies package wiring and docs.

Fixtures are public synthetic controls from that frozen experiment, selected
without regenerating or rerunning its complete matrix. `phase2-generate.py` is
retained as inert origin documentation, not a build step or instruction to run
the full campaign. The copied repository red fixture is an existing public
Rostra test asset. `fixture-sha256.txt` identifies every selected byte file.
The 17x15 gray/444/422/420 controls have all-zero coefficients, unit DQT, DC0
and EOB codewords. Their mathematical expected samples are exactly 128 (RGB
128,128,128 for YCbCr), alpha 255 if expanded; this is encoder-known constant
color, not a reference derived from this decoder. Host codec-only comparison
may allow one channel level for implementation rounding; broader color/IDCT
agreement is not established by these constant controls.
