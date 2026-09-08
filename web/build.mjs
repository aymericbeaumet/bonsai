import { build } from "esbuild";
import { copyFile, mkdir, readFile } from "node:fs/promises";

await mkdir("dist", { recursive: true });
const notices = await Promise.all(
  ["@xterm/xterm", "@xterm/addon-fit"].map(
    async (name) =>
      `${name}\n${await readFile(`node_modules/${name}/LICENSE`, "utf8")}`,
  ),
);
await build({
  entryPoints: ["src/app.ts"],
  bundle: true,
  minify: true,
  outfile: "dist/app.js",
  target: ["es2022"],
  format: "esm",
  legalComments: "eof",
  banner: { js: `/*!\n${notices.join("\n\n")}\n*/` },
});
await copyFile("index.html", "dist/index.html");
