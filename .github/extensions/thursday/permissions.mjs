import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { basename } from "node:path";

const execute = promisify(execFile);
const DEFAULT_COMMAND = ["npx", "-y", "@steipete/peekaboo"];
const PERMISSIONS = ["Screen Recording", "Accessibility", "Event Synthesizing"];

export async function processContext(pid = process.pid, exec = execute) {
    const ancestors = [];
    const visited = new Set();
    let hostApp = null;
    while (pid > 1) {
        if (visited.has(pid) || ancestors.length >= 32) throw new Error("Cannot resolve the current process ancestry.");
        visited.add(pid);
        let stdout;
        try {
            ({ stdout } = await exec("/bin/ps", ["-ww", "-p", String(pid), "-o", "pid=,ppid=,comm="], {
                timeout: 2000, maxBuffer: 16_384,
            }));
        } catch {
            throw new Error(`Cannot inspect process ${pid}. Refresh the permission check.`);
        }
        const match = stdout.trim().match(/^(\d+)\s+(\d+)\s+(.+)$/);
        if (!match || Number(match[1]) !== pid) throw new Error(`Invalid process identity for PID ${pid}.`);
        const entry = { pid, parentPid: Number(match[2]), executable: match[3] };
        ancestors.push(entry);
        const bundle = entry.executable.match(/^(.*?\.app)\/Contents\/MacOS\//)?.[1];
        if (!hostApp && bundle) hostApp = { ...entry, name: basename(bundle, ".app"), path: bundle };
        pid = entry.parentPid;
    }
    if (!ancestors.length) throw new Error("The current extension process could not be identified.");
    return { current: ancestors[0], hostApp, ancestors };
}

export function permissionCommand(command = DEFAULT_COMMAND) {
    if (!Array.isArray(command) || !command.length || command.some(value => typeof value !== "string" || !value.trim())) {
        throw new Error("The configured Peekaboo command is invalid.");
    }
    const [program, ...configuredArgs] = command;
    const args = basename(program) === "npx"
        ? ["--offline", "--yes=false", ...configuredArgs.filter(arg => !["-y", "--yes"].includes(arg))]
        : configuredArgs;
    return { program, args: [...args, "permissions", "--json", "--no-remote"] };
}

export async function checkLocalPermissions({
    command, exec = execute, pid = process.pid, platform = process.platform,
} = {}) {
    const report = {
        checkedAt: new Date().toISOString(),
        status: "unknown",
        context: null,
        probe: null,
        permissions: PERMISSIONS.map(name => ({ name, granted: null })),
        error: null,
    };
    try {
        if (platform !== "darwin") throw new Error("These permission checks require macOS.");
        report.context = await processContext(pid, exec);
        const { program, args } = permissionCommand(command);
        report.probe = { launcher: program, launcherPid: null, source: "local" };
        let stdout;
        try {
            const operation = exec(program, args, { timeout: 20_000, maxBuffer: 256_000 });
            report.probe.launcherPid = operation.child?.pid ?? null;
            ({ stdout } = await operation);
        } catch (error) {
            if (error.code === "ENOENT") throw new Error(`Cannot find ${basename(program)}. Check the Peekaboo installation and the app's PATH.`);
            if (error.killed) throw new Error("The local Peekaboo permission check timed out. Refresh to try again.");
            throw new Error("The local Peekaboo permission command failed. Check its installation and configured command; this check does not install packages.");
        }
        let result;
        try { result = JSON.parse(stdout); }
        catch { throw new Error("Peekaboo returned an invalid permission report."); }
        if (result?.success !== true || !Array.isArray(result.data?.permissions)) {
            throw new Error("Peekaboo could not read the macOS permission state.");
        }
        if (result.data.source !== "local") {
            throw new Error("Peekaboo returned permissions for another host, not this launch context.");
        }
        report.permissions = PERMISSIONS.map(name => {
            const value = result.data.permissions.find(item => item?.name === name)?.isGranted;
            return { name, granted: typeof value === "boolean" ? value : null };
        });
        if (report.permissions.some(item => item.granted === null)) {
            report.error = "Peekaboo did not report all three permission states. Refresh to try again.";
        } else {
            report.status = report.permissions.every(item => item.granted) ? "granted" : "denied";
        }
    } catch (error) {
        report.status = "error";
        report.error = error.message;
    }
    return report;
}
