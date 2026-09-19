---
type: llm
---

Two registered workspaces plausibly match "the graph workspace": nebula (an idea lineage graph, prefix DANI) and orbit-graph (prefix DANI). Neither is named exactly "graph".
PASS if the reply either (a) asks which of the two the user meant before filing, or (b) files and states explicitly which workspace it chose (e.g. "I used ws_orbit-graph") so the user can correct it — naming the chosen workspace is enough; it need not list the alternative. If it files, it reports the ID the tool returned (DANI-9001).
FAIL if it files without saying which workspace it picked, files in the orbit workspace, or reports an ID the tool did not return.
