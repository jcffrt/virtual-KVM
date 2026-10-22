# ADR 0001 — El workspace Cargo es la raíz del repositorio

- Estado: aceptada (2026-10-22)

## Contexto

El handoff dibuja el árbol dentro de un directorio `rawkvm/`. El repositorio ya se llama `virtual-KVM` y no contiene nada más.

## Decisión

El `Cargo.toml` del workspace vive en la raíz. Los crates se publican con prefijo (`rawkvm-proto`, `rawkvm-hub`, `rawkvm-agent-core`, `rawkvm-agent-linux`) y se importan con nombres cortos (`proto`, `agent_core`). El hub es librería + binario (`rawkvm_hub`), para que router, hotkey y servidor se puedan testear desde otros crates y en cualquier SO.

## Consecuencias

- Rutas más cortas en CI y en la documentación (`crates/…`, `deploy/…`).
- Los directorios no Rust (`drivers/`, `widget/`, `gnome-extension/`) también cuelgan de la raíz.
- Compilación cruzada sin Docker: `cargo zigbuild --release --target aarch64-unknown-linux-musl -p rawkvm-hub`.
