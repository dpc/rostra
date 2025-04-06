# Engineering conventions

## Conventions

> When writing code, follow this conventions


- Try to keep the code uniform, and follow the style of the existing code.
- Always use standalone Rust modules, avoid inline `mod`s

## Project structure

- `crates/` - all the project modules/crates
  - `crates/rostra-core` core domain types used all across the project
  - `crates/rostra-client-db` database tracking all the events
  - `crates/rostra-web-ui` the default web-based UI
  
