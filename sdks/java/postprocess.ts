import { copyFile, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const sdkRoot = dirname(fileURLToPath(import.meta.url));
const generatedRoot = resolve(sdkRoot, "generated");

await copyFile(
  resolve(sdkRoot, "overrides/src/main/java/net/azisaba/graph/api/StreamApi.java"),
  resolve(generatedRoot, "src/main/java/net/azisaba/graph/api/StreamApi.java"),
);
await copyFile(
  resolve(sdkRoot, "overrides/docs/StreamApi.md"),
  resolve(generatedRoot, "docs/StreamApi.md"),
);
await rm(
  resolve(generatedRoot, "src/test/java/net/azisaba/graph/api/StreamApiTest.java"),
  { force: true },
);
