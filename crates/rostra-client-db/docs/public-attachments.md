# Durable public opaque attachments

`Client::publish_public_attachment_idempotent(secret, id, &request)` publishes an
existing `SOCIAL_MEDIA` wire event. It is available without `native-media`.
`rostra_client_db::public_attachment_request` exports:

- `PublicAttachmentRequestId(pub [u8; 16])`: random caller-generated ID;
- `PublicAttachmentRequest { data: Vec<u8> }`: exact original bytes;
- `PUBLIC_ATTACHMENT_MIME = "application/octet-stream"`;
- `PUBLIC_ATTACHMENT_MAX_BYTES = 1_000_000`.

Persist the ID, account and immutable original bytes before calling, and retain
them until the result is durably recorded. Never reselect or reinterpret input on
retry. There is no filename, extension, MIME input, codec, normalization or
private-message destination. All accepted bytes are public and untrusted. Hosts
must not infer that acceptance permits automatic display, execution or opening.
Use the existing author-scoped opaque read/export API and its lifecycle rechecks
for later retrieval. This does not loosen image display or normalized image upload.

## Authority and outcomes

The client first requires an open client and a secret matching its account,
including on retries. It does not require active/background state. Hosts enforce
their own caller/session authority; the database methods are trusted integration
primitives, not authentication interfaces.

The return type is the existing `PostResult<VerifiedEvent>`. Storage errors include:

- `DbError::PublicAttachmentInput`: a new input is empty or exceeds the byte cap;
- `DbError::PublicAttachmentRequestMismatch`: an accepted ID is reused with
  different bytes (even if the changed bytes violate today's size policy);
- `DbError::PublicAttachmentCandidate`: wrong owner or invalid local candidate or
  receipt; do not silently assign another request ID on this error;
- existing admission/database errors. Closing uses existing `PostError::ClientClosing`.

Concurrent exact retries return the first committed signed envelope. A new ID
is an independent operation, not a content-deduplication request. Its auxiliary
key is the first 16 BLAKE3 bytes in derive-key domain
`rostra.public_attachment.event_nonce.v1`, over full account bytes then request
ID. No singleton flag is set, and the key does not authorize content.

The receipt digest uses BLAKE3 derive-key domain
`rostra.public_attachment.input.v1`, full account bytes, the fixed MIME's u64
big-endian length and bytes, then the original data's u64 big-endian length and
bytes. It never shares normalized image request IDs, digest policy or receipts.

## Atomic retention and retries

Receipt resolution comes before validation, current admission, candidate identity
or payload lifecycle checks. On a new request the database checks original-byte
limits, the candidate's account/kind/flags/nonce and canonical body hash, and
rejects an already-retained candidate without a matching receipt. Admission,
payload, event and receipt commit in one transaction. A competing accepted retry
returns the original envelope without re-admission.

Accepted retries after pruning/deletion still return that envelope and never
restore content. Closing rejects new client calls; cancellation or an error/lost
acknowledgment around commit does not prove rollback. This provides local
acceptance, not distributed delivery, compare-and-swap or deduplication across
independent/restored database copies.

Schema 43 initializes `media_public_attachment_receipts` in the existing reserved
`media_` namespace. Total rebuild preserves these authoritative rows in place.
Old binaries reject the higher schema version. Receipts never expire and remain
outside payload quotas: they store only a versioned exact-input digest and
original signed public envelope, not source bytes, filenames or secrets. They
still reveal public activity and enable low-entropy input dictionary guesses.
Deleting public content does not erase the receipt or promise forensic erasure.

## Work and memory bounds

The original data is nonempty and at most 1,000,000 bytes. The borrowed adapter
serializes the existing SocialMedia map with `m` and `d`; `d` remains a definite
array of shortest unsigned byte encodings, not a CBOR byte string. No second
original-data vector is cloned. The largest body with the fixed MIME is 2,000,036
bytes, within the existing 2,000,128 native signed-body cap. The database hashes
canonical serialization into a streaming hasher to bind the candidate without
allocating another encoded body.

Existing payload allocation/admission tracks serialized output and its storage
conversion, not caller-owned pending data. There is no new hard RSS/CPU deadline,
streaming upload or permanent attachment cache. Hosts must bound pending input
sizes and concurrent calls, account for their own copies, and keep hashing,
serialization and disk work off the UI executor. No source/private/account
network discovery is added.
