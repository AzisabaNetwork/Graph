import { copyFile, readFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

interface TypeScriptConfig {
  compilerOptions: {
    lib?: string[];
    [option: string]: unknown;
  };
  [option: string]: unknown;
}

const sdkRoot = dirname(fileURLToPath(import.meta.url));
const generatedRoot = resolve(sdkRoot, "generated");

await copyFile(
  resolve(sdkRoot, "overrides/src/apis/StreamApi.ts"),
  resolve(generatedRoot, "src/apis/StreamApi.ts"),
);
await copyFile(
  resolve(sdkRoot, "overrides/docs/StreamApi.md"),
  resolve(generatedRoot, "docs/StreamApi.md"),
);

const tsconfigPath = resolve(generatedRoot, "tsconfig.json");
const tsconfig = JSON.parse(
  await readFile(tsconfigPath, "utf8"),
) as TypeScriptConfig;

tsconfig.compilerOptions.lib = ["ES2018", "DOM"];
await writeFile(tsconfigPath, `${JSON.stringify(tsconfig, null, 2)}\n`);
