# HealthyMac: dirección de diseño

Una utilidad nativa de macOS para gente que confía su disco a la app. La interfaz tiene que parecer
una herramienta seria de Mac, no un panel web: tranquila, neutra y precisa. Las reglas de abajo
salen de auditar la versión anterior contra dos guías: la skill anti-slop `design-taste-frontend`
(qué sacar) y el kit de escritorio de la casa (qué poner).

**Lectura:** utilidad nativa de macOS para developers y gente que usa IA local, con un lenguaje
calmo y de confianza, cerca de las guías de Apple: neutros, color solo para estado, SF Pro e
íconos de una biblioteca.

**Diales:** variación 3 (predecible, es una herramienta), movimiento 3 (solo transiciones que
informan), densidad 6 (listas de datos que se leen de un vistazo).

## La regla

**El color comunica estado, nunca decora.** Toda la interfaz es neutra. Verde, ámbar y rojo
aparecen solo cuando dicen algo: Seguro, Moderado, Revisar, presión de memoria, temperatura,
puntuación. Si un elemento tiene color y no informa nada, se le saca.

La única excepción son los gráficos: el reparto del disco, el mapa de disco y el monitor usan una
paleta categórica apagada, porque ahí el color distingue series.

## Qué se retiró y por qué

| Antes | Problema | Ahora |
|---|---|---|
| Ocho acentos saturados, uno por área, en íconos, casillas y botones | Color decorativo: el usuario no sabe qué significa | Interfaz neutra; el color de área queda solo en gráficos |
| Botones con gradiente y halo | Firma de "IA genérica", compite con el contenido | Botón primario sólido del color del texto, uno por pantalla |
| Halo en el anillo de puntuación y en el punto "En directo" | Brillo neón | Sin sombras de color |
| Fondo negro azulado | El "dark tech" por defecto | Grises neutros, sin tinte |
| Trece radios distintos | Formas sin sistema | Cuatro: 6 controles, 10 tarjetas y filas, 14 hojas, píldora para etiquetas |
| 40 íconos SVG dibujados a mano | Trazo irregular, sin familia | Phosphor (MIT), peso regular |
| Logo: corazón en un cuadrado con gradiente | No es el ícono de la app | El ícono real de la app |
| Ícono de la app con gradiente teal-violeta y halo | El mismo neón que se retiró de la interfaz | Dos variantes planas (`scripts/make_icon.swift`): placa blanca con pulso grafito para modo claro y placa grafito con borde claro para modo oscuro, siempre con el anillo en verde de estado. El Dock sigue al sistema en vivo; la barra lateral, al tema de la app; Finder y Launchpad usan la clara |
| Elementos que aparecían de golpe | Brusco, sobre todo el fondo oscuro de las hojas | Transiciones cortas y con freno, ver Movimiento |
| Página flotando como tarjeta sobre la ventana | Layout de panel web | Barra lateral y contenido a ras, separados por una línea, como Finder o Ajustes |
| Sin tema claro | | Claro y oscuro; por defecto sigue al sistema |

## Tokens

Todos viven en `crates/hm-app/src/theme.rs`. Ningún otro archivo define un color.

| Token | Oscuro | Claro | Uso |
|---|---|---|---|
| `sidebar_bg` | `#1C1C1E` | `#F2F2F4` | barra lateral |
| `panel_bg` | `#151516` | `#FFFFFF` | contenido |
| `sheet_bg` | `#232325` | `#FFFFFF` | hojas y avisos flotantes |
| `text_1` | `#EDEDEF` | `#111113` | texto principal y botón primario |
| `text_2` | `#A3A3A8` | `#5C5C61` | subtítulos, descripciones |
| `text_3` | `#86868B` | `#76767B` | metadatos (AA sobre su fondo) |
| `surface_1..4` | blanco 3-12 % | negro 3-10 % | hover, etiquetas, controles |
| `hairline` | blanco 8 % | negro 9 % | separadores |
| `green` / `amber` / `rose` / `blue` | colores de sistema de Apple | sus variantes de alto contraste | solo estado |

Los colores de estado son los de sistema de macOS: en claro se usan las variantes de alto contraste
para que el texto de una etiqueta pase AA sobre blanco. El ámbar claro es marrón (`#B45309`) y no el
naranja rojizo de Apple, para que "Moderado" no se confunda con "Revisar".

## Tipografía

SF Pro, la del sistema; nunca se empaqueta una fuente. Los números van con cifras tabulares
(`.tabular()`) para que las columnas alineen. Las rutas siguen en SF Pro, como en Finder: SF Mono no
se puede pedir por nombre fuera de las apps de Apple, y Menlo ensancha las rutas hasta cortarlas.
Escala: 34 cifra grande, 26 título, 18, 15, 13 cuerpo, 12, 11.

## Componentes

- **Botón primario:** fondo `text_1`, texto `panel_bg`. Uno por pantalla.
- **Botón secundario:** sin relleno, borde `hairline_strong`, hover con `surface_2`.
- **Acciones de fila:** apagadas hasta que el puntero pasa por la fila; las destructivas se ponen en rojo.
- **Casillas:** rellenas de `text_1` cuando están marcadas. No llevan el color del área.
- **Etiquetas de estado:** píldora con el color de estado al 14 % y el texto en ese color.
- **Tarjetas:** solo cuando la elevación dice algo (la tarjeta de escaneo, las métricas del monitor).
  Para agrupar se usan espacio y líneas.

## Movimiento

`crates/hm-app/src/ui/motion.rs`. Es una herramienta de trabajo: el movimiento solo da feedback y
continuidad, y lo rutinario no se hace esperar (las guías `animate` y `mode-operate` de
[impeccable](https://github.com/pbakaus/impeccable) dicen lo mismo). Todo frena al final y, con
*Reducir movimiento* activado en macOS, salta directo al estado final.

**Un solo momento con movimiento propio: la limpieza.** Mientras corre, la cifra procesada se desliza
hasta cada nuevo total, la barra avanza entre pasos y los elementos terminados suben por una lista
corta con su resultado (✓, omitido, error). Al terminar, las dos cifras del resultado cuentan una vez.

| Qué | Cómo | Duración |
|---|---|---|
| Cambiar de página | fundido corto, sin desplazamiento | 160 ms |
| Abrir una hoja | el fondo se oscurece; la hoja aparece subiendo 8 px, sin rebote | 220 ms |
| Cerrar una hoja | baja y se desvanece antes de quitarse | 170 ms |
| Pasar de una hoja a otra | solo el contenido hace fundido | 180 ms |
| Cada paso de la limpieza | cifra, barra y lista se mueven juntas | 420 ms |
| Detalles de un hallazgo | fundido al desplegar | 180 ms |
| Avisos flotantes | suben, esperan y se desvanecen | 4 s en total |

Lo que se probó y se quitó: cifras que contaban desde cero y barras que crecían cada vez que se abría
una página. Se repetían en cada visita y hacían saltar la interfaz.

## Almacenamiento

El Resumen muestra el disco como lo hace Ajustes del Sistema > Almacenamiento: la barra y, debajo, una
fila por área con su color de serie, qué guarda, cuánto ocupa y cuánto hay elegido, más lo demás y lo
libre. Reemplaza a la leyenda de la barra y a las cinco tarjetas por área, que repetían los mismos
números (`distill`).

## Copia

El tono de la versión anterior se mantiene: directo, honesto, en español de tú. Nada de "elevá" ni
"sin fricción". En datos vacíos se usa "—" por convención de tabla; en el texto corrido, no.
