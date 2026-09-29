# Argument for qualified direct-message security

## Scope and model

This implementation-grounded argument addresses the exact property and immediate
assumptions in [CLAIM-direct-message-security](../CLAIM-direct-message-security.md).
It is not an independent verification result. The assumptions are axioms here,
not facts established by source inspection or tests.

The scope is text messages produced by the current `rostra-dm` native-age
profile, their signed `DIRECT_MESSAGE` events and `DM_DEVICE` announcements,
the per-account database reducers and rebuild path, full-client activation and
background processing, and `/messages` and `/settings/messages` HTTP workflows.
Relevant source boundaries are:

- [crypto envelope and strict parser](../../crates/rostra-dm/src/envelope.rs),
  [body](../../crates/rostra-dm/src/body.rs),
  [public keys](../../crates/rostra-dm/src/public_key.rs),
  [announcement](../../crates/rostra-dm/src/announcement.rs),
  [epoch](../../crates/rostra-dm/src/epoch.rs), and
  [sticky device reducer](../../crates/rostra-dm/src/device.rs);
- [selection and local lifecycle](../../crates/rostra-client-db/src/dm.rs),
  [atomic send and receive](../../crates/rostra-client-db/src/dm_receive.rs),
  [eligibility index](../../crates/rostra-client-db/src/dm_index.rs),
  [migration](../../crates/rostra-client-db/src/dm_migration.rs), and
  [bounded private views](../../crates/rostra-client-db/src/dm_view.rs);
- [publication and worker](../../crates/rostra-client/src/client/dm.rs),
  [serialized activation](../../crates/rostra-client/src/client.rs);
- [private routes](../../crates/rostra-web-ui/src/routes/messages.rs),
  [session guard](../../crates/rostra-web-ui/src/routes/messages/session.rs),
  [device forms](../../crates/rostra-web-ui/src/routes/messages/settings.rs),
  and [response middleware](../../crates/rostra-web-ui/src/routes.rs).

An outsider may observe public graph data, learn all public destination keys,
submit signed or malformed events through ordinary ingestion, and use an
anonymous or public-only HTTP session. Concurrent local requests and background
work, delayed/reordered announcements, wall-clock corrections, admission failure,
transaction rollback, restart, and supported total projection rebuilds are in
scope. Authorized plaintext readers and holders of selected keys are not
outsiders. All full installations of one account share signing authority.

The durable receive predicate is: a history winner keyed by `(full sender,
random message ID)` came from a verified signed event with matching content,
successful complete native-age authentication, and encrypted participants
containing the local account with encrypted sender equal to the event author.
Local sent history instead comes from the trusted publisher's exact constructed
body committed with its ciphertext; it deliberately need not be decryptable by
the sending installation.

## Argument

1. **`code`, `test` — Destinations are authenticated account statements.**
   Generic ingestion verifies the event and content before `DM_DEVICE`
   projection. The fixed versioned announcement decoder rejects invalid
   canonical/non-contributory keys, malformed deadlines, advance publication,
   and unsupported framing. Reduction is scoped by the full signed author and
   random device ID. The newest `(timestamp, event ID)` active statement wins;
   retirement is permanent and removes eligibility without deleting old receive
   secrets. Selection checks the newest statement, never falls back to an older
   eligible key, and excludes the sending installation. It requires at least one
   recipient device, allocates four recipient and four sender-other slots with
   spillover, and records selected states in a send permit. The write transaction
   checks fresh wall time, every selected state's identity, and local
   retirement/re-enrollment before admitting a send. The dyadic index changes
   query cost, not the authoritative selection predicate.
   Regression mechanisms include
   `dm_send_rejects_learned_retirement_and_supersession_atomically`,
   `dm_local_retirement_rejects_existing_and_new_send_permits`, and the
   differential eligibility-index tests.

2. **`code`, `assumption` — Every slot uses the same native construction.**
   `encrypt` turns each selected canonical key into a native age recipient,
   fills the list to exactly eight using fresh throwaway X25519 identities, and
   shuffles it. It passes all eight recipients to the pinned age 0.12.1
   encryptor, writes the padded body, and finishes the complete file.
   Native X25519 wrapping independently encapsulates the same random file key
   for each recipient; duplicates are still separately wrapped. A dummy is a
   genuine native recipient, not random bytes with a detectable invalid tag.
   Thus the assumed native wrapping confidentiality/concealment applies equally
   to real and dummy slots. This argument relies on the stated primitive
   assumption; it is not a new cryptographic anonymity proof for age.

3. **`code`, `test` — Public framing adds only the accepted size leakage.**
   Sender, recipient, random message ID, exact text length, and text are in the
   encrypted body. The metadata has fixed width, and text is padded inside
   encryption to one of five buckets. The outer frame holds an exact age-file
   length, the file, and zero padding to the fixed bucket frame size. The strict
   profile accepts exactly eight native stanzas and at most one bounded native
   GREASE stanza, independent of real-slot count. Native header variability
   comes from library-generated GREASE, not destinations or exact text length.
   No cleartext recipient identifier or real-slot count is added.
   `rostra-dm` tests exercise every bucket, all real-slot counts, duplicate
   destinations, GREASE lengths, canonical keys, and malformed profiles; the
   [format derivation](../../crates/rostra-dm/README.md) records the concrete
   header/frame bounds. Public eligibility and the 4+4 rule can nevertheless
   narrow recipients without breaking this envelope property.

4. **`code`, `test`, `assumption` — Receive history authenticates the full body.**
   The background transaction revalidates kind, zero auxiliary key,
   non-singleton flag, retained lifecycle state, account signature, full content
   length, and content hash. Before general age parsing, `validate_frame`
   bounds the frame, header, stanza profile, and exact one-chunk file length.
   Decryption reads the complete expected plaintext and performs an additional
   EOF read; accepting a valid prefix is insufficient. Body decoding checks
   version, exact length, canonical padding, UTF-8, signed-sender equality, and
   local participation. Only then is history inserted. Signature and age
   authentication assumptions exclude outsider forgery at these checks.
   First-winner deduplication does not silently replace text: a different
   authenticated body with the same sender/message ID marks a conflict.
   Tests cover wrong author/participants, modified MAC/body, truncation,
   padding, duplicate replay, and conflicting authenticated bodies.

5. **`schema`, `code`, `test` — Secret lifetime is independent and irreversible
   within live application state.**
   `LocalEpoch::generate` uses a new random native identity, not an account
   secret, and fixes seven-day sending and subsequent 28-day receiving
   deadlines. The authoritative installation/epoch tables preserve these
   assignments across restart. Every receive operation first durably removes
   expired keys under the database writer and then samples wall time again in
   its trial transaction. A newly expired key forces another purge rather than
   a trial; `LocalEpoch::identity` separately rejects expiry at that sampled
   time. The synchronous trial drops loaded secret/plaintext temporaries before
   the next async scheduling boundary. Backward clock correction cannot
   recreate deleted rows; future-dated newest epochs pause generation instead
   of rewriting their deadlines. Retirement preserves old receive-only epochs;
   explicit re-enrollment creates a new random device ID.
   Total migration stashes/restores installation, epochs, sticky device states,
   and plaintext history before replay. Derived indexes rebuild from these
   sources, not by reconstructing deleted secrets from ciphertext.
   Clock-race, all-live-key resumption, pending-stash reopen, retirement, and
   total-rebuild tests protect these paths.

6. **`assumption`, `code` — Later independent keys do not recover old ciphertext.**
   Given Lemma 5 and the independent-randomness assumption, later epoch or
   account signing keys provide no derivation path to old selected keys.
   Given native-age confidentiality and the explicit absence of *all*
   recovery-capable secret copies, archived public ciphertext remains protected.
   Live-row deletion alone does not establish the absence premise. Any selected
   sender-other key matters equally to a recipient key. A device that was off
   during its deletion deadline, an old database page, or a stolen key can
   invalidate the premise indefinitely.

7. **`code`, `test` — Receiving does not create a plaintext-dependent network
   response.**
   Event ingestion queues ciphertext without decrypting it inline. The owned
   worker processes at most eight retained identities per transaction and
   persists its exclusive cursor until all live keys have been tried; a first
   batch miss is not treated as proof that the message is irrelevant.
   It writes history or progress locally and emits no receipt, callback,
   remote fetch, or decryption-result notification to peers. Its idle
   notification comes from durable queue insertion, not decryption success.
   Startup drains existing work even without a fresh notification, and
   serialized successful full activation starts one owned worker. Tests cover
   pending work before activation, repeated/concurrent unlock, non-full clients,
   failed activation, idle wakeup, and joined shutdown. This is not a
   constant-time whole-host or traffic-analysis guarantee. Local trusted UI
   subscribers receive coalesced content-free wakeups after every serviced
   trial batch, independent of decryption outcome, including misses and invalid
   queue entries. These carry no result or identity to peers; the existing
   authenticated UI websocket rechecks exact-session full authority before
   forwarding them, and history reads independently enforce private HTTP access.
   Tests cover wakeups on both failed and successful trials and silence at idle.

8. **`code`, `test` — HTTP authorization belongs to the requesting session.**
   `MessageSession` retrieves the current session token, looks up only that
   session's in-memory secret, and checks matching full active-client authority.
   Handlers upgrade only that originally authorized runtime's weak handle and
   repeat the authority check on the strong reference used by the operation;
   eviction cannot substitute a newly loaded runtime for the same account.
   It does not accept another browser's shared account activation.
   POST handlers validate a separate random session synchronizer token before
   sending, retiring, or re-enrolling. They never use the actual session token
   as an HTML form value. Message text uses the posts' sanitized Djot renderer,
   so user-controlled raw HTML and dangerous URLs do not become executable
   content. Conversation pages share the application shell, navigation, and
   self-hosted rich-content runtime with ordinary pages.
   Their CSP permits same-origin scripts and fonts, Alpine's expression
   evaluator, and inline styles required by self-hosted MathJax, but no inline
   or remote script source; sanitized message content cannot supply styles.
   Browser drafts use distinct
   account-and-recipient local-storage keys. An AJAX send clears only when the
   response's submitted draft-instance token still matches the current persisted
   instance; edits rotate that token, so a delayed response does not clear newer
   text. The ordinary form remains a POST/303 workflow without JavaScript.
   Response middleware covers successful pages, AJAX responses, 303/308
   redirects, extractor errors, and missing routes with sensitive headers and a
   restrictive CSP. Integration tests exercise a public-only session while
   another session has the same account unlocked, cross-session token rejection,
   logout, account/recipient draft scoping, delayed-clear guards, escaped hostile
   text, failed sends preserving drafts, successful send/receive, and
   retirement/re-enrollment. Retirement uses an ordinary read-only confirmation
   page before the explicit POST; typed lifecycle forms reject missing or
   extraneous action fields before mutation.

Together these steps connect the explicitly assumed primitive/host guarantees
to account-bound accepted plaintext, qualified outsider envelope confidentiality,
conditional epoch protection, and session-local HTTP access.

## Residuals and weakest links

- This is account authentication. A holder of the account signing secret can
  announce malicious future keys, retire devices, and impersonate the account.
  Retirement does not repair account compromise.
- A selected file-key holder can rewrap/rewrite an age file and sign a new event
  as itself. Such active insiders are outside recipient-concealment scope.
  Known public device counts, sender-other membership, and spillover reveal
  predicates to insiders; public graph/timing behavior can identify recipients.
- Retained sent and received text is deliberately plaintext in local history,
  and unsent composer drafts persist in browser local storage. Database, browser,
  backup, or endpoint access can reveal those plaintext copies after epoch
  expiry.
  Zeroizing application temporaries is best-effort hygiene, not a statement
  about compiler copies, allocator pages, redb pages, swap, or forensic erasure.
- Sampled ordinary wall time is not trusted elapsed time. Clock jumps can erase
  keys early or pause generation; powered-off installations do no maintenance.
  No universal “unrecoverable after 35 days” property follows.
- The crypto premise is the weakest nonlocal step: tests establish framing and
  source integration, not computational security. Changes to age/age-core,
  stanza construction, parsing, or randomness require source reinspection and
  renewed independent verification, not just passing existing examples.
- Finite batches and bounded indexed views constrain individual operations,
  not all database growth, codec allocations, host scheduling, or adversarial
  overload. There is no unconditional delivery, fairness, or DoS-resistance
  claim. No groups, ratchet, post-quantum security, automatic history expiry, or
  new-device plaintext-history synchronization is implemented.
