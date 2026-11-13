<!--
Borrador de carta para EMSE, número especial PROMPT-SE 2026 (2026-11-06).
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

- an amortization condition that prices an injected artifact by the rounds it must
  save, since its token cost recurs on every round of the trajectory;
- a measurement procedure built on a same-prompt control, an arm byte-identical to
  the baseline, which in our data dissolved an apparently significant gain
  (McNemar p = 0.011) into an unattributable one (p = 0.541);
- a pre-registered null result and its pre-registered replication on a second
  executor, where the same artifact becomes harmful.

This matches three of the call's topics: empirical evaluations of prompting
strategies for SE tasks, metrics for prompt cost and reproducibility, and replication
studies with negative or non-significant results.

**Replicability.** As the call requests, the manuscript reports the injected
artifacts and the execution settings (context and output limits, temperature, fixed
seeds). All per-run records, hypothesis documents, and the harness are public in the
repository and archived on Zenodo (doi:10.5281/zenodo.22547684).

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
