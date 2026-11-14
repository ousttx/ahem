// Writes latest.json, the file the app's updater reads, from the files of a release build.
// Usage: node tools/update-manifest.mjs <folder of the build's files> <version> [notes file]
// The folder holds, for each system, the package and its .sig file made by `tauri build` with
// createUpdaterArtifacts (see docs/ARCHITECTURE.md, Updates).
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const RELEASES = "https://github.com/ousttx/ahem/releases/download";

/// The packages the updater installs, by the name of the system in the manifest.
const PLATFORMS = {
  "darwin-aarch64": (name) => name.endsWith(".app.tar.gz"),
  "windows-x86_64": (name) => name.endsWith("-setup.exe"),
};

/// `files`: name → contents of the .sig file (for every package that was built). A system whose
/// package was not built is left out (its users find no update); one package at least is needed.
export function buildManifest({ version, notes, files, date }) {
  const platforms = {};
  for (const [platform, isPackage] of Object.entries(PLATFORMS)) {
    const packages = Object.keys(files).filter(isPackage);
    if (packages.length > 1) throw new Error(`${platform}: expected one package, found ${packages.length}`);
    if (packages.length === 0) continue;
    const [name] = packages;
    const signature = files[name]?.trim();
    if (!signature) throw new Error(`${platform}: no signature for ${name}`);
    platforms[platform] = { signature, url: `${RELEASES}/v${version}/${encodeURIComponent(name)}` };
  }
  if (Object.keys(platforms).length === 0) throw new Error("no package with a signature in the files");
  return { version, notes: notes ?? "", pub_date: date, platforms };
}

function main(folder, version, notesFile) {
  const files = {};
  for (const name of readdirSync(folder)) {
    if (name.endsWith(".sig")) continue;
    const signed = readdirSync(folder).includes(`${name}.sig`);
    if (signed) files[name] = readFileSync(join(folder, `${name}.sig`), "utf8");
  }
  const notes = notesFile ? readFileSync(notesFile, "utf8").trim() : "";
  const manifest = buildManifest({ version, notes, files, date: new Date().toISOString() });
  writeFileSync(join(folder, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(`latest.json for ${version}: ${Object.keys(manifest.platforms).join(", ")}`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const [folder, version, notesFile] = process.argv.slice(2);
  if (!folder || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version ?? "")) {
    console.error("Usage: node tools/update-manifest.mjs <folder> <version> [notes file]");
    process.exit(1);
  }
  main(folder, version, notesFile);
}
