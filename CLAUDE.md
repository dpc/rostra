# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Read ./CONVENTIONS.md file.

## Project Overview

Rostra is a p2p (friend-to-friend) social network built in Rust. It uses a lightweight event DAG system where all data is propagated as signed Events, enabling decentralized social networking with sovereign identities and multi-device support.

## Architecture

- **Core principle**: All data propagates as cryptographically signed `Event`s forming a DAG structure
- **Network layer**: Uses Pkarr for distributed identity/naming and iroh-net for p2p transport
- **UI**: Web-based interface using Axum, htmx, and Maud for server-side rendering
- **Storage**: Local database for tracking events and content

## Project Structure

- `crates/rostra-core` - Core domain types used across the project
- `crates/rostra-client-db` - Database for tracking all events  
- `crates/rostra-web-ui` - Default web-based UI (Axum + htmx)
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
cargo test
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
- **htmx** for dynamic frontend interactions
- **Tower** middleware for sessions, cookies, compression
- Server-side rendering with htmx for interactivity

Key web UI files:
- `src/routes/` - Route handlers for different pages
- `src/lib.rs` - Main application setup
- Routes include: timeline, post, profile, avatar, etc.

## Development Notes

- Uses Rust 2024 edition
- Workspace-based multi-crate structure
- Structured logging with `tracing`
- No inline `mod`s - use standalone modules
- Event-driven architecture with cryptographic verification
- Supports multi-device sync through event DAG merging
