# ADR 0021 — Distribución por GitHub Releases

- Estado: aceptada (2026-10-24), Fase 5.

## Contexto

Hasta la Fase 4 los binarios salían de los artefactos de CI, que caducan a los 90 días, exigen sesión en GitHub y no tienen nombre de versión. Los instaladores de la Fase 5 necesitan un origen estable y comprobable. El repositorio era privado: la API de GitHub y los assets de una release pedían autenticación.

## Decisión

- **Releases por etiqueta.** `release.yml` se dispara con una etiqueta `v*`. Construye los paquetes, comprueba que la etiqueta coincide con la versión de `Cargo.toml`, calcula `SHA256SUMS` y lo publica todo como release de GitHub (`gh release create --verify-tag`). Lanzado a mano (`workflow_dispatch`), solo construye y prueba, sin publicar. La etiqueta la pone el orquestador cuando todo está validado en hardware.
- **Contenido.** En la 0.5.0: `rawkvm-hub_<ver>_{arm64,armhf}.deb`, `rawkvm-agent-gadget_<ver>_{arm64,armhf}.deb`, `rawkvm-agent_<ver>_amd64.deb` (Fase 5.2) y `SHA256SUMS`. El tar de macOS y el zip de Windows se añaden en sus bloques (Fase 5.3 y 5.4).
- **Versión.** Una sola, la de `[workspace.package]` en `Cargo.toml`, igual en `widget/src-tauri/Cargo.toml`, `tauri.conf.json` y `package.json`. Pasa de 0.1.0 a **0.5.0** (Fase 5). `cargo-deb` y los nombres de los ficheros la toman de ahí. El protocolo tiene su propia versión (`PROTO_VERSION`): una release nueva no implica cambiarlo.
- **Repositorio público** (decisión del usuario, 2026-10-24), para que la descarga no necesite credenciales.
- **Descarga en los instaladores**, en este orden:
  1. `--from CARPETA`: ficheros ya descargados o compilados (desarrollo y despliegues del orquestador).
  2. Por defecto, `curl` anónimo a `releases/latest` (o a la etiqueta de `--version vX.Y.Z`). Si hay `GH_TOKEN`, se usa, para no chocar con el límite de peticiones anónimas a la API.
  3. `gh release download` como alternativa, si `gh` está instalado y autenticado.

  Siempre se comprueba el SHA-256 contra `SHA256SUMS` antes de instalar. `releases/latest` da 404 hasta la primera etiqueta (`v0.5.0`).
- **Documentación**: la vía principal es `curl` anónimo con comprobación de `SHA256SUMS`; `gh` y `scp`/`--from` quedan como alternativas.

## Revisión del historial antes de hacerlo público

Hecha por el orquestador antes de cambiar la visibilidad:

- Ningún token real del hub. Las cadenas de 64 hex fuera de `Cargo.lock` son de tests o de ejemplo.
- Ni claves ni certificados (`*.der`, `*.pem`, `*.p12`), ni patrones de claves de API.
- Ningún correo personal.
- **Quedan, y se aceptan**: direcciones de la LAN (`192.168.1.x`) y nombres de máquina en los docs.

Para revisiones futuras (antes de añadir fixtures, trazas o logs): `git log -p --all | grep -E '[0-9a-f]{64}'` y `gitleaks detect`. No subir ficheros `agent.toml`, `hub.toml`, `widget.toml` ni `nodes.toml` copiados de una máquina, `hub_fingerprint` reales, ni capturas `--dump`/`.hidrec` con texto tecleado.

## Consecuencias

- Hay una URL versionada para cada componente, y una suma para comprobarlo.
- Cualquier equipo con red descarga e instala sin cuenta de GitHub.
- Los artefactos de CI siguen existiendo para probar commits sin etiqueta.
