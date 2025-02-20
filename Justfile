build:
    cargo build
    cargo clippy

clean:
    cargo clean

install:
    cargo install --path .

fmt:
    cargo fmt
