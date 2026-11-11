# HealthyMac: arquitectura

Limpiador y monitor para macOS, escrito íntegramente en Rust, con funciones para developers y para
quien usa IA local. UI en GPUI.

## Crates

| Crate | Qué hace |
|---|---|
| `hm-core` | Contrato común: `Finding`, `Scanner`, `ScanContext`, `SafetyPolicy`, `Cleaner`. Único código que borra. |
| `hm-system` | Basura de sistema, apps y restos, arranque, mapa de disco, monitor, puntuación de salud. |
| `hm-dev` | Xcode, artefactos de proyectos, cachés de paquetes, Docker, toolchains, puertos. |
| `hm-ai` | Inventario de modelos, duplicados, metadatos GGUF, encaje en memoria, estado de runtimes. |
| `hm-cli` | Binario `healthymac`: el motor sin UI. |
| `hm-app` | Binario `HealthyMac`: la UI en GPUI. |

`hm-system`, `hm-dev` y `hm-ai` solo dependen de `hm-core`, nunca entre sí.

## Reglas que no se negocian

1. **Los escáneres son de solo lectura.** No crean, modifican ni borran nada, y los comandos que
   ejecutan no tienen efectos secundarios (`docker system df` sí, `docker system prune` no). Un
   escáner *describe* una acción con `Action`, y solo `hm_core::Cleaner` la ejecuta.
2. **Un escáner nunca borra por su cuenta.** Ni `fs::remove_*`, ni `trash`, ni `rename` fuera de
   `hm-core`. La excepción acotada es la caché propia de HealthyMac en
   `~/Library/Caches/HealthyMac`.
3. **La Papelera es el modo por defecto.** El borrado permanente solo se aplica a hallazgos `Safe`
   que se regeneran solos, y solo si el usuario lo activa.
4. **Lo que no se regenera no va preseleccionado.** `Risk::Review` implica `selected_by_default =
   false`. Ante la duda, `Risk` más alto y sin preseleccionar.
5. **Las rutas salen de `ctx.home`**, nunca de `$HOME` ni de `dirs`, para poder probar con un home
   de fixture.
6. **Sin prompts de permisos a mitad de un escaneo.** `~/Documents`, `~/Desktop`, `~/Downloads`,
   `~/Library/Containers/*` y `~/Library/Group Containers/*` solo se tocan si
   `fsutil::has_full_disk_access(&ctx.home)` es cierto o si el usuario los configuró como raíz. Si
   no hay acceso, el escáner emite un único hallazgo `ReportOnly` explicando qué ganaría el usuario
   al concederlo. Un directorio sin permiso se trata como vacío (`SizeInfo::unreadable`), nunca como
   error fatal.
7. **El PATH de una app lanzada desde Finder es mínimo.** Las herramientas se localizan con
   `ctx.which("brew")`, no asumiendo el PATH.
8. **Textos de interfaz en español e inglés.** Cada texto se escribe en español y, al lado, en
   inglés: `t!("Papelera", "Trash")` da un `&'static str` y `tf!("{n} elementos", "{n} items")`
   formatea. Vale también para los escáneres, porque sus títulos y explicaciones los ve el usuario.
   Los tests corren en español, que es el idioma por defecto. Identificadores y comentarios en
   inglés.

## Modelo

Un `Scanner` produce `Finding`s. Cada `Finding` declara cuánto ocupa, qué riesgo tiene, cómo se
recupera y qué `Action` lo limpia. La UI los muestra y el usuario aprueba. `Cleaner` revalida cada
ruta contra `SafetyPolicy` justo antes de actuar y deja constancia en
`~/Library/Logs/HealthyMac/history.jsonl`.

`SafetyPolicy` no confía en los escáneres: un escáner con un bug no debe poder dañar datos. Protege
credenciales, datos personales y carpetas sincronizadas con la nube a cualquier profundidad, y
resuelve symlinks en la ruta padre.

`Finding.size` es lo que limpiar liberaría de verdad, no lo que ocupa. `SizeInfo` separa `bytes`
(lo que ocupa) de `shared` (lo compartido con algo de fuera mediante clones APFS o enlaces duros), y
`SizeInfo::exclusive()` da la diferencia. Es un mínimo: eliminar todos los clones de un bloque libera
más que la suma de sus partes exclusivas, por eso la UI dice "al menos".

Las guardas de permisos (regla 6) solo se pueden validar lanzando la app con `open`. Desde una
terminal o desde Claude Code, macOS atribuye el acceso a ese proceso y no muestra ningún diálogo, así
que los tests y las pruebas por consola no los detectan. `~/Library/iTunes` se descubrió así.

`Action::CloneDedupe` sustituye copias idénticas por clones APFS: todas las rutas siguen siendo
válidas y solo se liberan los bloques duplicados. Es la forma de ahorrar espacio con modelos
repetidos entre runtimes sin romper ninguno.

## Compilación

- `runtime_shaders` de GPUI evita necesitar el Metal Toolchain de Xcode.
- Las dependencias se optimizan también en dev, porque GPUI sin optimizar va muy lento.
