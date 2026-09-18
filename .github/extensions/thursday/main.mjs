import { joinSession, createCanvas } from "@github/copilot-sdk/extension";
import { spawn, execFile } from "node:child_process";
import { promisify, stripVTControlCharacters } from "node:util";
import { fileURLToPath } from "node:url";
import { join } from "node:path";
import { configPath, readJson, writeJson, discover, discoverAccounts, selectionFor, preferredChoice } from "./catalog.mjs";
import { PromptBridge } from "./bridge.mjs";
import { startServer } from "./server.mjs";
import { checkLocalPermissions } from "./permissions.mjs";
import { ensureCurrentBinary, BUILD_COMMAND } from "./build.mjs";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const binary = join(root, "target", "release", "thursday-agent");
const execute = promisify(execFile);
const panels = new Set();
let session, bridge, server, serverPromise, descriptor, jobsFile;
let catalog = null;
let child = null;
let starting = null;
let selecting = false;
let processState = { status: "stopped", log: "" };
let persistence = Promise.resolve();
let discovery;
let checking = false;
let permissionState = null;
let permissionCheck = null;
let markReady;
const ready = new Promise(resolve => { markReady = resolve; });

function clean(text) {
    const key = process.env.OPENAI_API_KEY;
    return key ? String(text).replaceAll(key, "[redacted]") : String(text);
}

function appendLog(text) {
    processState.log = (processState.log + clean(text)).slice(-8000);
}

async function state() {
    await ready;
    const file = await readJson(configPath(), {});
    return {
        sessionId: session.sessionId, configPath: configPath(),
        selection: file.live_selection || null,
        preferred: catalog ? preferredChoice(catalog, file.live_selection) : null,
        catalog, process: { ...processState, log: stripVTControlCharacters(processState.log) }, requests: bridge.list(),
        permissions: permissionState, permissionsChecking: Boolean(permissionCheck),
    };
}

async function checkPermissions() {
    await ready;
    if (permissionCheck) return permissionCheck;
    permissionCheck = (async () => {
        let command;
        try {
            command = (await readJson(configPath(), {})).peekaboo_command;
        } catch (error) {
            permissionState = {
                checkedAt: new Date().toISOString(), status: "error", context: null,
                permissions: [], error: clean(error.message),
            };
            return permissionState;
        }
        permissionState = await checkLocalPermissions({ command });
        return permissionState;
    })();
    try { return await permissionCheck; }
    finally { permissionCheck = null; }
}

async function refresh(subscription) {
    await ready;
    if (discovery) throw new Error("Resource discovery is already running.");
    const file = await readJson(configPath(), {});
    discovery = discover(subscription || file.live_selection?.subscription || process.env.AZURE_OPENAI_SUBSCRIPTION);
    try {
        catalog = await discovery;
        return await state();
    } finally {
        discovery = null;
    }
}

async function select(choice) {
    if (child || starting || checking || selecting) throw new Error("Stop thursday-agent and wait for any startup, model save, or connection check before you change its model.");
    if (!catalog) throw new Error("Load resources before you select a model.");
    const selection = selectionFor(catalog, choice);
    selecting = true;
    try {
        const file = await readJson(configPath(), {});
        file.live_selection = selection;
        await writeJson(configPath(), file);
        return await state();
    } finally {
        selecting = false;
    }
}

async function environment() {
    const file = await readJson(configPath(), {});
    if (!file.live_selection) throw new Error("Select and save a model first.");
    const env = { ...process.env, THURSDAY_CONFIG: configPath(), THURSDAY_APP_BRIDGE: descriptor };
    // A voice worker started with Azure must not inherit an unused OpenAI key.
    if (file.live_selection.provider === "azure") delete env.OPENAI_API_KEY;
    delete env.AZURE_OPENAI_API_KEY;
    return env;
}

async function ensureBinary() {
    await ensureCurrentBinary(root, binary);
    try {
        await execute(binary, ["voice-check", "--help"], { timeout: 5000, maxBuffer: 32_000 });
    } catch {
        throw new Error(`This thursday-agent binary is out of date. Run ${BUILD_COMMAND} in this folder, then try again.`);
    }
}

async function start() {
    if (child || starting) throw new Error("thursday-agent is already starting or running from this canvas.");
    if (checking || selecting) throw new Error("Wait for the model save or connection check to finish.");
    const attempt = { cancelled: false };
    starting = attempt;
    try {
        await ensureBinary();
        if (attempt.cancelled) return state();
        await ensureServer();
        if (attempt.cancelled) return state();
        const env = await environment();
        if (attempt.cancelled) return state();
        const permissions = await checkPermissions();
        if (attempt.cancelled) return state();
        if (!panels.size) throw new Error("The canvas closed before thursday-agent could start.");
        processState = { status: "starting", log: "" };
        if (permissions.status !== "granted") {
            appendLog("Mac control permissions are not all granted. See the Mac permissions section; voice can still start.\n");
        }
        const started = spawn(binary, ["run", "--workspace", root], {
            cwd: root, env, stdio: ["ignore", "pipe", "pipe"],
        });
        child = started;
        started.stdout.setEncoding("utf8").on("data", appendLog);
        started.stderr.setEncoding("utf8").on("data", appendLog);
        started.once("error", error => {
            appendLog(`\nCannot start thursday-agent: ${error.message}`);
            processState.status = "failed";
            child = null;
        });
        started.once("exit", (code, signal) => {
            if (child === started) child = null;
            processState.status = code === 0 || signal === "SIGINT" ? "stopped" : "failed";
            appendLog(`\nthursday-agent exited (${signal || code}).\n`);
        });
        await new Promise((resolve, reject) => {
            started.once("spawn", resolve);
            started.once("error", reject);
        });
        if (!attempt.cancelled && child === started) processState.status = "running";
        return state();
    } finally {
        starting = null;
    }
}

async function stop() {
    if (starting) starting.cancelled = true;
    const owned = child;
    if (owned) {
        processState.status = "stopping";
        await new Promise(resolve => {
            // The host kills extension providers five seconds after SIGTERM.
            const timer = setTimeout(() => owned.kill("SIGKILL"), shuttingDown ? 4000 : 7000);
            owned.once("exit", () => { clearTimeout(timer); resolve(); });
            owned.kill("SIGINT");
        });
    }
    return state();
}

async function check() {
    if (checking || child || starting || selecting) throw new Error("Stop thursday-agent and wait for startup or model changes before you run a connection check.");
    checking = true;
    try {
        await ensureBinary();
        const { stdout } = await execute(binary, ["voice-check"], {
            cwd: root, env: await environment(), timeout: 65_000, maxBuffer: 256_000,
        });
        return { message: clean(stdout.trim()) };
    } catch (error) {
        throw new Error(clean(error.stderr || error.message));
    } finally {
        checking = false;
    }
}

async function ensureServer() {
    await ready;
    if (server) return server;
    if (serverPromise) return serverPromise;
    serverPromise = (async () => {
        const entry = await startServer({
            state, accounts: discoverAccounts, discover: refresh, select, start, stop, check,
            permissions: checkPermissions,
            prompt: prompt => bridge.send(prompt), request: id => bridge.get(id),
        });
        try {
            await writeJson(descriptor, { url: entry.url, token: entry.token, sessionId: session.sessionId });
        } catch (error) {
            entry.server.close();
            throw error;
        }
        server = entry;
        return entry;
    })();
    try { return await serverPromise; }
    finally { serverPromise = null; }
}

const choiceSchema = {
    type: "object", additionalProperties: false,
    properties: { provider: { enum: ["azure", "openai"] }, resourceId: { type: "string" }, model: { type: "string", minLength: 1 } },
    required: ["provider", "model"],
};
const promptSchema = {
    type: "object", additionalProperties: false,
    properties: { prompt: { type: "string", minLength: 1, maxLength: 16000 } }, required: ["prompt"],
};

session = await joinSession({
    requestedEnvironmentVariables: ["OPENAI_API_KEY"],
    tools: [{
        name: "thursday_send_prompt",
        description: "Send an agent-to-agent prompt with immediate delivery in this Copilot App session, or read a receipt with requestId. The app agent must answer promptly through thursday_reply, not through chat or UI. Sending returns a receipt, not an answer. Requests are independent and expire after three minutes. Never call this recursively to answer a thursday-agent request.",
        parameters: {
            type: "object", additionalProperties: false,
            properties: { prompt: { type: "string", minLength: 1, maxLength: 16000 }, requestId: { type: "string" } },
            oneOf: [{ required: ["prompt"] }, { required: ["requestId"] }],
        },
        handler: async args => {
            await ready;
            try {
                const result = args.requestId ? bridge.get(args.requestId) : await bridge.send(args.prompt);
                return JSON.stringify(result);
            } catch (error) {
                return { textResultForLlm: clean(error.message), resultType: "failure" };
            }
        },
    }, {
        name: "thursday_reply",
        description: "Return the answer to a pending thursday-agent request immediately. Use the exact requestId from its prompt and a concise plain-text response, including any blocker. This completes the waiting tool call without waiting for this session to become idle. Do not post the response in chat or render UI; stop after this tool succeeds.",
        parameters: {
            type: "object", additionalProperties: false,
            properties: {
                requestId: { type: "string", minLength: 1 },
                response: { type: "string", minLength: 1, maxLength: 16000 },
            },
            required: ["requestId", "response"],
        },
        handler: async args => {
            await ready;
            try {
                const result = bridge.reply(args.requestId, args.response);
                await persistence;
                return JSON.stringify(result);
            } catch (error) {
                return { textResultForLlm: clean(error.message), resultType: "failure" };
            }
        },
    }],
    canvases: [createCanvas({
        id: "thursday",
        displayName: "thursday-agent",
        description: "Select deployed Azure or OpenAI voice models, start thursday-agent, and ask the Copilot App about its sessions.",
        inputSchema: { type: "object", properties: {}, additionalProperties: false },
        actions: [
            { name: "get_state", description: "Read model selection, voice process state and prompt receipts.", handler: state },
            { name: "list_accounts", description: "List all cached Azure CLI subscriptions and available tenant names.", handler: () => discoverAccounts() },
            {
                name: "list_resources", description: "List Foundry resources and deployed models in a subscription. Azure is preferred.",
                inputSchema: { type: "object", properties: { subscription: { type: "string" } }, additionalProperties: false },
                handler: ctx => refresh(ctx.input?.subscription),
            },
            { name: "select_model", description: "Save a discovered provider and model for thursday-agent.", inputSchema: choiceSchema, handler: ctx => select(ctx.input) },
            { name: "check_connection", description: "Test a GPT-Live session without microphone or computer tools.", handler: check },
            {
                name: "check_permissions",
                description: "Identify this extension's process and host app, then check local Peekaboo permissions from the same launch context. Does not request permission or control the screen.",
                inputSchema: { type: "object", properties: {}, additionalProperties: false },
                handler: checkPermissions,
            },
            { name: "start_voice", description: "Start thursday-agent with the selected model and this app session bridge.", handler: start },
            { name: "stop_voice", description: "Stop the thursday-agent process owned by this canvas.", handler: stop },
            { name: "send_prompt", description: "Queue a prompt for the app agent and return its receipt.", inputSchema: promptSchema, handler: ctx => bridge.send(ctx.input.prompt) },
        ],
        open: async ctx => {
            const wasOpen = panels.has(ctx.instanceId);
            panels.add(ctx.instanceId);
            try {
                const entry = await ensureServer();
                await checkPermissions();
                return { title: "thursday-agent", url: entry.canvasUrl };
            } catch (error) {
                if (!wasOpen) panels.delete(ctx.instanceId);
                throw error;
            }
        },
        onClose: async ctx => {
            panels.delete(ctx.instanceId);
            if (panels.size === 0) {
                await stop();
                if (panels.size === 0) {
                    server?.server.close();
                    server = null;
                }
            }
        },
    })],
});

if (!session.workspacePath) throw new Error("thursday-agent requires a session workspace for its private bridge descriptor.");
descriptor = join(session.workspacePath, "files", "thursday-bridge.json");
jobsFile = join(session.workspacePath, "files", "thursday-requests.json");
bridge = new PromptBridge(session, await readJson(jobsFile, []), jobs => {
    const snapshot = structuredClone(jobs);
    persistence = persistence.then(() => writeJson(jobsFile, snapshot))
        .catch(error => session.log(`thursday-agent request persistence failed: ${clean(error.message)}`, { level: "error" }));
});
await bridge.recover();
await writeJson(jobsFile, bridge.list());
markReady();

let shuttingDown = false;
async function shutdown() {
    if (shuttingDown) return;
    shuttingDown = true;
    bridge.unsubscribe();
    await stop();
    server?.server.close();
    await persistence;
    process.exit(0);
}
process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);
