<div>
  <h1>Azisaba Graph</h1>
  <p>An integrated API platform for connecting and sharing data across the Azisaba Network.</p>
  <p>
    <a href="https://github.com/AzisabaNetwork/Graph/actions/workflows/openapi-lint.yaml"><img alt="OpenAPI lint" src="https://img.shields.io/github/actions/workflow/status/AzisabaNetwork/Graph/openapi-lint.yaml?branch=main&amp;style=flat-square&amp;label=OpenAPI%20lint&amp;logo=openapiinitiative&amp;logoColor=white"></a>
    <a href="https://github.com/AzisabaNetwork/Graph/actions/workflows/publish-container.yaml"><img alt="Container" src="https://img.shields.io/github/actions/workflow/status/AzisabaNetwork/Graph/publish-container.yaml?branch=main&amp;style=flat-square&amp;label=container&amp;logo=docker&amp;logoColor=white"></a>
    <a href="https://spec.openapis.org/oas/v3.1.0"><img alt="OpenAPI 3.1" src="https://img.shields.io/badge/OpenAPI-3.1-6BA539?style=flat-square&amp;logo=openapiinitiative&amp;logoColor=white"></a>
    <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/github/license/AzisabaNetwork/Graph?style=flat-square"></a>
  </p>
</div>

> [!IMPORTANT]
> Graph is under active development. The API and generated SDKs may change before a stable release.

## About

Graph exposes shared Azisaba Network data through a scope-protected HTTP API and a Server-Sent Events stream. The [OpenAPI specification](openapi/openapi.yaml) is the source of truth for available resources, operations, schemas, authentication, and event types.

The server is implemented in Rust with Axum. It uses MariaDB for persistent data, Redis Pub/Sub for event delivery across instances, and optional S3-compatible storage for uploaded files.

## API documentation

The specification declares `https://graph.azisaba.net` as the production server. Requests use Bearer API-key authentication as documented in OpenAPI.

To browse the current specification with Swagger UI:

```bash
docker compose -f openapi/compose.yaml up
```

Open [http://localhost:8080](http://localhost:8080).

## SDKs

Client SDKs are generated from the OpenAPI specification with [OpenAPI Generator](https://openapi-generator.tech/). The configured publishing targets are:

| Language | Package | Package repository and documentation |
| --- | --- | --- |
| TypeScript | `@azisaba/graph` | [npm](https://www.npmjs.com/package/@azisaba/graph) |
| Java | `net.azisaba.graph:graph` | [Azisaba Repository](https://repo.azisaba.net/) |
| Rust | `azisaba-graph` | [crates.io](https://crates.io/crates/azisaba-graph) · [docs.rs](https://docs.rs/azisaba-graph) |

Generate SDKs locally with:

```bash
pnpm generate:typescript-sdk
pnpm generate:java-sdk
pnpm generate:rust-sdk
```

Generated files are written to ignored `generated/` directories under [`sdks/`](sdks/README.md). Each language keeps its generator configuration, post-processing script, and overrides together in its SDK directory.

The generated `StreamApi` is replaced during generation with the SSE adapters under `overrides/`. The override paths mirror the generated package layout and reuse the generated configuration, authentication, and `StreamEvent` models.

## Development

Install the pinned pnpm dependencies, validate the API definition, and generate the Rust server contract:

```bash
pnpm install --frozen-lockfile
pnpm openapi:lint
pnpm generate:server
```

Then check and test the server:

```bash
cd server
cargo check -p graph-server
cargo test -p graph-server
```

The development stack is defined in [`server/compose.yaml`](server/compose.yaml). It provides the primary MariaDB database and Redis; a compatible punishments database must be supplied separately:

```bash
PUNISHMENTS_DATABASE_URL='<mariadb-url-reachable-from-docker>' \
  docker compose -f server/compose.yaml up
```

Runtime configuration is kept close to the implementation in [`server/app/src/main.rs`](server/app/src/main.rs) and the Compose file, reducing duplication in this README.

## Contributing

Update the OpenAPI contract first, regenerate the server interface, implement the generated traits, and run the validation and test commands above.

## License

Azisaba Graph is available under the [MIT License](LICENSE).
