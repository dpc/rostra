# Native semantic Djot documents

`rostra_djot::native::render_semantic(source)` returns
`Result<SemanticDocument, NativeRenderError>`. The existing `render` and
`render_with_media` APIs keep their span output and behavior. This API changes
presentation, not signed source, web rendering, publication or media policy.

## Wire contract, version 1

`SemanticDocument` serializes as `{ "version": 1, "nodes": [...], "warnings": [...] }`.
Each `SemanticNode` serializes its `SemanticKind` fields at the same level as
`children`, for example:

```json
{
  "kind": "heading",
  "level": 2,
  "children": [
    { "kind": "text", "text": "Heading", "children": [] }
  ]
}
```

Consult the public Rust enums for exact fields and tags. All text is owned UTF-8,
not byte offsets. Children follow parser presentation order; the renderer omits
parser-only document/section/span wrappers. Jotup emits a table's caption before
its rows even when the caption follows the table in source.

Blocks include paragraph, heading (exact level), quote, list, list item, fenced
code (language and literal whitespace), rule, table, table row, table cell and
caption. Lists preserve tightness, ordered start as `u64`, numbering
(`decimal`, `alpha_lower`, `alpha_upper`, `roman_lower`, `roman_upper`) and style
(`period`, `paren`, `paren_paren`). Items retain nullable read-only `checked`.
Tables preserve row/cell header flags and cell alignment. Tight lists still
contain paragraph nodes; the host chooses compact spacing from `tight`.

Inline nodes include text, soft/hard breaks, marks, links and images. Marks nest
to preserve combinations: strong, emphasis, code, highlight, insert, delete,
subscript and superscript. Soft breaks normally become wrapping spaces; hard
breaks are explicit. Code is literal text, including tabs/newlines emitted by
the pinned parser, and language is an inert hint, not a highlighter command.
Heading levels and ordered starts are not safe array indices; bound native
layout and use checked marker arithmetic.

Each image appears at its inline occurrence, including empty alternatives and
consecutive images. Its children are alternative text with optional marks.
Depth-first ordered image traversal gives a deterministic occurrence index
within this render; it is not durable identity across edits or parser versions.
The renderer omits images inside outer image alternatives with `media_omitted`,
but retains their alternative text. There is no span-offset image side channel.

Serialization is output-only, not a validation API. Hosts must validate their
entire transport schema before rendering actionable content. Unknown
versions/tags, malformed child shapes, missing required fields, unsupported
integer ranges or exceeded host bounds require the host to reject the whole
document and show inert source text or an unavailable view. Do not skip unknown
nodes or keep a partially clickable prefix.

## Actions and enrichment

Links reuse the legacy `NativeLink` validation and exact full/short reference
types. `destination: null` is inert. `origin` is `authored`, `autolink` or `email`;
email destinations remain unsupported. Preserve authored labels. A host may
replace an autolink profile label with a bounded retained local name, but that
name does not change or authenticate the target. The renderer hides definitions
and does not turn an unresolved reference into a destination.

Hosts must stop inherited link actions at code, raw, math and image boundaries,
even inside a link label. Image alternatives have no descendant link actions.
Do not attach an outer clickable container across such inert children. Code,
raw markup, math and alternatives must never be reparsed as markup or URLs.

Images carry inert `NativeImageDestination` references. Hosts can preserve draft
preview geometry without granting the preview access to media. External
images are not links or fetch instructions. Rostra image reads require the
actual signed source post author and the existing checked retained-media reader.
This API supplies no author, local display name, media bytes or signing authority,
and performs no network reads or route execution.

The host retains its stricter URL checks, short-reference resolution, exact
post-envelope validation, destination disclosure, external confirmation and
action-time revalidation. A successful parse only supplies presentation data.

## Explicit deferred presentation

The renderer emits the deduplicated `unsupported_markup` warning for deferred
nodes. Hosts must show warnings and a readable fallback rather than silently
flattening the feature:

- Math retains literal TeX children and the display flag; show labelled literal
  math, without MathJax, remote rendering or navigation.
- Footnote references retain a label; show a literal `[^label]` marker, with no
  fabricated destination. Definitions retain their label and blocks at parser
  position, with links/media suppressed. Show a labelled definition rather than
  offering reference/backlink navigation.
- Description lists retain explicit list/term/details boundaries. A labelled
  readable fallback is sufficient; native description-list layout is deferred.
- Divisions retain their blocks, with authored classes discarded.

Raw block/inline nodes are separate from deferred nodes: show literal code,
never execute any format, and show `raw_markup`. The renderer discards source
attributes with `attributes_ignored`; hosts must not apply authored HTML
IDs/classes/styles.
Syntax highlighting and provider-specific embeds are outside this API.

## Bounds and failure

The unchanged pre-parser guard runs before eager parser construction:
64 KiB source, at most 64 potential recursive block openers across the entire
source, and the existing excessive-heading-hash guard. The opener guard also
counts siblings and apparent syntax in literal code; it is not merely a depth
limit. Do not remove it because the returned tree is bounded.

During parsing, limits are:

| Resource | Maximum |
| --- | ---: |
| Parser container depth, including transparent/hidden containers | 64 |
| Parser events, including hidden definitions | 65,536 |
| Emitted nodes, including empty containers and text leaves | 8192 |
| Text leaves | 4096 |
| Aggregate owned text/metadata/destination bytes | 128 KiB |
| Accepted destination length | 2048 bytes |
| Images | 32 |
| Table rows, across all tables | 256 |
| Table cells, across all tables | 1024 |

The renderer charges a conservative 128 bytes for identity/media references and
256 bytes for post references. Language, raw-format and footnote labels also
consume the byte budget. JSON punctuation/field names and allocator overhead are not part of
that byte metric; node bounds separately constrain structure.

Oversized or invalid destinations become inert with `invalid_link`, not a
document error. Exceeding a document limit returns only an error: input,
complexity, depth, event, span, output, image, node or table limit. The renderer
never returns a partial tree.
The host must offer labelled source-only text without clickable annotations,
or an unavailable view. Oversized source display needs its own paging/display
bound. These are deterministic bounds, not a hard CPU deadline or RSS ceiling;
render off the UI executor and bound concurrency.

## Consumer fixtures

`src/native/semantic/fixtures/` contains paired `.djot` and `.json` files tested
by `native::semantic::tests::golden_documents`:

- `common`: paragraphs, all marks, breaks, headings, nested quotes/lists,
  ordered metadata, tasks, literal code, rule and aligned captioned table.
- `links-media`: authored/autolinks, reference definitions, invalid destinations,
  empty/consecutive inline media and literal code.
- `references`: full/short profile, post and media references, autolink origin,
  absolute external lookalikes and linked-image alternative isolation.
- `deferred`: math, footnotes, description lists, raw HTML, divisions and ignored
  attributes.

Feed these structured JSON documents into the actual native renderer for UI
acceptance; a fake backend returning the source as one text span does not test
Djot parsing. Rust tests verify source-to-document output and atomic limits.
These fixtures establish the semantic contract, not web pixel
parity or Android rendering acceptance.
