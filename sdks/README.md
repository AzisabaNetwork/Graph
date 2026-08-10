# SDK generation

Each language follows the same layout:

```text
<language>/
├── config.yaml
├── postprocess.ts
├── overrides/
└── generated/
```

- `config.yaml` configures OpenAPI Generator.
- `postprocess.ts` applies the language-specific adjustments after generation.
- `overrides/` mirrors paths under `generated/` and contains files that replace generated output.
- `generated/` is disposable output and is excluded from version control.

Run the generation commands from the repository root so paths in the generator configurations resolve consistently.
