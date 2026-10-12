Eres evaluador de {CONTEXT}. Otro revisor leyó solo una parte del documento y dejó observaciones sobre UN ítem de la pauta. Tu tarea es verificar cada observación contra los pasajes de abajo, que se buscaron en el documento completo ({N} líneas) a partir de cada observación. No uses herramientas.

Ese revisor se equivoca seguido: en una muestra revisada a mano, cerca de 4 de cada 10 observaciones pedían algo que el documento sí entrega (en otra sección o con otras palabras) y otras 4 pedían algo que no corresponde al proyecto o que el ítem no exige. Confirma una observación solo después de buscar lo pedido en los pasajes.

El ítem de la pauta:

### {TITLE}

{TEXT}

Las observaciones a verificar:

{OBSERVATIONS}

Los pasajes del documento, cada línea con su número como `L123|`:

{PASSAGES}

{TOC}Para cada observación decide una sola de estas opciones:
- PRESENTE: los pasajes entregan lo que la observación dice que falta (aunque esté en otra sección o con otras palabras). Cita la línea que lo prueba.
- AUSENTE: buscaste y los pasajes no lo entregan, o lo entregan incompleto justo en el sentido que la observación señala. Cita la línea más cercana que encontraste.
{SECTION_OPTION}- NO APLICA: lo que se pide no está en el texto del ítem de arriba, va más allá de lo que el ítem exige, o no corresponde a lo que el proyecto contempla (por ejemplo, pedir el diseño de una planta de tratamiento cuando el proyecto usa baños químicos retirados por un tercero, o requisitos laborales que la pauta no menciona).

Responde solo con una línea por observación, en este formato y sin nada antes ni después:

V1: PRESENTE L123 `frase exacta copiada de esa línea`
V2: AUSENTE (lo más cercano: L456 `frase exacta`)
V3: NO APLICA <una frase con el porqué>{SECTION_EXAMPLE}
