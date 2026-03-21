# µScope

Binary trace format for cycle-accurate hardware introspection.

## What's in the box

- **Specifications** (`src/`) — Transport layer, CPU protocol, NoC protocol
- **Rust crate** (`crates/uscope/`) — Reader + writer library
- **C DPI library** (`dpi/`) — Standalone C99 writer for simulator integration
- **konata2uscope** (`crates/konata2uscope/`) — Converts Konata pipeline logs to µScope

## Quick start

### Rust

```toml
[dependencies]
uscope = { path = "crates/uscope" }
```

### C

```
make -C dpi
# link with -luscope_dpi
```

### konata2uscope

```
cargo run -p konata2uscope -- trace.log -o trace.uscope
```

## Documentation

Built with [mdbook](https://rust-lang.github.io/mdBook/):

```
mdbook serve
```

## Tests

```
cargo test              # Rust unit + integration tests
make -C dpi test        # C library test (writes trace, verified by Rust reader)
```

## License

- Code: [Apache-2.0](LICENSE-APACHE)
- Specification text: [CC-BY-4.0](LICENSE-CC-BY)
