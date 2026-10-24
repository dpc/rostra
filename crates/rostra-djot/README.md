# rostra-djot

Djot parsing utilities used by Rostra content handling. The crate extracts
links and recognizes Rostra mentions from Djot content.

## Native presentation

`native::render(&str)` returns owned text segments with strong/emphasis/code
flags and typed full/short identity or credential-free HTTP(S) destinations.
Segments avoid byte/UTF-16 offset translation. Raw markup is literal code,
attributes are discarded, images become labelled alternative text, and other
unsupported structures flatten with warnings. No HTML, media loading, network
requests, or event-reference routing is produced. Existing web rendering and
signed content formats are unchanged.

`native::render_with_media(&str)` opts into inert image records alongside spans,
without reading any bytes. Each record has literal `alt`, `span_index` (insert
before that span, or at the end), and a destination distinct from ordinary links:
`Rostra { reference }`, `External { url }`, or `Invalid`. Records preserve source
order, including empty alternatives and consecutive images sharing an anchor.
This mode puts alternative text in image records rather than duplicating it in
spans. At most 32 records are returned and their alt/destination bytes share the
ordinary aggregate output budget. The text-only `render` behavior is unchanged.

Rostra media references preserve full versus short event IDs, serialized as
`{ "kind": "full" | "short", "id": "..." }`. They require a checked retained read
bound to the actual source post author; labels supply no authority. A matching
short prefix never proves a requested full ID. External image destinations remain
inert and must not initiate third-party fetches or ordinary link navigation.
`rostra-client-db`'s opt-in `native-media` reader supplies the bounded signed
static PNG/WebP presentation path; no HTML/WebView or divergent Djot parser is
needed.

Hosts must show warnings, resolve short identities only through their retained
identity index (unknown prefixes remain inert), and disclose actual destinations
rather than trusting link labels. External navigation requires explicit user
action. Serialization is output-only: arbitrary deserialized DTOs would not
inherit the renderer's validation.

Limits are 64 KiB source, 64 potential recursive block openers, 64 emitted
container depth, 65,536 events, 4096 spans, 2048 bytes per destination, and
128 KiB aggregate output text plus repeated destinations. Identity destinations
are charged a conservative 128 bytes each. Oversized destinations become inert;
document limits return an error, never partial/truncated output. Hosts may show
explicitly labelled source-only text without clickable annotations on failure.
Source-only presentation needs its own display/paging bound for oversized input.

The **pre-parser block-opener budget is not a nesting-only limit**. It counts
potential recursive quote/list/footnote/div openers across the entire document,
including siblings and candidates inside code/raw blocks. Thus more than 64 flat
list items can fall back to source. Runs of more than 65,535 `#` bytes also fall back
before the parser's heading-level conversion. Normal inline formatting and URL
punctuation do not consume the opener budget. The guard is audited against the
exact pinned jotup 0.10.0 recursive entrypoints; upgrades require a fresh audit.
Post-parser event/output limits alone cannot protect that eager recursive pass.
These are deterministic input/presentation bounds, not CPU deadlines or process
memory guarantees; hosts should bound concurrency and render off the UI executor.

The same pre-parser guard protects `mention::contains_mention`, which runs during
remote SocialPost ingestion. Over-budget content returns `false` without parsing.
Its signed event and original content remain eligible for ordinary ingestion, but
its mention index/notification may be omitted, even for a flat list exceeding
64 items. In-budget full/short mention matching is unchanged. This guard does not
cover the separate excerpt helpers or the web renderer.

It is an implementation crate in the Rostra publication set. Applications that
need a networked Rostra client should use
[`rostra-client`](https://docs.rs/rostra-client).

Source and issue tracking: <https://github.com/dpc/rostra>

## License

Licensed under MIT OR Apache-2.0 OR MPL-2.0. See
[LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE), and
[LICENSE-MPL](LICENSE-MPL).
