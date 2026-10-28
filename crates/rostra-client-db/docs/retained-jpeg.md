# Explicit retained JPEG reading and reuse

Enable `rostra-client-db/native-media-jpeg` and explicitly call
`Database::read_native_media_with_jpeg(author, reference)` or
`Database::list_own_media_with_jpeg(cursor)`. The existing `NativeMediaRead` and
`OwnMediaPage` types are unchanged. Cargo feature activation alone never widens
`read_native_media`, `list_own_media`, native avatars, external media or V1 upload
normalization. There is no JPEG upload/signing/normalization path here.

The opted-in reader delegates PNG/WebP to the existing presentation validator and
JPEG to `rostra-jpeg-validate`'s private composition. Only the exact canonical
`image/jpeg` declaration selects JPEG. The helper's packaged README owns its
fixed JFIF baseline subset, completion invariant, maintenance and buffer bounds.
EXIF/ICC/progressive/camera coverage is intentionally not promised.

Original eligible bytes return unchanged only after decoded validation pixels
are dropped. Existing full-ID, actual source-post author, signature, lifecycle,
hash, raw wrapper/body and complete CBOR preallocation checks all apply. Nothing
fetches, schedules, writes, strips metadata or signs. Re-read exact identity and
lifecycle on later display; a later delete cannot revoke already-returned bytes.

Listing borrows the existing bounded CBOR integer array and walks at most 32
JPEG header segments whose ends are <=65,536 logical bytes. It allocates no
payload or pixels. Existing maximum eight full-body reservations/128 raw-row
budget remains unchanged. Dimensions and `RequiresReadValidation` are hints,
not permission to display or insert: invoke the exact opted-in read on preview
and insertion. Cursors are additionally policy-bound; do not exchange them with
the default reader's listing.

## Host permit and execution

Use **one process-wide retained-image validation permit**, shared across accounts,
viewer and reuse paths. Acquire it before calling the reader, hence before media
materialization, and hold it until actual completion and pixel drop. A nonwaiting
try-acquire is appropriate. Reject/coalesce stale pending UI work; do not queue
already-copied media buffers. Dismissing a view or stopping an await must not
release the permit while validation is still executing. Discard stale results.

The reader awaits the existing DB snapshot read, then validates inline with
`block_in_place`; it spawns no detached task or new worker. `block_in_place` is
not isolation. A UI/current-thread host must dispatch via its own suitable
worker; a synchronous JNI call running on that worker may hold its permit until
return. Candidate listing never decodes pixels and needs no validation permit.

Input <=1 MB, axes <=2048, pixels <=4 M. Named large decoder buffers are bounded
logically at about 24.8 MB, excluding input, allocator overhead, stacks, DB and
host rendering. This is not an enforced RSS cap, CPU deadline, cancellability or
panic-freedom guarantee. No catch_unwind is an admission mechanism. Consumer
panic strategy/OOM can terminate the process. Preserve ordinary FFI panic policy.
Host Android MIME/dimension/color/decode agreement and one-bitmap lifetime remain
separate acceptance gates, not established by core adoption.

TooLarge means file/geometry overage; Invalid means malformed admitted content;
Unsupported means excluded coding/metadata/header complexity, not certified
validity. Suggested presentation: “View supported JPEG images”; on unsupported
content, “This JPEG encoding or metadata is not supported yet; the original is
still available.” Keep Save-original. Describe this as limited baseline JPEG
viewing/reuse, not all-photo support.
