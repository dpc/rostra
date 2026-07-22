# Rostra: Frequently Asked Questions

Use the [Rostra Discussions Board](https://github.com/dpc/rostra/discussions)
to ask questions, and we'll add the answers here over time.

You can also reach out on [Matrix `#support:dpc.pw`](https://matrix.to/#/#support:dpc.pw),
[Discord](https://discord.gg/zens2jjA3U), or [dpc's Rostra profile](https://rostra.me/profile/rse1okfyp4yj75i6riwbz86mpmbgna3f7qr66aj1njceqoigjabegy).

## Does deleting a post erase every copy?

No. Rostra deletion is signed metadata that asks compliant clients to stop
showing the content and permits their locally stored bytes to be garbage
collected when no longer referenced. It cannot force peers that already
replicated the content—especially malicious or independently operated
replicas—to erase their copies. Even locally, deletion makes bytes eligible for
later garbage collection; it is not immediate secure erasure.
