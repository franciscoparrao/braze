<!--
Borrador de carta para EMSE, número especial PROMPT-SE 2026 (2026-11-06;
contribuciones alineadas con la historia de la sección B el 2026-11-13;
Estudio 2 con la corrida intercalada el 2026-11-26).
Revisar con el autor antes de enviar. Pendientes marcados con [ ].
- [ ] ID del manuscrito del Paper 1 en EMSE (Editorial Manager).
- [ ] Confirmar que la memoria como "instruction artifact" es el encuadre que el autor quiere sostener.
Hechos verificados para esta carta:
- Solapamiento textual Paper 1 / Paper 2: 2 % de 8-gramas del PDF del Paper 2, todo en
  filiación, referencias compartidas y la autocita (medido 2026-11-06).
- Ajustes de ejecución reportados en el manuscrito: contexto 32.768, salida 12.288,
  temperatura 0,2, semillas fijas (§ Method). Datos: Zenodo 10.5281/zenodo.22547684 y repo público.
-->

Dear Guest Editors of the PROMPT-SE 2026 special issue,

Please consider the enclosed manuscript, "When Procedural Memory Does Not Pay: The
Amortization Frontier of Prompt-Injected Memory for Local-First Coding Agents", for
the Empirical Software Engineering special issue on Empirical Studies on Prompts and
other Instruction Artifacts in SE.

**Fit with the special issue.** The paper treats a prompt-injected artifact as its
object of study. Coding-agent harnesses increasingly carry distilled "playbooks" or
project memory into every session. That text is an instruction artifact in the sense
of the call: the harness re-sends it to the model on every round, alongside the
system prompt. We measure how injecting it changes correctness, reliability, and
cost, which are the outcomes the call names. The paper contributes:

- an amortization condition, offered as a reporting convention, that splits an injected
  artifact's net token balance into the rounds it saves and the overhead it adds on every
  round of the trajectory;
- a same-prompt (A/A) control arm and interleaved arms, measured: with the arms in
  sequence, the control rose nearly as much as the treatment (+9 vs. +13 of 102 cells) and
  exposed an arm-level throughput artifact behind an apparently significant gain (McNemar
  p = 0.011); a pre-registered re-run with the arms interleaved removed the artifact and
  found no effect of project memory (-7 cells, Holm p = 0.59);
- a pre-registered replication on a second executor, where the same artifact lengthens
  trajectories, adds 2.7k-6.6k net input tokens per task, and lowers the registered pass
  rate on one task of three, reported with every departure from the registered plans.

This matches three of the call's topics: empirical evaluations of prompting
strategies for SE tasks, metrics for prompt cost and reproducibility, and replication
studies with negative or non-significant results.

**Replicability.** As the call requests, the manuscript reports the injected
artifacts and the execution settings (context and output limits, temperature, fixed
seeds). All per-run records, hypothesis documents, the harness, and a script that
regenerates every reported statistic are public in the repository and archived on
Zenodo (doi:10.5281/zenodo.22547684).

**Related submission.** A separate manuscript by the same author is under review in
the regular track of Empirical Software Engineering: "Not All Scaffolding Helps: A
Lever-by-Lever Study of Agentic Harnesses at Small-Model Scales" [manuscript ID]. Both
use the same open-source harness, but they answer different questions with different
experiments and data. That paper measures how reliability levers (a lead model, plan
delivery, tool deferral) behave across executor scale. This one measures the cost and
benefit of one instruction artifact, prompt-injected memory, under a pre-registered
design. The two manuscripts share no results and no text beyond common references.
The present paper cites the other as "under review", and we can provide it to the
reviewers if useful.

The manuscript is original, is not under consideration elsewhere, and has no
competing interests. Its use of generative AI tools is declared in the manuscript.

Sincerely,

Francisco Parra
Departamento de Ingeniería Informática, Universidad de Santiago de Chile
francisco.parra.o@usach.cl
