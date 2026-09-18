import { readdir, stat } from "node:fs/promises";
import { join } from "node:path";

export const BUILD_COMMAND = "cargo build --release --target-dir target";

export async function ensureCurrentBinary(root, binary) {
    let built;
    try {
        built = await stat(binary);
    } catch (error) {
        if (error.code === "ENOENT") throw new Error(`Build thursday-agent first: ${BUILD_COMMAND} (in this folder).`);
        throw error;
    }
    const sources = (await readdir(join(root, "src"), { recursive: true }))
        .filter(path => path.endsWith(".rs")).map(path => join("src", path));
    for (const path of ["Cargo.toml", "Cargo.lock", ...sources]) {
        const source = await stat(join(root, path));
        if (source.isFile() && source.mtimeMs > built.mtimeMs) {
            throw new Error(`The thursday-agent binary is older than ${path}. Run ${BUILD_COMMAND} in this folder, then start thursday-agent again.`);
        }
    }
}
