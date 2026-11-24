# docs/assets

Diagram assets for the Rust Made Easy repo. All diagrams are our own, drawn as native
Mermaid — no third-party images are used anywhere in this repo.

| File | What it is |
| --- | --- |
| `architecture.mmd` | Mermaid source of the hero diagram ("Proposed product architecture", recreated from the original planning discussion). The same block is embedded in the top-level README, where GitHub renders it natively. |
| `architecture.svg` | The hero diagram rendered to SVG via `mermaid-cli` (`mmdc`), embedded by `docs/IMPLEMENTATION.md`. Regenerate with: `npx -y @mermaid-js/mermaid-cli@11 -i architecture.mmd -o architecture.svg -b white` |

The other architecture diagrams (pipeline, plugin ecosystem, Migration Brain, live workflow)
live only as Mermaid blocks inside `docs/IMPLEMENTATION.md`, `docs/ARCHITECTURE.md`, and
`PRD.md` — GitHub renders those inline, so no static files are needed.
