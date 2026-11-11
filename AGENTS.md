# HealthyMac

## Notas heredadas de la memoria

> Pasadas desde `~/.claude/projects/-Users-mats-PROYECTOS/memory` el 08/11/2026 (F8 de JARVIS).

### healthymac-amigo

HealthyMac es un limpiador/monitor de macOS de un amigo de Mateo (fernandevdaza/HealthyMac, GPL-3.0, creado 2026-11-05). Clon en ~/PROYECTOS/HealthyMac con remotes `origin` = fork Mats2208/HealthyMac y `upstream` = el del amigo.

El front NO es web: GPUI 0.2.2 (framework de Zed, Rust sobre Metal), crate `crates/hm-app`, componentes propios y tokens en `src/theme.rs`. La app llama directo a los crates del back (sin API). Mateo mejora el front; el amigo hizo el back.

Estado 2026-11-06: el PR #1 se abrió y se cerró a pedido de Mateo; después se mandó el PR #2 (rama ui/pulido, 28 commits, todo junto) a fernandevdaza/HealthyMac, sin línea de Claude en la descripción. Ramas apiladas en el fork: `ux/mejoras-iniciales` (5 commits de UX) → `ui/rediseno` (tema neutro claro/oscuro, Phosphor, docs/DESIGN.md) → `ui/idiomas` (español/inglés con `t!`/`tf!` de hm_core::i18n en todos los crates) → `ui/pulido` (movimiento en ui/motion.rs, ícono en dos variantes claro/oscuro por scripts/make_icon.swift (Dock sigue al sistema en vivo, barra lateral al tema; .icns = claro), demo con íconos de apps instaladas, README con 28 capturas claro/oscuro × es/en, arreglos por datos reales: ~/PROYECTOS y ComfyUI no se encontraban, Puertos). Para capturas: HEALTHYMAC_LANG=es|en y HEALTHYMAC_THEME=light|dark. Capturas de PR en la rama huérfana `pr-assets`. El primer idioma de su Mac es en-BO, así que con "Sistema" la app abre en inglés.

Para comparar con la versión de Fernando: worktree desacoplado de upstream/main en el scratchpad de la sesión (se puede quitar con git worktree remove). Datos reales de Mateo (2026-11-06): M4 Pro 24 GB, 494 GB de disco, ~26 GB recuperables; probar con HEALTHYMAC_DRY_RUN=1 (simula). Al buscar la ventana de una instancia, filtrar por PID y elegir la ventana más grande (hay una auxiliar de 500×500).

**Why:** aportar sin romper su proyecto: todo por ramas del fork y PR para que él revise.
**How to apply:** probar con `HEALTHYMAC_DEMO=1 HEALTHYMAC_BACKGROUND=1` y manejar la app con `HEALTHYMAC_DRIVE=<archivo>` (clic/key/resize/scroll sin mover el puntero, ver [[no-robar-foco]]); capturar con `screencapture -l <windowid>`. Antes de PR: `cargo fmt --all`, `cargo clippy --workspace --all-targets`, `cargo test --workspace`. Commits según [[commits-sin-trailer-claude]]. El progreso se lleva en ~/.claude/todo.json (mod todo-panel).
