# PolicyVisor Documentation

Use **PolicyVisor** for the product name, **pVisor** for its short name, and
`pvisor` for the CLI. The positioning is **Policy-governed, reviewable execution.**
In Chinese, use **策略约束下的可审查执行。** The product covers Agent CLIs,
scripts, and automation commands; describe Agent-specific integrations as such.

Use `pvisor` for the Python distribution, import package, and wheel filename
prefix. Keep Rust crate names, `PERSISTING_*` environment variables, and existing
repository/deployment URLs accurate until they are migrated. Separate requested policy, installed controls, and observed
results; describe filesystem staging as opt-in and platform-dependent.

The site uses Zensical 0.0.61. English and Chinese Markdown live under
`docs/src/en/` and `docs/src/zh/`, with matching relative paths.

```bash
just docs-serve         # build, watch, and serve on 127.0.0.1:3000
just docs-serve --port 3001
just docs-build         # build docs/site and validate generated pages
```

`scripts/build-docs.py` renders the English configuration, then renders Chinese
pages with Zensical's native Chinese theme and a translated navigation tree.
Both use `docs/zensical.toml` as the navigation source. The language selector
uses relative links to the corresponding article, so local preview and the
GitHub Pages `/Persisting/` deployment both work. Keep locale paths paired;
`check-docs.py` checks their links, navigation and HTML language attributes.

`docs/overrides/home.html` overrides the native content block for the full-width
homepage. The header, mobile drawer, sidebars and table of contents remain
Zensical components. `docs/src/stylesheets/extra.css` supplies the shared blue
gradient, grid, brand contrast and homepage layout. Use native Markdown fences,
`!!! note` / `!!! tip` callouts, and relative image paths.

CI uses the same bilingual build and page checks before uploading `docs/site`.

## Information architecture

Both locales use the same six directories:

- `start/`: product scope, installation and the first reproducible Run.
- `guides/`: tasks, commands, expected results and troubleshooting.
- `concepts/`: terminology, evidence and capability limits.
- `reference/`: CLI options and concrete cases.
- `development/`: contributor setup, validation, release and roadmap.
- `design/`: implementation ownership, mechanisms and explicitly labeled future designs.

Keep one canonical article per subject and link to it instead of repeating its
contract. Start task guides with prerequisites and executable examples. Separate
implemented behavior from design goals; describe a guarantee only with its
executor and scope. Every workspace-review example must enable OverlayFS
explicitly, normally with `--stage` outside the project.

`zensical.toml` owns navigation. Add or move both language versions together.
`redirects.json` maps old locale-relative Markdown paths to their replacements;
the build emits redirects for English, Chinese, and the original unprefixed
published URLs. Update incoming source links to canonical paths as well.
The checker rejects unpaired pages, omitted or duplicate navigation entries,
broken links, missing anchors and invalid redirect targets.

## Design proposals

- [pVisor core algebra (Chinese)](pvisor-algebra.md): a small Haskell-style definition
  of operations, composition and rewrites, with concrete backend obligations and
  Event observations. Includes conditional laws and
  [finite model checks](pvisor-algebra-check.py). This is the semantic starting point
  for a future core rewrite, not a statement of current implementation guarantees.
- [Event v2 contract (Chinese)](event-contract-v2.md): proposed observations of
  interface operations and backend execution, with shared identity, links,
  collection and commit contracts; follows the core semantics and is not yet implemented.
