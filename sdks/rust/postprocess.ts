import { copyFile, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const sdkRoot = dirname(fileURLToPath(import.meta.url));
const generatedRoot = resolve(sdkRoot, "generated");

await copyFile(
  resolve(sdkRoot, "overrides/src/apis/stream_api.rs"),
  resolve(generatedRoot, "src/apis/stream_api.rs"),
);
await copyFile(
  resolve(sdkRoot, "overrides/docs/StreamApi.md"),
  resolve(generatedRoot, "docs/StreamApi.md"),
);

const cargoTomlPath = resolve(generatedRoot, "Cargo.toml");
const cargoToml = await readFile(cargoTomlPath, "utf8");
const dependencySection = "[dependencies]\n";
const streamDependencies = [
  'async-stream = "^0.3"',
  'futures-core = "^0.3"',
  'futures-util = "^0.3"',
].join("\n");

if (!cargoToml.includes(dependencySection)) {
  throw new Error("Could not find the generated SDK dependency section.");
}

await writeFile(
  cargoTomlPath,
  cargoToml.replace(
    dependencySection,
    `${dependencySection}${streamDependencies}\n`,
  ),
);
