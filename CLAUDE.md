# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Read ./CONVENTIONS.md file.

This project uses the Linked Specs convention; consult the `linked-specs`
skill before working with specs or governed code.

## Project Overview

Rostra is a p2p (friend-to-friend) social network built in Rust. It uses a lightweight event DAG system where all data is propagated as signed Events, enabling decentralized social networking with sovereign identities and multi-device support.

## Architecture

- **Core principle**: All data propagates as cryptographically signed `Event`s forming a DAG structure
- **Network layer**: Uses Pkarr for distributed identity/naming and iroh-net for p2p transport
- **UI**: Web-based interface using Axum, Maud, Alpine, and alpine-ajax
- **Storage**: Local database for tracking events and content

## Project Structure

- `crates/rostra-core` - Core domain types used across the project
- `crates/rostra-client-db` - Database for tracking all events
- `crates/rostra-web-ui` - Default web-based UI (Axum + Maud + Alpine/alpine-ajax)
- `crates/rostra-client` - Client implementation (includes shared RPC utilities in `util::rpc`)
- `crates/rostra-p2p` - P2P networking layer
- `crates/rostra-p2p-api` - P2P API definitions
- `crates/rostra-util-*` - Various utility crates
- `crates/bots/` - Bot implementations

## Development Commands

### Building and Testing

```bash
# Build the project
cargo build
# or: just build

# Run tests
cargo nextest run
# or: just test

# Check code (faster than build)
cargo check
# or: just check
```

### Code Quality
```bash
# Run all pre-PR checks (lint + clippy + test)
just final-check

# Format code
just format  # runs cargo fmt + nixfmt

# Run lints (pre-commit hook)
just lint

# Run clippy
just clippy

# Fix clippy issues
just clippy-fix
```

### Running the Application

```bash
# Run web UI in production mode
cargo run --release web-ui

# Development mode with hot reload on port 2345
just dev

# Development on custom port
just dev 3000
```

### Testing Individual Components

```bash
# Test specific crate
cargo test -p rostra-core

# Test with logging
RUST_LOG=debug cargo test

# Run specific test
cargo test test_name
```

## Web UI Architecture

The web UI (`crates/rostra-web-ui`) uses:
- **Axum** for the web server framework
- **Maud** for HTML templating
- **Alpine + alpine-ajax** for progressive enhancement
- **Tower** middleware for sessions, cookies, compression
- Server-rendered HTML as the primary interaction architecture

Key web UI files:
- `src/routes/` - Route handlers for different pages
- `src/lib.rs` - Main application setup
- Routes include: timeline, post, profile, avatar, etc.

## Development Notes

- Uses Rust 2024 edition
- Workspace-based multi-crate structure
- Structured logging with `tracing`
- No inline `mod`s - use standalone modules
- Supports multi-device sync through event DAG merging

## Web UI Conventions

- Follow
  [`DESIGN-server-rendered-hypermedia`](crates/rostra-web-ui/specs/DESIGN-server-rendered-hypermedia.md):
  changed workflows must work through ordinary HTTP without JavaScript. Alpine
  is progressive enhancement; justify any new custom JavaScript in review.
- For keyboard shortcuts that trigger `requestSubmit()`, always use `keyup` (not `keydown`). `keydown` fires repeatedly with key auto-repeat, which can cause duplicate form submissions and race conditions in alpine-ajax.
- Keep credential-bearing pages server-rendered, session-scoped, unavailable to
  read-only sessions, and protected by the sensitive response headers in
  `routes/recovery.rs`. Recovery phrases use a masked read-only field and a
  conventional copy control; do not add reveal dialogs or confirmation steps.
