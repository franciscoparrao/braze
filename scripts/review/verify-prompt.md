You are checking ONE criticism that a peer reviewer wrote about a manuscript. You did not write it, and you must not take it on trust: decide whether the manuscript actually supports it.

The criticism:

{ISSUE}

The manuscript lines it cites (each prefixed with its line number as `L123|`):

{LINES}

The full manuscript is the file `manuscript.tex` ({N} lines) in your working directory. If the criticism says something is MISSING or ABSENT from the paper (no confidence intervals, no equation, no power analysis, not discussed...), you MUST search the manuscript with the grep tool before deciding, because the cited lines alone cannot show an absence. Use read_file only to read around a grep hit.

If the criticism says two parts of the paper CONTRADICT each other, or that the paper misinterprets a result, do not accept the reviewer's reading: grep for the key terms of both statements and read how the paper itself connects them. A result the paper reports and then explains (for example, an effect that a control experiment shows to be noise) is not a contradiction.

Decide:
- SUPPORTED: the manuscript text shows the problem the criticism describes.
- PARTIAL: the problem is real but overstated, or only part of it holds.
- UNSUPPORTED: the manuscript contradicts the criticism (for example, the "missing" item is present), or the cited lines do not say what the criticism claims.

Answer in exactly this format, and nothing else:

VERDICT: SUPPORTED | PARTIAL | UNSUPPORTED
EVIDENCE: L123 `an exact phrase from the manuscript that decides it`
REASON: one sentence.
